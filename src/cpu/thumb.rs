use super::alu::{add_with_carry, shift, Shift};
use super::{Cpu, Exception, FLAG_C, FLAG_T, FLAG_V};
use crate::cpu::bus::Bus;

impl Cpu {
    pub(super) fn step_thumb<B: Bus>(&mut self, bus: &mut B) {
        let pc = self.regs[15];
        let instr = bus.read16(pc & !1) as u32;
        self.execute_thumb(bus, instr, pc);
    }

    /// Run `instr`, fetched from `pc`, leaving `regs[15]` at the next instruction.
    /// The JIT hands whatever it doesn't translate to this.
    pub fn execute_thumb<B: Bus>(&mut self, bus: &mut B, instr: u32, pc: u32) {
        self.branched = false;
        self.regs[15] = pc.wrapping_add(4);
        self.exec_thumb(bus, instr, pc);
        if !self.branched {
            self.regs[15] = pc.wrapping_add(2);
        }
    }

    fn undefined_thumb(&mut self, pc: u32) {
        self.enter_exception(Exception::Undefined, pc.wrapping_add(2));
    }

    fn set_arith_flags(&mut self, (result, carry, overflow): (u32, bool, bool)) -> u32 {
        self.set_nz(result);
        self.set_flag(FLAG_C, carry);
        self.set_flag(FLAG_V, overflow);
        result
    }

