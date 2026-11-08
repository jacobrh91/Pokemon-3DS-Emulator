//! Translation of one block of guest instructions (ARM or Thumb) to AArch64.
//!
//! Register map inside a block: `x19` = the `Cpu`, `x20` = the `Hardware`,
//! `x21` = a written-back base register across a call, `w8..w15` = scratch.
//! Guest registers stay in `Cpu::regs`; the guest's N, Z, C, V live in the host's
//! NZCV flags between instructions where that saves work (`host`), and in
//! `Cpu::cpsr` when they have to be visible to the interpreter (`mem`). At least
//! one of the two always holds the truth.
//!
//! Blocks can run on into one another. `x22` counts the instructions run so far since
//! the dispatcher handed over, `x23` is how many it allowed. A block ending in a
//! forward branch to a known place jumps straight into that block's second entry
//! (`Compiled::chain`), which checks the allowance first; until the target exists, the
//! jump goes to a stub that returns to the dispatcher.

mod alu;
mod arm;
mod helpers;
mod memory;
mod thumb;

use super::asm::*;
use crate::cpu::Cpu;
use crate::system::{ARM9, Hardware};
use std::mem::offset_of;

pub const MAX_BLOCK: u32 = 16;

const CPU: Reg = 19;
const HW: Reg = 20;
const SAVED: Reg = 21;
const DONE: Reg = 22;
const ALLOWED: Reg = 23;
/// Collects the "leave the block" flags of the accesses in a block transfer.
const FLAGS: Reg = 24;

const T_TMP: Reg = 8;
const T_A: Reg = 9;
const T_B: Reg = 10;
const T_C: Reg = 11;
const T_OLD: Reg = 12;
const T_NEW: Reg = 13;
const T_BIT: Reg = 14;

fn reg_off(r: u32) -> u32 {
    (offset_of!(Cpu, regs) + 4 * r as usize) as u32
}

fn cpsr_off() -> u32 {
    offset_of!(Cpu, cpsr) as u32
}

/// A jump out of a block to the block named `key`, which can be re-aimed at it.
pub struct Link {
    /// Word index of the branch instruction.
    pub site: usize,
    pub key: u32,
}

pub struct Compiled {
    pub code: Vec<u32>,
    pub len: u32,
    pub pages: Vec<u32>,
    /// Word index of the entry that skips setting up the frame.
    pub chain: usize,
    /// Word index of the stub that leaves the block, where unlinked jumps go.
    pub bail: usize,
    pub links: Vec<Link>,
}

/// What a translated instruction means for the block.
#[derive(PartialEq)]
enum Flow {
    Continue,
    /// The block ends, and every path out of it has already been emitted.
    Done,
    /// The block ends here but may carry on to the next instruction.
    Fall,
}

enum Exit {
    /// Store this as the next PC, then leave.
    SetPc(u32),
    /// The interpreter already moved the PC.
    PcSet,
}

/// What a logical operation does to the carry flag.
#[derive(Clone, Copy)]
enum Carry {
    Unchanged,
    Const(bool),
    /// A 0/1 value held in this host register.
    Reg(Reg),
}

/// The second operand of an ALU operation.
#[derive(Clone, Copy)]
enum Operand {
    Imm(u32),
    /// The value is in `T_B`.
    Reg,
}

#[derive(Clone, Copy)]
enum Access {
    Word,
    Half,
    HalfSigned,
    Byte,
    ByteSigned,
}

struct Translator<const C: usize> {
    a: Asm,
    pc: u32,
    thumb: bool,
    /// Guest NZCV is in the host flags.
    host: bool,
    /// Guest NZCV is in `Cpu::cpsr`.
    mem: bool,
    exits: Vec<(Label, u32, Exit)>,
    epilogue: Label,
    /// The label of the stub that unlinked jumps to other blocks go to.
    bail: Label,
    /// Whether the block may jump into others (not for the cut-short ones).
    chaining: bool,
    links: Vec<Link>,
}

