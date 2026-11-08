//! ARM-state instructions: which ones are translated natively and how.

use super::super::asm::*;
use super::*;
use crate::system::ARM9;

impl<const C: usize> Translator<C> {
    pub(super) fn instruction(&mut self, instr: u32, index: u32) -> Flow {
        if instr >> 28 != 0xF {
            if instr >> 25 & 7 == 0b101 {
                let offset = (((instr & 0xFF_FFFF) << 8) as i32 >> 6) as u32;
                let target = self.pc.wrapping_add(8).wrapping_add(offset);
                let link = (instr & (1 << 24) != 0).then(|| self.pc.wrapping_add(4));
                self.branch(instr >> 28, target, link, index + 1);
                return Flow::Done;
            }
            let cond = instr >> 28;
            if instr & 0x0FFF_FFF0 == 0x012F_FF10 {
                self.exchange(cond, instr & 15, None, index + 1);
                return Flow::Done;
            }
            if C == ARM9 && instr & 0x0FFF_FFF0 == 0x012F_FF30 && instr & 15 != 15 {
                self.exchange(cond, instr & 15, Some(self.next_pc()), index + 1);
                return Flow::Done;
            }
            if self.try_native(instr, index) {
                return Flow::Continue;
            }
        }
        self.interpret(instr, index);
        if may_branch(instr) { Flow::Fall } else { Flow::Continue }
    }

    fn try_native(&mut self, instr: u32, index: u32) -> bool {
        let group = instr >> 25 & 7;
        let supported = match group {
            0b000 if instr & 0xF0 == 0x90 => multiply_supported(instr),
            0b000 if instr & 0x90 == 0x90 => halfword_supported(instr),
            0b000 if instr & 0x0FFF_0FFF == 0x010F_0000 => instr >> 12 & 15 != 15,
            0b000 | 0b001 => data_processing_supported(instr),
            0b010 | 0b011 => transfer_supported(instr),
            0b100 => block_supported(instr),
            _ => false,
        };
        if !supported {
            return false;
        }
        let cond = instr >> 28;
        let skip = (cond != COND_AL).then(|| {
            self.ensure_host();
            let skip = self.a.label();
            self.a.b_cond(cond ^ 1, skip);
            (skip, self.mem)
        });
        match group {
            0 if instr & 0xF0 == 0x90 => self.arm_multiply(instr),
            0 if instr & 0x90 == 0x90 => self.arm_halfword_transfer(instr, index),
            0 if instr & 0x0FFF_0FFF == 0x010F_0000 => self.read_cpsr(instr >> 12 & 15),
            0 | 1 => self.arm_data_processing(instr),
            4 => {
                let list = instr & 0xFFFF;
                let (pre, up) = (instr & (1 << 24) != 0, instr & (1 << 23) != 0);
                let load = instr & (1 << 20) != 0;
                self.block_transfer(load, instr >> 16 & 15, list, up, pre, instr & (1 << 21) != 0, index);
            }
            _ => self.arm_transfer(instr, index),
        }
        if let Some((skip, mem_before)) = skip {
            // Both paths must agree on where the flags are.
            self.ensure_host();
            self.a.bind(skip);
            self.mem = self.mem && mem_before;
        }
        true
    }

    fn arm_data_processing(&mut self, instr: u32) {
        let opcode = instr >> 21 & 15;
        let set = instr & (1 << 20) != 0;
        let rn = instr >> 16 & 15;
        let rd = instr >> 12 & 15;
        let logical = matches!(opcode, 0 | 1 | 8 | 9 | 12 | 13 | 14 | 15);
        if instr >> 25 & 1 == 1 {
            let rot = (instr >> 8 & 0xF) * 2;
            let value = (instr & 0xFF).rotate_right(rot);
            let carry = if rot != 0 { Carry::Const(value >> 31 != 0) } else { Carry::Unchanged };
            self.alu(opcode, set, rn, rd, Operand::Imm(value), carry);
        } else {
            let carry = self.shifted_register(instr, set && logical);
            self.alu(opcode, set, rn, rd, Operand::Reg, carry);
        }
    }

    /// The address of a single transfer: base register `rn` moved by the offset (a constant,
    /// or the value in `T_B`) before the access if `pre`, otherwise after. With `writeback`,
    /// `SAVED` ends up holding the new base. Returns the host register with the address.
    fn transfer_address(&mut self, pre: bool, up: bool, rn: u32, writeback: bool, offset_imm: Option<u32>) -> Reg {
        let base_const = (rn == 15).then(|| self.pc_read());
        if base_const.is_none() {
            self.load_reg(T_A, rn);
        }
        // dst = base +/- offset
        let moved = |t: &mut Self, dst: Reg| match (base_const, offset_imm) {
            (Some(base), Some(off)) => {
                t.a.mov_imm(dst, if up { base.wrapping_add(off) } else { base.wrapping_sub(off) })
            }
            (None, Some(off)) => t.a.add_sub_imm(!up, false, dst, T_A, off),
            (Some(base), None) => {
                t.a.mov_imm(T_A, base);
                t.a.addsub_reg(!up, false, dst, T_A, T_B);
            }
            (None, None) => t.a.addsub_reg(!up, false, dst, T_A, T_B),
        };
        let addr = if pre {
            moved(self, T_C);
            if writeback {
                self.a.mov_reg(SAVED, T_C);
            }
            T_C
        } else {
            moved(self, SAVED);
            T_A
        };
        addr
    }