    fn exec_thumb<B: Bus>(&mut self, bus: &mut B, instr: u32, pc: u32) {
        let rd = (instr & 7) as usize;
        let rs = ((instr >> 3) & 7) as usize;
        match instr >> 13 {
            0b000 => {
                if (instr >> 11) & 3 == 3 {
                    let operand = if instr & (1 << 10) != 0 { (instr >> 6) & 7 } else { self.regs[((instr >> 6) & 7) as usize] };
                    let a = self.regs[rs];
                    let sum = if instr & (1 << 9) != 0 {
                        add_with_carry(a, !operand, true)
                    } else {
                        add_with_carry(a, operand, false)
                    };
                    self.regs[rd] = self.set_arith_flags(sum);
                } else {
                    let carry = self.cpsr & FLAG_C != 0;
                    let (value, carry) =
                        shift(Shift::from_bits(instr >> 11), self.regs[rs], (instr >> 6) & 0x1F, carry, true);
                    self.set_nz(value);
                    self.set_flag(FLAG_C, carry);
                    self.regs[rd] = value;
                }
            }
            0b001 => {
                let rd = ((instr >> 8) & 7) as usize;
                let imm = instr & 0xFF;
                let a = self.regs[rd];
                match (instr >> 11) & 3 {
                    0 => {
                        self.set_nz(imm);
                        self.regs[rd] = imm;
                    }
                    1 => {
                        self.set_arith_flags(add_with_carry(a, !imm, true));
                    }
                    2 => self.regs[rd] = self.set_arith_flags(add_with_carry(a, imm, false)),
                    _ => self.regs[rd] = self.set_arith_flags(add_with_carry(a, !imm, true)),
                }
            }
            0b010 => match instr >> 10 {
                0x10 => self.thumb_alu(instr),
                0x11 => self.thumb_hi_register(instr, pc),
                0x12 | 0x13 => {
                    let rd = ((instr >> 8) & 7) as usize;
                    let addr = (self.regs[15] & !3).wrapping_add((instr & 0xFF) * 4);
                    self.regs[rd] = bus.read32(addr);
                }
                _ => self.thumb_load_store_register(bus, instr),
            },
            0b011 => {
                let byte = instr & (1 << 12) != 0;
                let offset = (instr >> 6) & 0x1F;
                let addr = self.regs[rs].wrapping_add(if byte { offset } else { offset * 4 });
                match (byte, instr & (1 << 11) != 0) {
                    (false, false) => bus.write32(addr & !3, self.regs[rd]),
                    (false, true) => self.regs[rd] = bus.read32(addr & !3).rotate_right((addr & 3) * 8),
                    (true, false) => bus.write8(addr, self.regs[rd] as u8),
                    (true, true) => self.regs[rd] = bus.read8(addr) as u32,
                }
            }
            0b100 => {
                let load = instr & (1 << 11) != 0;
                if instr & (1 << 12) == 0 {
                    let addr = self.regs[rs].wrapping_add(((instr >> 6) & 0x1F) * 2);
                    if load {
                        self.regs[rd] = bus.read16(addr & !1) as u32;
                    } else {
                        bus.write16(addr & !1, self.regs[rd] as u16);
                    }
                } else {
                    let rd = ((instr >> 8) & 7) as usize;
                    let addr = self.regs[13].wrapping_add((instr & 0xFF) * 4);
                    if load {
                        self.regs[rd] = bus.read32(addr & !3);
                    } else {
                        bus.write32(addr & !3, self.regs[rd]);
                    }
                }
            }
            0b101 => {
                if instr & (1 << 12) == 0 {
                    let rd = ((instr >> 8) & 7) as usize;
                    let base = if instr & (1 << 11) != 0 { self.regs[13] } else { self.regs[15] & !3 };
                    self.regs[rd] = base.wrapping_add((instr & 0xFF) * 4);
                } else {
                    match (instr >> 8) & 0xF {
                        0x0 => {
                            let offset = (instr & 0x7F) * 4;
                            self.regs[13] = if instr & 0x80 != 0 {
                                self.regs[13].wrapping_sub(offset)
                            } else {
                                self.regs[13].wrapping_add(offset)
                            };
                        }
                        0x4 | 0x5 => self.thumb_push(bus, instr),
                        0xC | 0xD => self.thumb_pop(bus, instr),
                        _ => self.undefined_thumb(pc),
                    }
                }
            }
            0b110 => {
                if instr & (1 << 12) == 0 {
                    self.thumb_block_transfer(bus, instr);
                } else {
                    let cond = (instr >> 8) & 0xF;
                    match cond {
                        0xF => self.enter_exception(Exception::SoftwareInterrupt, pc.wrapping_add(2)),
                        0xE => self.undefined_thumb(pc),
                        _ if self.check_cond(cond) => {
                            let offset = ((instr & 0xFF) as u8 as i8 as i32) << 1;
                            self.branch_to(self.regs[15].wrapping_add(offset as u32));
                        }
                        _ => {}
                    }
                }
            }
            _ => match instr >> 11 {
                0b11100 => {
                    let offset = (((instr & 0x7FF) << 21) as i32 >> 20) as u32;
                    self.branch_to(self.regs[15].wrapping_add(offset));
                }
                0b11110 => {
                    let offset = (((instr & 0x7FF) << 21) as i32 >> 9) as u32;
                    self.regs[14] = self.regs[15].wrapping_add(offset);
                }
                0b11111 => {
                    let target = self.regs[14].wrapping_add((instr & 0x7FF) << 1);
                    self.regs[14] = pc.wrapping_add(2) | 1;
                    self.branch_to(target);
                }
                0b11101 if self.v5() => {
                    let target = self.regs[14].wrapping_add((instr & 0x7FF) << 1) & !3;
                    self.regs[14] = pc.wrapping_add(2) | 1;
                    self.set_flag(FLAG_T, false);
                    self.branch_to(target);
                }
                _ => self.undefined_thumb(pc),
            },
        }
    }

    fn thumb_alu(&mut self, instr: u32) {
        let rd = (instr & 7) as usize;
        let a = self.regs[rd];
        let b = self.regs[((instr >> 3) & 7) as usize];
        let carry = self.cpsr & FLAG_C != 0;
        let shifted = |cpu: &mut Cpu, kind| {
            let (value, c) = shift(kind, a, b & 0xFF, carry, false);
            cpu.set_nz(value);
            cpu.set_flag(FLAG_C, c);
            value
        };
        let op = (instr >> 6) & 0xF;
        let result = match op {
            0x0 | 0x8 => a & b,
            0x1 => a ^ b,
            0x2 => shifted(self, Shift::Lsl),
            0x3 => shifted(self, Shift::Lsr),
            0x4 => shifted(self, Shift::Asr),
            0x7 => shifted(self, Shift::Ror),
            0x5 => self.set_arith_flags(add_with_carry(a, b, carry)),
            0x6 => self.set_arith_flags(add_with_carry(a, !b, carry)),
            0x9 => self.set_arith_flags(add_with_carry(0, !b, true)),
            0xA => self.set_arith_flags(add_with_carry(a, !b, true)),
            0xB => self.set_arith_flags(add_with_carry(a, b, false)),
            0xC => a | b,
            0xD => a.wrapping_mul(b),
            0xE => a & !b,
            _ => !b,
        };
        // Shifts and arithmetic already set their flags.
        if matches!(op, 0x0 | 0x1 | 0x8 | 0xC | 0xD | 0xE | 0xF) {
            self.set_nz(result);
        }
        if !matches!(op, 0x8 | 0xA | 0xB) {
            self.regs[rd] = result;
        }
    }

