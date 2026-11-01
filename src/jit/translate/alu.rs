//! Data-processing translation: shifted operands, the ALU operations, and the flag updates.

use super::super::asm::*;
use super::*;

impl<const C: usize> Translator<C> {
    /// Put the value of a register shifted by a constant in `T_B`, reading the register
    /// and shift from `instr` as an ARM data-processing instruction lays them out
    /// (Thumb callers build the same fields). Returns the shifter's carry out if wanted.
    pub(super) fn shifted_register(&mut self, instr: u32, want_carry: bool) -> Carry {
        let rm = instr & 0xF;
        let kind = instr >> 5 & 3;
        let amount = instr >> 7 & 31;
        self.load_reg(T_B, rm);
        let (shift, n, carry_bit) = match (kind, amount) {
            (0, 0) => return Carry::Unchanged,
            (0, n) => (Some(Shift::Lsl), n, 32 - n),
            (1, 0) => (None, 0, 31),
            (1, n) => (Some(Shift::Lsr), n, n - 1),
            (2, 0) => (Some(Shift::Asr), 31, 31),
            (2, n) => (Some(Shift::Asr), n, n - 1),
            // n == 0 (rotate right with extend) is not translated.
            (_, n) => (Some(Shift::Ror), n, n - 1),
        };
        let carry = if want_carry {
            self.a.ubfx(T_C, T_B, carry_bit, 1);
            Carry::Reg(T_C)
        } else {
            Carry::Unchanged
        };
        match shift {
            Some(kind) => self.a.shift_imm(kind, T_B, T_B, n),
            None => self.a.mov_imm(T_B, 0), // LSR #32
        }
        carry
    }

    /// One data-processing operation, `rd = rn <op> operand`, optionally setting the flags.
    /// `opcode` is the ARM one (0 AND .. 15 MVN); `rn` is unused by MOV and MVN.
    pub(super) fn alu(&mut self, opcode: u32, set: bool, rn: u32, rd: u32, operand: Operand, carry: Carry) {
        let test_only = (8..=11).contains(&opcode);
        let logical = matches!(opcode, 0 | 1 | 8 | 9 | 12 | 13 | 14 | 15);
        // ADC, SBC and RSC read the carry flag, and logical flag updates keep V and maybe C.
        if matches!(opcode, 5 | 6 | 7) || (set && logical) {
            self.ensure_host();
        }
        if !matches!(opcode, 13 | 15) {
            self.load_reg(T_A, rn);
        }
        // Arithmetic with flags goes straight to the host's.
        let host_flags = set && !logical;
        let dest = if test_only && !logical { ZR } else { T_TMP };
        let imm = match operand {
            Operand::Imm(value) => Some(value),
            Operand::Reg => None,
        };
        let reg_b = |t: &mut Self| {
            if let Some(value) = imm {
                t.a.mov_imm(T_B, value);
            }
        };
        match opcode {
            0 | 8 => {
                reg_b(self);
                self.a.logic(Logic::And, false, T_TMP, T_A, T_B);
            }
            1 | 9 => {
                reg_b(self);
                self.a.logic(Logic::Eor, false, T_TMP, T_A, T_B);
            }
            12 => {
                reg_b(self);
                self.a.logic(Logic::Orr, false, T_TMP, T_A, T_B);
            }
            14 => {
                reg_b(self);
                self.a.logic(Logic::And, true, T_TMP, T_A, T_B);
            }
            13 => match imm {
                Some(value) => self.a.mov_imm(T_TMP, value),
                None => self.a.mov_reg(T_TMP, T_B),
            },
            15 => match imm {
                Some(value) => self.a.mov_imm(T_TMP, !value),
                None => self.a.mvn_reg(T_TMP, T_B),
            },
            2 | 10 | 4 | 11 => {
                let sub = matches!(opcode, 2 | 10);
                match imm {
                    Some(value) if Asm::fits_imm(value) => self.a.add_sub_imm(sub, host_flags, dest, T_A, value),
                    _ => {
                        reg_b(self);
                        self.a.addsub_reg(sub, host_flags, dest, T_A, T_B);
                    }
                }
            }
            3 => {
                reg_b(self);
                self.a.addsub_reg(true, host_flags, T_TMP, T_B, T_A);
            }
            5 => {
                reg_b(self);
                self.a.addsub_carry(false, host_flags, T_TMP, T_A, T_B);
            }
            6 => {
                reg_b(self);
                self.a.addsub_carry(true, host_flags, T_TMP, T_A, T_B);
            }
            _ => {
                reg_b(self);
                self.a.addsub_carry(true, host_flags, T_TMP, T_B, T_A);
            }
        }

        if host_flags {
            self.host = true;
            self.mem = false;
        } else if set {
            self.logical_flags(carry);
        }
        if !test_only {
            self.store_reg(T_TMP, rd);
        }
    }

    /// N and Z from the result in `T_TMP`, C from the shifter, V untouched.
    pub(super) fn logical_flags(&mut self, carry: Carry) {
        debug_assert!(self.host);
        self.a.mrs_nzcv(T_OLD);
        self.a.tst(T_TMP);
        self.a.mrs_nzcv(T_NEW);
        // `ands` cleared C and V; put V back, and C as the shifter dictates.
        self.a.ubfx(T_BIT, T_OLD, 28, 1);
        self.a.bfi(T_NEW, T_BIT, 28, 1);
        match carry {
            Carry::Unchanged => {
                self.a.ubfx(T_BIT, T_OLD, 29, 1);
                self.a.bfi(T_NEW, T_BIT, 29, 1);
            }
            Carry::Const(true) => {
                self.a.mov_imm(T_BIT, 1);
                self.a.bfi(T_NEW, T_BIT, 29, 1);
            }
            Carry::Const(false) => {}
            Carry::Reg(reg) => self.a.bfi(T_NEW, reg, 29, 1),
        }
        self.a.msr_nzcv(T_NEW);
        self.host = true;
        self.mem = false;
    }
}