/// Compile the block that starts at `key` (a pc, with bit 0 set for Thumb code), at most
/// `max_len` instructions long.
pub fn compile<const C: usize>(hw: &mut Hardware, hooks: &[u32], key: u32, max_len: u32) -> Option<Compiled> {
    let thumb = key & 1 != 0;
    let start = key & !1;
    let step = if thumb { 2 } else { 4 };
    let mut t = Translator::<C> {
        a: Asm::new(),
        pc: start,
        thumb,
        host: false,
        mem: true,
        exits: Vec::new(),
        epilogue: Label::default(),
        bail: Label::default(),
        chaining: max_len == MAX_BLOCK,
        links: Vec::new(),
    };
    t.epilogue = t.a.label();
    t.bail = t.a.label();
    t.a.stp_push(19, 20, 64);
    t.a.stp(SAVED, LR, 16);
    t.a.stp(DONE, ALLOWED, 32);
    t.a.stp(FLAGS, ZR, 48);
    t.a.mov_reg_x(CPU, 0);
    t.a.mov_reg_x(HW, 1);
    t.a.mov_imm(DONE, 0);
    t.a.mov_reg(ALLOWED, 2);
    // The second entry: the whole block has to fit in what is left of the allowance.
    let chain = t.a.len();
    let check = t.a.len();
    t.a.emit(0); // add T_TMP, DONE, #len, once the length is known
    t.a.addsub_reg(true, true, ZR, T_TMP, ALLOWED);
    t.a.b_cond(COND_HI, t.bail);

    let mut count = 0;
    let mut pages: Vec<u32> = Vec::new();
    let mut ended = false;
    while count < max_len {
        let pc = start.wrapping_add(step * count);
        // A hooked address is where the emulator takes over: a block ends just before it.
        if C == ARM9 && hooks.contains(&pc) {
            break;
        }
        let Some(page) = hw.code_page(C, pc) else { break };
        let page = page as u32;
        if !pages.contains(&page) {
            pages.push(page);
        }
        t.pc = pc;
        let flow = if thumb {
            let instr = hw.read(C, pc, 2);
            t.thumb_instruction(instr, count)
        } else {
            let instr = hw.read(C, pc, 4);
            t.instruction(instr, count)
        };
        count += 1;
        if flow != Flow::Continue {
            ended = flow == Flow::Done;
            break;
        }
    }
    if count == 0 {
        return None;
    }
    if !ended {
        t.finish(start.wrapping_add(step * count), count);
    }
    let bail = t.epilogue_and_exits();
    let mut patch = Asm::new();
    patch.add_imm(T_TMP, DONE, count, false);
    t.a.set(check, patch.finish()[0]);
    let links = std::mem::take(&mut t.links);
    Some(Compiled { code: t.a.finish(), len: count, pages, chain, bail, links })
}

impl<const C: usize> Translator<C> {
    /// What reading R15 gives in the instruction being translated.
    fn pc_read(&self) -> u32 {
        self.pc.wrapping_add(if self.thumb { 4 } else { 8 })
    }

    fn next_pc(&self) -> u32 {
        self.pc.wrapping_add(if self.thumb { 2 } else { 4 })
    }

    fn load_reg(&mut self, host: Reg, r: u32) {
        if r == 15 {
            self.a.mov_imm(host, self.pc_read());
        } else {
            self.a.ldr_w(host, CPU, reg_off(r));
        }
    }

    fn store_reg(&mut self, host: Reg, r: u32) {
        self.a.str_w(host, CPU, reg_off(r));
    }

    /// Make the host flags hold the guest's.
    fn ensure_host(&mut self) {
        if !self.host {
            self.a.ldr_w(T_TMP, CPU, cpsr_off());
            self.a.msr_nzcv(T_TMP);
            self.host = true;
        }
    }

    /// Make `Cpu::cpsr` hold the guest's flags. Clobbers `T_TMP` and `T_A`.
    fn ensure_mem(&mut self) {
        if !self.mem {
            self.a.mrs_nzcv(T_TMP);
            self.a.shift_imm(Shift::Lsr, T_TMP, T_TMP, 28);
            self.a.ldr_w(T_A, CPU, cpsr_off());
            self.a.bfi(T_A, T_TMP, 28, 4);
            self.a.str_w(T_A, CPU, cpsr_off());
            self.mem = true;
        }
    }

    fn call(&mut self, target: u64) {
        self.a.mov_imm64(16, target);
        self.a.blr(16);
        // The callee is free to use the flags.
        self.host = false;
    }

    fn exit_to(&mut self, count: u32, exit: Exit) -> Label {
        let label = self.a.label();
        self.exits.push((label, count, exit));
        label
    }

    /// Leave the block, having run `count` instructions, continuing at `next`.
    fn finish(&mut self, next: u32, count: u32) {
        self.ensure_mem();
        self.a.mov_imm(T_TMP, next);
        self.store_reg(T_TMP, 15);
        self.chain_to(next, count);
    }