    fn thumb_hi_register(&mut self, instr: u32, pc: u32) {
        let rd = ((instr & 7) | ((instr >> 4) & 8)) as usize;
        let rs = ((instr >> 3) & 0xF) as usize;
        let value = self.regs[rs];
        match (instr >> 8) & 3 {
            0 => {
                let result = self.regs[rd].wrapping_add(value);
                self.write_hi(rd, result);
            }
            1 => {
                self.set_arith_flags(add_with_carry(self.regs[rd], !value, true));
            }
            2 => self.write_hi(rd, value),
            _ => {
                if instr & 0x80 != 0 {
                    if self.v5() {
                        self.regs[14] = pc.wrapping_add(2) | 1;
                    } else {
                        return self.undefined_thumb(pc);
                    }
                }
                self.branch_exchange(value);
            }
        }
    }

    fn write_hi(&mut self, rd: usize, value: u32) {
        if rd == 15 {
            self.branch_to(value);
        } else {
            self.regs[rd] = value;
        }
    }

    fn thumb_load_store_register<B: Bus>(&mut self, bus: &mut B, instr: u32) {
        let rd = (instr & 7) as usize;
        let addr = self.regs[((instr >> 3) & 7) as usize].wrapping_add(self.regs[((instr >> 6) & 7) as usize]);
        match (instr >> 9) & 7 {
            0 => bus.write32(addr & !3, self.regs[rd]),
            1 => bus.write16(addr & !1, self.regs[rd] as u16),
            2 => bus.write8(addr, self.regs[rd] as u8),
            3 => self.regs[rd] = bus.read8(addr) as i8 as u32,
            4 => self.regs[rd] = bus.read32(addr & !3).rotate_right((addr & 3) * 8),
            5 => self.regs[rd] = bus.read16(addr & !1) as u32,
            6 => self.regs[rd] = bus.read8(addr) as u32,
            _ => self.regs[rd] = bus.read16(addr & !1) as i16 as u32,
        }
    }

    fn thumb_push<B: Bus>(&mut self, bus: &mut B, instr: u32) {
        let mut list = instr & 0xFF;
        if instr & 0x100 != 0 {
            list |= 1 << 14;
        }
        let mut addr = self.regs[13].wrapping_sub(list.count_ones() * 4);
        self.regs[13] = addr;
        for r in (0..16).filter(|r| list & (1 << r) != 0) {
            bus.write32(addr & !3, self.regs[r]);
            addr = addr.wrapping_add(4);
        }
    }

    fn thumb_pop<B: Bus>(&mut self, bus: &mut B, instr: u32) {
        let mut addr = self.regs[13];
        for r in (0..8).filter(|r| instr & (1 << r) != 0) {
            self.regs[r] = bus.read32(addr & !3);
            addr = addr.wrapping_add(4);
        }
        if instr & 0x100 != 0 {
            let target = bus.read32(addr & !3);
            addr = addr.wrapping_add(4);
            if self.v5() { self.branch_exchange(target) } else { self.branch_to(target) }
        }
        self.regs[13] = addr;
    }

    fn thumb_block_transfer<B: Bus>(&mut self, bus: &mut B, instr: u32) {
        let rb = ((instr >> 8) & 7) as usize;
        let list = instr & 0xFF;
        let mut addr = self.regs[rb];
        let new_base = addr.wrapping_add(list.count_ones() * 4);
        if instr & (1 << 11) != 0 {
            self.regs[rb] = new_base;
            for r in (0..8).filter(|r| list & (1 << r) != 0) {
                self.regs[r] = bus.read32(addr & !3);
                addr = addr.wrapping_add(4);
            }
        } else {
            for r in (0..8).filter(|r| list & (1 << r) != 0) {
                bus.write32(addr & !3, self.regs[r]);
                addr = addr.wrapping_add(4);
            }
            self.regs[rb] = new_base;
        }
    }
}
