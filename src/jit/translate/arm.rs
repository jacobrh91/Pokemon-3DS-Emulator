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
            0b000 | 0b001 => data_processing_supported(instr),
            0b010 | 0b011 => transfer_supported(instr),
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
        if group < 2 {
            self.arm_data_processing(instr);
        } else {
            self.arm_transfer(instr, index);
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

