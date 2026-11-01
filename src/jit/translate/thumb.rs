//! Thumb-state instructions.

use super::super::asm::*;
use super::*;
use crate::system::ARM9;

impl<const C: usize> Translator<C> {
    pub(super) fn thumb_instruction(&mut self, instr: u32, index: u32) -> Flow {
        let rd = instr & 7;
        let rs = instr >> 3 & 7;
        let sp = 13;
        match instr >> 13 {
            0b000 => {
                if instr >> 11 & 3 == 3 {
                    // add / subtract
                    let opcode = if instr & (1 << 9) != 0 { 2 } else { 4 };
                    if instr & (1 << 10) != 0 {
                        self.alu(opcode, true, rs, rd, Operand::Imm(instr >> 6 & 7), Carry::Unchanged);
                    } else {
                        self.load_reg(T_B, instr >> 6 & 7);
                        self.alu(opcode, true, rs, rd, Operand::Reg, Carry::Unchanged);
                    }
                } else {
                    // shift by a constant
                    let fields = rs | (instr >> 11 & 3) << 5 | (instr >> 6 & 31) << 7;
                    let carry = self.shifted_register(fields, true);
                    self.alu(13, true, 0, rd, Operand::Reg, carry);
                }
            }
            0b001 => {
                let rd = instr >> 8 & 7;
                let opcode = [13, 10, 4, 2][(instr >> 11 & 3) as usize];
                self.alu(opcode, true, rd, rd, Operand::Imm(instr & 0xFF), Carry::Unchanged);
            }
            0b010 => match instr >> 10 {
                0x10 => return self.thumb_alu(instr, index),
                0x11 => return self.thumb_hi_register(instr, index),
                0x12 | 0x13 => {
                    // PC-relative load
                    self.ensure_mem();
                    let addr = (self.pc_read() & !3).wrapping_add((instr & 0xFF) * 4);
                    self.a.mov_imm(T_C, addr);
                    self.access(Access::Word, true, instr >> 8 & 7, T_C, None, index);
                }
                _ => {
                    // load / store with a register offset
                    self.ensure_mem();
                    self.load_reg(T_A, rs);
                    self.load_reg(T_B, instr >> 6 & 7);
                    self.a.addsub_reg(false, false, T_C, T_A, T_B);
                    let (load, kind) = [
                        (false, Access::Word),
                        (false, Access::Half),
                        (false, Access::Byte),
                        (true, Access::ByteSigned),
                        (true, Access::Word),
                        (true, Access::Half),
                        (true, Access::Byte),
                        (true, Access::HalfSigned),
                    ][(instr >> 9 & 7) as usize];
                    self.access(kind, load, rd, T_C, None, index);
                }
            },
            0b011 => {
                self.ensure_mem();
                let byte = instr & (1 << 12) != 0;
                let offset = instr >> 6 & 31;
                self.address_imm(rs, if byte { offset } else { offset * 4 });
                let kind = if byte { Access::Byte } else { Access::Word };
                self.access(kind, instr & (1 << 11) != 0, rd, T_C, None, index);
            }
            0b100 => {
                self.ensure_mem();
                let load = instr & (1 << 11) != 0;
                if instr & (1 << 12) == 0 {
                    self.address_imm(rs, (instr >> 6 & 31) * 2);
                    self.access(Access::Half, load, rd, T_C, None, index);
                } else {
                    self.address_imm(sp, (instr & 0xFF) * 4);
                    self.access(Access::Word, load, instr >> 8 & 7, T_C, None, index);
                }
            }
            0b101 => {
                if instr & (1 << 12) == 0 {
                    // ADD rd, PC / SP, #imm
                    let rd = instr >> 8 & 7;
                    if instr & (1 << 11) != 0 {
                        self.load_reg(T_A, sp);
                        self.a.add_sub_imm(false, false, T_TMP, T_A, (instr & 0xFF) * 4);
                    } else {
                        self.a.mov_imm(T_TMP, (self.pc_read() & !3).wrapping_add((instr & 0xFF) * 4));
                    }
                    self.store_reg(T_TMP, rd);
                } else if instr >> 8 & 0xF == 0 {
                    // ADD SP, #imm
                    self.load_reg(T_A, sp);
                    self.a.add_sub_imm(instr & 0x80 != 0, false, T_TMP, T_A, (instr & 0x7F) * 4);
                    self.store_reg(T_TMP, sp);
                } else {
                    self.interpret(instr, index);
                    // POP with PC in the list returns.
                    return if instr >> 8 & 0xF == 0xD { Flow::Fall } else { Flow::Continue };
                }
            }
            0b110 => {
                if instr & (1 << 12) == 0 {
                    self.interpret(instr, index);
                } else {
                    let cond = instr >> 8 & 0xF;
                    if cond >= 0xE {
                        self.interpret(instr, index);
                        return Flow::Fall;
                    }
                    let offset = ((instr & 0xFF) as u8 as i8 as i32 as u32) << 1;
                    let target = self.pc_read().wrapping_add(offset);
                    self.branch(cond, target, None, index + 1);
                    return Flow::Done;
                }
            }
            _ => match instr >> 11 {
                0b11100 => {
                    let offset = (((instr & 0x7FF) << 21) as i32 >> 20) as u32;
                    let target = self.pc_read().wrapping_add(offset);
                    self.branch(COND_AL, target, None, index + 1);
                    return Flow::Done;
                }
                0b11110 => {
                    // First half of BL: LR = PC + (offset << 12)
                    let offset = (((instr & 0x7FF) << 21) as i32 >> 9) as u32;
                    self.a.mov_imm(T_TMP, self.pc_read().wrapping_add(offset));
                    self.store_reg(T_TMP, 14);
                }
                0b11111 => {
                    // Second half: jump to LR + (offset << 1), leaving the return address in LR.
                    self.ensure_mem();
                    self.load_reg(T_A, 14);
                    self.a.add_sub_imm(false, false, T_A, T_A, (instr & 0x7FF) << 1);
                    self.a.shift_imm(Shift::Lsr, T_A, T_A, 1);
                    self.a.shift_imm(Shift::Lsl, T_A, T_A, 1);
                    self.store_reg(T_A, 15);
                    self.a.mov_imm(T_TMP, self.next_pc() | 1);
                    self.store_reg(T_TMP, 14);
                    self.a.mov_imm(0, index + 1);
                    self.a.b(self.epilogue);
                    return Flow::Done;
                }
                _ => {
                    self.interpret(instr, index);
                    return Flow::Fall;
                }
            },
        }
        Flow::Continue
    }