    /// Leave having run `count` instructions, on to the block at `next` (PC is already
    /// set to it) if that is compiled and comes later in memory. Going back to an
    /// earlier address always passes through the dispatcher, so that loops waiting for
    /// something are noticed.
    fn chain_to(&mut self, next: u32, count: u32) {
        if !self.chaining || next <= self.pc {
            self.a.mov_imm(0, count);
            self.a.b(self.epilogue);
            return;
        }
        self.a.add_imm(DONE, DONE, count, false);
        self.links.push(Link { site: self.a.len(), key: next | self.thumb as u32 });
        self.a.b(self.bail);
    }

    fn epilogue_and_exits(&mut self) -> usize {
        let exits = std::mem::take(&mut self.exits);
        for (label, count, exit) in exits {
            self.a.bind(label);
            if let Exit::SetPc(next) = exit {
                self.a.mov_imm(T_TMP, next);
                self.store_reg(T_TMP, 15);
            }
            self.a.mov_imm(0, count);
            self.a.b(self.epilogue);
        }
        let bail = self.a.len();
        self.a.bind(self.bail);
        self.a.mov_imm(0, 0);
        self.a.bind(self.epilogue);
        self.a.addsub_reg(false, false, 0, 0, DONE);
        self.a.ldp(SAVED, LR, 16);
        self.a.ldp(DONE, ALLOWED, 32);
        self.a.ldp(FLAGS, ZR, 48);
        self.a.ldp_pop(19, 20, 64);
        self.a.ret();
        bail
    }

    // --- the interpreter fallback ----------------------------------------

    fn interpret(&mut self, instr: u32, index: u32) {
        self.ensure_mem();
        self.a.mov_reg_x(0, CPU);
        self.a.mov_reg_x(1, HW);
        self.a.mov_imm(2, instr);
        self.a.mov_imm(3, self.pc);
        let target = if self.thumb { helpers::interpret::<C, true> as usize } else { helpers::interpret::<C, false> as usize };
        self.call(target as u64);
        let leave = self.exit_to(index + 1, Exit::PcSet);
        self.a.cbnz(0, leave);
    }

    // --- branches -----------------------------------------------------------

    /// A direct branch to `target`, taken if `cond` holds, setting LR to `link` when it is.
    fn branch(&mut self, cond: u32, target: u32, link: Option<u32>, count: u32) {
        self.conditional_exit(cond, count, Some(target), |t| {
            if let Some(lr) = link {
                t.a.mov_imm(T_TMP, lr);
                t.store_reg(T_TMP, 14);
            }
            t.a.mov_imm(T_TMP, target);
            t.store_reg(T_TMP, 15);
        });
    }

    /// Branch to the address in guest register `rm`, switching between ARM and Thumb
    /// according to its bit 0 (BX, and BLX when `link` gives the return address).
    fn exchange(&mut self, cond: u32, rm: u32, link: Option<u32>, count: u32) {
        self.conditional_exit(cond, count, None, |t| {
            t.load_reg(T_A, rm);
            if let Some(lr) = link {
                t.a.mov_imm(T_TMP, lr);
                t.store_reg(T_TMP, 14);
            }
            // T = bit 0 of the target; the PC loses bit 0 for Thumb and bits 1:0 for ARM.
            t.a.ldr_w(T_B, CPU, cpsr_off());
            t.a.bfi(T_B, T_A, 5, 1);
            t.a.str_w(T_B, CPU, cpsr_off());
            t.a.ubfx(T_C, T_A, 0, 1);
            t.a.shift_imm(Shift::Lsl, T_C, T_C, 1);
            t.a.mov_imm(T_BIT, 3);
            t.a.addsub_reg(true, false, T_BIT, T_BIT, T_C);
            t.a.logic(Logic::And, true, T_A, T_A, T_BIT);
            t.store_reg(T_A, 15);
        });
    }

    /// End the block with `taken` run if `cond` holds, and a plain step to the next
    /// instruction if not. Either way `count` instructions were executed.
    /// `target` is where `taken` sends the PC, when that is a known address.
    fn conditional_exit(&mut self, cond: u32, count: u32, target: Option<u32>, taken: impl FnOnce(&mut Self)) {
        let leave_taken = |t: &mut Self| match target {
            Some(target) => t.chain_to(target, count),
            None => {
                t.a.mov_imm(0, count);
                t.a.b(t.epilogue);
            }
        };
        if cond == COND_AL {
            self.ensure_mem();
            taken(self);
            leave_taken(self);
            return;
        }
        self.ensure_host();
        self.ensure_mem();
        let not_taken = self.a.label();
        self.a.b_cond(cond ^ 1, not_taken);
        taken(self);
        leave_taken(self);
        self.a.bind(not_taken);
        self.a.mov_imm(T_TMP, self.next_pc());
        self.store_reg(T_TMP, 15);
        self.chain_to(self.next_pc(), count);
    }

}
