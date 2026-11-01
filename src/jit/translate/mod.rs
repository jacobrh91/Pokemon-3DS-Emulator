//! Translation of one block of guest instructions (ARM or Thumb) to AArch64.
//!
//! Register map inside a block: `x19` = the `Cpu`, `x20` = the `Hardware`,
//! `x21` = a written-back base register across a call, `w8..w15` = scratch.
//! Guest registers stay in `Cpu::regs`; the guest's N, Z, C, V live in the host's
//! NZCV flags between instructions where that saves work (`host`), and in
//! `Cpu::cpsr` when they have to be visible to the interpreter (`mem`). At least
//! one of the two always holds the truth.

mod alu;
mod arm;
mod helpers;
mod memory;
mod thumb;

use super::asm::*;
use crate::cpu::Cpu;
use crate::system::Hardware;
use std::mem::offset_of;

pub const MAX_BLOCK: u32 = 16;

const CPU: Reg = 19;
const HW: Reg = 20;
const SAVED: Reg = 21;

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

pub struct Compiled {
    pub code: Vec<u32>,
    pub len: u32,
    pub pages: Vec<u32>,
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
}

/// Compile the block that starts at `key` (a pc, with bit 0 set for Thumb code), at most
/// `max_len` instructions long.
pub fn compile<const C: usize>(hw: &mut Hardware, key: u32, max_len: u32) -> Option<Compiled> {
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
    };
    t.epilogue = t.a.label();
    t.a.stp_push(19, 20, 32);
    t.a.stp(SAVED, LR, 16);
    t.a.mov_reg_x(CPU, 0);
    t.a.mov_reg_x(HW, 1);

    let mut count = 0;
    let mut pages: Vec<u32> = Vec::new();
    let mut ended = false;
    while count < max_len {
        let pc = start.wrapping_add(step * count);
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
    t.epilogue_and_exits();
    Some(Compiled { code: t.a.finish(), len: count, pages })
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
        self.a.mov_imm(0, count);
        self.a.b(self.epilogue);
    }

    fn epilogue_and_exits(&mut self) {
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
        self.a.bind(self.epilogue);
        self.a.ldp(SAVED, LR, 16);
        self.a.ldp_pop(19, 20, 32);
        self.a.ret();
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
        self.conditional_exit(cond, count, |t| {
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
        self.conditional_exit(cond, count, |t| {
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
    fn conditional_exit(&mut self, cond: u32, count: u32, taken: impl FnOnce(&mut Self)) {
        if cond == COND_AL {
            self.ensure_mem();
            taken(self);
            self.a.mov_imm(0, count);
            self.a.b(self.epilogue);
            return;
        }
        self.ensure_host();
        self.ensure_mem();
        let not_taken = self.a.label();
        self.a.b_cond(cond ^ 1, not_taken);
        taken(self);
        self.a.mov_imm(0, count);
        self.a.b(self.epilogue);
        self.a.bind(not_taken);
        self.a.mov_imm(T_TMP, self.next_pc());
        self.store_reg(T_TMP, 15);
        self.a.mov_imm(0, count);
        self.a.b(self.epilogue);
    }

}