    fn thumb_alu(&mut self, instr: u32, index: u32) -> Flow {
        let rd = instr & 7;
        let rs = instr >> 3 & 7;
        let op = instr >> 6 & 0xF;
        // Shifts by a register take their carry from the bit shifted out; leave those be.
        if matches!(op, 2 | 3 | 4 | 7) {
            self.interpret(instr, index);
            return Flow::Continue;
        }
        self.load_reg(T_B, rs);
        match op {
            0xD => {
                // MUL
                self.ensure_host();
                self.load_reg(T_A, rd);
                self.a.mul(T_TMP, T_A, T_B);
                self.logical_flags(Carry::Unchanged);
                self.store_reg(T_TMP, rd);
            }
            0x9 => self.alu(3, true, rs, rd, Operand::Imm(0), Carry::Unchanged),
            _ => {
                let opcode = match op {
                    0x0 => 0,
                    0x1 => 1,
                    0x5 => 5,
                    0x6 => 6,
                    0x8 => 8,
                    0xA => 10,
                    0xB => 11,
                    0xC => 12,
                    0xE => 14,
                    _ => 15,
                };
                self.alu(opcode, true, rd, rd, Operand::Reg, Carry::Unchanged);
            }
        }
        Flow::Continue
    }

    fn thumb_hi_register(&mut self, instr: u32, index: u32) -> Flow {
        let rd = instr & 7 | instr >> 4 & 8;
        let rs = instr >> 3 & 0xF;
        let op = instr >> 8 & 3;
        if op == 3 {
            // BX / BLX. BLX with R15 or on ARMv4 is left to the interpreter.
            let link = instr & 0x80 != 0;
            if !link {
                self.exchange(COND_AL, rs, None, index + 1);
                return Flow::Done;
            }
            if C == ARM9 && rs != 15 {
                self.exchange(COND_AL, rs, Some(self.next_pc() | 1), index + 1);
                return Flow::Done;
            }
        }
        if op == 3 || (rd == 15 && op != 1) {
            self.interpret(instr, index);
            return Flow::Fall;
        }
        self.load_reg(T_B, rs);
        match op {
            0 => {
                self.load_reg(T_A, rd);
                self.a.addsub_reg(false, false, T_TMP, T_A, T_B);
                self.store_reg(T_TMP, rd);
            }
            1 => self.alu(10, true, rd, rd, Operand::Reg, Carry::Unchanged),
            _ => self.store_reg(T_B, rd),
        }
        Flow::Continue
    }
}