    /// MUL and MLA.
    fn arm_multiply(&mut self, instr: u32) {
        let set = instr & (1 << 20) != 0;
        if set {
            self.ensure_host();
        }
        self.load_reg(T_A, instr & 15);
        self.load_reg(T_B, instr >> 8 & 15);
        self.a.mul(T_TMP, T_A, T_B);
        if instr & (1 << 21) != 0 {
            self.load_reg(T_C, instr >> 12 & 15);
            self.a.addsub_reg(false, false, T_TMP, T_TMP, T_C);
        }
        if set {
            self.logical_flags(Carry::Unchanged);
        }
        self.store_reg(T_TMP, instr >> 16 & 15);
    }

    /// LDRH, STRH, LDRSB and LDRSH.
    fn arm_halfword_transfer(&mut self, instr: u32, index: u32) {
        let pre = instr & (1 << 24) != 0;
        let up = instr & (1 << 23) != 0;
        let load = instr & (1 << 20) != 0;
        let rn = instr >> 16 & 15;
        let rd = instr >> 12 & 15;
        let writeback = !pre || instr & (1 << 21) != 0;
        self.ensure_mem();
        let offset_imm = (instr & (1 << 22) != 0).then(|| instr >> 4 & 0xF0 | instr & 0xF);
        if offset_imm.is_none() {
            self.load_reg(T_B, instr & 15);
        }
        let addr = self.transfer_address(pre, up, rn, writeback, offset_imm);
        let kind = match instr >> 5 & 3 {
            1 => Access::Half,
            2 => Access::ByteSigned,
            _ => Access::HalfSigned,
        };
        self.access(kind, load, rd, addr, writeback.then_some(rn), index);
    }

    /// MRS rd, CPSR
    fn read_cpsr(&mut self, rd: u32) {
        self.ensure_mem();
        self.a.ldr_w(T_TMP, CPU, cpsr_off());
        self.store_reg(T_TMP, rd);
    }

    fn arm_transfer(&mut self, instr: u32, index: u32) {
        let pre = instr & (1 << 24) != 0;
        let up = instr & (1 << 23) != 0;
        let byte = instr & (1 << 22) != 0;
        let load = instr & (1 << 20) != 0;
        let rn = instr >> 16 & 15;
        let rd = instr >> 12 & 15;
        let writeback = !pre || instr & (1 << 21) != 0;

        self.ensure_mem();
        let offset_imm = (instr >> 25 & 1 == 0).then_some(instr & 0xFFF);
        if offset_imm.is_none() {
            self.shifted_register(instr, false);
        }
        let addr = self.transfer_address(pre, up, rn, writeback, offset_imm);
        let kind = if byte { Access::Byte } else { Access::Word };
        self.access(kind, load, rd, addr, writeback.then_some(rn), index);
    }
}

/// Whether the interpreter fallback should end the block: the instruction usually moves the PC.
fn may_branch(instr: u32) -> bool {
    let cond = instr >> 28;
    let rd = instr >> 12 & 15;
    match instr >> 25 & 7 {
        0b000 | 0b001 => {
            // Data processing into the PC, or a branch-and-exchange.
            (rd == 15 && instr & (1 << 4 | 1 << 7) != (1 << 4 | 1 << 7)) || instr & 0x0FFF_FFD0 == 0x012F_FF10
        }
        0b010 | 0b011 => rd == 15 && instr & (1 << 20) != 0,
        0b100 => instr & (1 << 20) != 0 && instr & (1 << 15) != 0,
        0b111 => true,
        _ => cond == 0xF,
    }
}

fn data_processing_supported(instr: u32) -> bool {
    let opcode = instr >> 21 & 15;
    let set = instr & (1 << 20) != 0;
    let rd = instr >> 12 & 15;
    let test_only = (8..=11).contains(&opcode);
    if !set && test_only {
        return false; // MSR, MRS, BX, CLZ and friends
    }
    if rd == 15 && !test_only {
        return false;
    }
    if instr >> 25 & 1 == 1 {
        return true;
    }
    // Register operand: only shifts by a constant, and not rotate-right-with-extend.
    instr & (1 << 4) == 0 && !(instr >> 5 & 3 == 3 && instr >> 7 & 31 == 0)
}

fn multiply_supported(instr: u32) -> bool {
    // MUL / MLA, with no long forms and no R15 anywhere.
    instr & 0x0FC0_0000 == 0 && [16, 12, 8, 0].iter().all(|&shift| instr >> shift & 15 != 15)
}

fn halfword_supported(instr: u32) -> bool {
    let load = instr & (1 << 20) != 0;
    let kind = instr >> 5 & 3;
    let rn = instr >> 16 & 15;
    let rd = instr >> 12 & 15;
    let writeback = instr & (1 << 24) == 0 || instr & (1 << 21) != 0;
    // Not SWP or multiplies (kind 0), nor the double-word forms, which are stores of kind 2 and 3.
    if kind == 0 || (!load && kind != 1) || rd == 15 || (rn == 15 && writeback) {
        return false;
    }
    instr & (1 << 22) != 0 || instr & 15 != 15
}

fn block_supported(instr: u32) -> bool {
    let load = instr & (1 << 20) != 0;
    let list = instr & 0xFFFF;
    // No user-bank or CPSR-restoring forms, no empty list, and no loads of the PC.
    instr & (1 << 22) == 0 && list != 0 && instr >> 16 & 15 != 15 && !(load && list & 0x8000 != 0)
}

fn transfer_supported(instr: u32) -> bool {
    let rn = instr >> 16 & 15;
    let rd = instr >> 12 & 15;
    let writeback = instr & (1 << 24) == 0 || instr & (1 << 21) != 0;
    if rd == 15 || (rn == 15 && writeback) {
        return false;
    }
    if instr >> 25 & 1 == 0 {
        return true;
    }
    instr & (1 << 4) == 0 && !(instr >> 5 & 3 == 3 && instr >> 7 & 31 == 0)
}

