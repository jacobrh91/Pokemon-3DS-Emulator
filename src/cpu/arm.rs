use super::alu::{add_with_carry, shift, Shift};
use super::{Cpu, Exception, FLAG_C, FLAG_N, FLAG_Q, FLAG_T, FLAG_V, FLAG_Z};
use crate::cpu::bus::Bus;

fn sat_add(a: i32, b: i32) -> (i32, bool) {
    match a.checked_add(b) {
        Some(v) => (v, false),
        None => (if b < 0 { i32::MIN } else { i32::MAX }, true),
    }
}

fn sat_sub(a: i32, b: i32) -> (i32, bool) {
    match a.checked_sub(b) {
        Some(v) => (v, false),
        None => (if b < 0 { i32::MAX } else { i32::MIN }, true),
    }
}

fn half(value: u32, top: bool) -> i32 {
    if top { (value >> 16) as i16 as i32 } else { value as i16 as i32 }
}

impl Cpu {
    pub(super) fn step_arm<B: Bus>(&mut self, bus: &mut B) {
        let pc = self.regs[15];
        let instr = bus.read32(pc & !3);
        self.execute_arm(bus, instr, pc);
    }

    /// Run `instr`, fetched from `pc`, leaving `regs[15]` at the next instruction.
    /// The JIT hands whatever it doesn't translate to this.
    pub fn execute_arm<B: Bus>(&mut self, bus: &mut B, instr: u32, pc: u32) {
        self.branched = false;
        self.regs[15] = pc.wrapping_add(8);
        let cond = instr >> 28;
        if cond == 0xF {
            if self.v5() {
                self.arm_unconditional(instr, pc);
            }
        } else if self.check_cond(cond) {
            self.exec_arm(bus, instr, pc);
        }
        if !self.branched {
            self.regs[15] = pc.wrapping_add(4);
        }
    }

    fn undefined_arm(&mut self, pc: u32) {
        self.enter_exception(Exception::Undefined, pc.wrapping_add(4));
    }

    /// Register read for the shift-by-register forms, where PC reads as +12.
    fn reg_shift_read(&self, r: usize) -> u32 {
        if r == 15 { self.regs[15].wrapping_add(4) } else { self.regs[r] }
    }

    /// Instructions with condition field 0b1111 (ARMv5 only).
    fn arm_unconditional(&mut self, instr: u32, pc: u32) {
        if (instr >> 25) & 7 == 0b101 {
            let offset = (((instr & 0xFF_FFFF) << 8) as i32 >> 6) as u32;
            let h = (instr >> 24) & 1;
            let target = self.regs[15].wrapping_add(offset).wrapping_add(h * 2);
            self.regs[14] = pc.wrapping_add(4);
            self.set_flag(FLAG_T, true);
            self.branch_to(target);
        }
        // PLD and other hints are no-ops.
    }

    pub(super) fn exec_arm<B: Bus>(&mut self, bus: &mut B, instr: u32, pc: u32) {
        match (instr >> 25) & 7 {
            0b000 => self.arm_group0(bus, instr, pc),
            0b001 => {
                let opcode = (instr >> 21) & 0xF;
                if instr & (1 << 20) == 0 && (0x8..=0xB).contains(&opcode) {
                    if opcode & 1 == 1 {
                        let value = (instr & 0xFF).rotate_right(((instr >> 8) & 0xF) * 2);
                        self.msr(instr, value);
                    } else {
                        self.undefined_arm(pc);
                    }
                } else {
                    let rot = ((instr >> 8) & 0xF) * 2;
                    let value = (instr & 0xFF).rotate_right(rot);
                    let carry = if rot == 0 { self.cpsr & FLAG_C != 0 } else { value >> 31 != 0 };
                    let op1 = self.regs[((instr >> 16) & 0xF) as usize];
                    self.data_processing(instr, op1, value, carry);
                }
            }
            0b010 => self.arm_single_transfer(bus, instr, pc),
            0b011 if instr & 0x10 == 0 => self.arm_single_transfer(bus, instr, pc),
            0b100 => self.arm_block_transfer(bus, instr, pc),
            0b101 => {
                let offset = (((instr & 0xFF_FFFF) << 8) as i32 >> 6) as u32;
                if instr & (1 << 24) != 0 {
                    self.regs[14] = pc.wrapping_add(4);
                }
                self.branch_to(self.regs[15].wrapping_add(offset));
            }
            0b111 if instr & (1 << 24) != 0 => {
                self.enter_exception(Exception::SoftwareInterrupt, pc.wrapping_add(4));
            }
            0b111 if instr & 0x10 != 0 && (instr >> 8) & 0xF == 15 && self.v5() => {
                self.arm_cp15(instr)
            }
            _ => self.undefined_arm(pc),
        }
    }

    fn arm_cp15(&mut self, instr: u32) {
        let (crn, crm, op2) = ((instr >> 16) & 0xF, instr & 0xF, (instr >> 5) & 7);
        let rd = ((instr >> 12) & 0xF) as usize;
        if instr & (1 << 20) != 0 {
            let value = self.cp15_read(crn, crm, op2);
            if rd == 15 {
                self.cpsr = (self.cpsr & 0x0FFF_FFFF) | (value & 0xF000_0000);
            } else {
                self.regs[rd] = value;
            }
        } else {
            self.cp15_write(crn, crm, op2, self.regs[rd]);
        }
    }

    fn arm_group0<B: Bus>(&mut self, bus: &mut B, instr: u32, pc: u32) {
        let bit7 = instr & 0x80 != 0;
        let bit4 = instr & 0x10 != 0;
        if bit7 && bit4 {
            if instr & 0x60 == 0 {
                match (instr >> 23) & 3 {
                    0 => self.arm_multiply(instr),
                    1 => self.arm_multiply_long(instr),
                    2 => self.arm_swap(bus, instr),
                    _ => self.undefined_arm(pc),
                }
            } else {
                self.arm_halfword_transfer(bus, instr, pc);
            }
        } else if (instr >> 23) & 3 == 2 && instr & (1 << 20) == 0 {
            self.arm_misc(instr, pc);
        } else {
            let rm = (instr & 0xF) as usize;
            let rn = ((instr >> 16) & 0xF) as usize;
            let kind = Shift::from_bits(instr >> 5);
            let carry_in = self.cpsr & FLAG_C != 0;
            let (op1, value, carry) = if bit4 {
                let amount = self.reg_shift_read(((instr >> 8) & 0xF) as usize) & 0xFF;
                let (v, c) = shift(kind, self.reg_shift_read(rm), amount, carry_in, false);
                (self.reg_shift_read(rn), v, c)
            } else {
                let (v, c) = shift(kind, self.regs[rm], (instr >> 7) & 0x1F, carry_in, true);
                (self.regs[rn], v, c)
            };
            self.data_processing(instr, op1, value, carry);
        }
    }

    fn data_processing(&mut self, instr: u32, a: u32, b: u32, shifter_carry: bool) {
        let opcode = (instr >> 21) & 0xF;
        let set_flags = instr & (1 << 20) != 0;
        let rd = ((instr >> 12) & 0xF) as usize;
        let carry_in = self.cpsr & FLAG_C != 0;

        // (result, carry out, overflow if the op defines one)
        let (result, carry, overflow) = match opcode {
            0x0 | 0x8 => (a & b, shifter_carry, None),
            0x1 | 0x9 => (a ^ b, shifter_carry, None),
            0x2 | 0xA => arith(add_with_carry(a, !b, true)),
            0x3 => arith(add_with_carry(b, !a, true)),
            0x4 | 0xB => arith(add_with_carry(a, b, false)),
            0x5 => arith(add_with_carry(a, b, carry_in)),
            0x6 => arith(add_with_carry(a, !b, carry_in)),
            0x7 => arith(add_with_carry(b, !a, carry_in)),
            0xC => (a | b, shifter_carry, None),
            0xD => (b, shifter_carry, None),
            0xE => (a & !b, shifter_carry, None),
            _ => (!b, shifter_carry, None),
        };
        let test_only = (0x8..=0xB).contains(&opcode);

        if set_flags {
            if rd == 15 && !test_only {
                let spsr = self.spsr();
                self.set_cpsr(spsr);
            } else {
                // N, Z, C and V in one update; logical operations leave V alone.
                let v = overflow.unwrap_or(self.cpsr & FLAG_V != 0);
                self.cpsr = (self.cpsr & 0x0FFF_FFFF)
                    | (result & FLAG_N)
                    | ((result == 0) as u32) << 30
                    | (carry as u32) << 29
                    | (v as u32) << 28;
            }
        }
        if !test_only {
            if rd == 15 {
                self.branch_to(result);
            } else {
                self.regs[rd] = result;
            }
        }
    }

    fn msr(&mut self, instr: u32, value: u32) {
        let mut mask = 0;
        for field in 0..4 {
            if instr & (1 << (16 + field)) != 0 {
                mask |= 0xFF << (field * 8);
            }
        }
        if instr & (1 << 22) != 0 {
            let old = self.spsr();
            self.set_spsr((old & !mask) | (value & mask));
        } else {
            if self.cpsr & 0x1F == 0x10 {
                mask &= 0xFF00_0000;
            }
            mask &= !FLAG_T;
            self.set_cpsr((self.cpsr & !mask) | (value & mask));
        }
    }

    fn arm_misc(&mut self, instr: u32, pc: u32) {
        let op = (instr >> 21) & 3;
        let rd = ((instr >> 12) & 0xF) as usize;
        let rm = (instr & 0xF) as usize;
        if instr & 0x80 != 0 {
            if self.v5() {
                self.arm_dsp_multiply(instr);
            } else {
                self.undefined_arm(pc);
            }
            return;
        }
        match (instr >> 4) & 0xF {
            0x0 if op & 1 == 0 => {
                self.regs[rd] = if instr & (1 << 22) != 0 { self.spsr() } else { self.cpsr };
            }
            0x0 => self.msr(instr, self.regs[rm]),
            0x1 if op == 1 => self.branch_exchange(self.regs[rm]),
            0x1 if op == 3 && self.v5() => self.regs[rd] = self.regs[rm].leading_zeros(),
            0x3 if op == 1 && self.v5() => {
                let target = self.regs[rm];
                self.regs[14] = pc.wrapping_add(4);
                self.branch_exchange(target);
            }
            0x5 if self.v5() => self.arm_saturating(instr),
            _ => self.undefined_arm(pc),
        }
    }

    fn arm_saturating(&mut self, instr: u32) {
        let rd = ((instr >> 12) & 0xF) as usize;
        let rn = self.regs[((instr >> 16) & 0xF) as usize] as i32;
        let rm = self.regs[(instr & 0xF) as usize] as i32;
        let mut saturated = false;
        let operand = if instr & (1 << 22) != 0 {
            let (doubled, s) = sat_add(rn, rn);
            saturated |= s;
            doubled
        } else {
            rn
        };
        let (result, s) = if instr & (1 << 21) == 0 { sat_add(rm, operand) } else { sat_sub(rm, operand) };
        saturated |= s;
        if saturated {
            self.cpsr |= FLAG_Q;
        }
        self.regs[rd] = result as u32;
    }

    fn arm_dsp_multiply(&mut self, instr: u32) {
        let rd = ((instr >> 16) & 0xF) as usize;
        let rn = ((instr >> 12) & 0xF) as usize;
        let rs = self.regs[((instr >> 8) & 0xF) as usize];
        let rm = self.regs[(instr & 0xF) as usize];
        let x = instr & 0x20 != 0;
        let y = instr & 0x40 != 0;
        match (instr >> 21) & 3 {
            0 => {
                let product = half(rm, x) * half(rs, y);
                let (result, overflow) = product.overflowing_add(self.regs[rn] as i32);
                if overflow {
                    self.cpsr |= FLAG_Q;
                }
                self.regs[rd] = result as u32;
            }
            1 => {
                let product = ((rm as i32 as i64 * half(rs, y) as i64) >> 16) as i32;
                if x {
                    self.regs[rd] = product as u32;
                } else {
                    let (result, overflow) = product.overflowing_add(self.regs[rn] as i32);
                    if overflow {
                        self.cpsr |= FLAG_Q;
                    }
                    self.regs[rd] = result as u32;
                }
            }
            2 => {
                let product = (half(rm, x) * half(rs, y)) as i64;
                let acc = (self.regs[rd] as u64) << 32 | self.regs[rn] as u64;
                let total = acc.wrapping_add(product as u64);
                self.regs[rn] = total as u32;
                self.regs[rd] = (total >> 32) as u32;
            }
            _ => self.regs[rd] = (half(rm, x) * half(rs, y)) as u32,
        }
    }

    fn arm_multiply(&mut self, instr: u32) {
        let rd = ((instr >> 16) & 0xF) as usize;
        let rn = ((instr >> 12) & 0xF) as usize;
        let rs = ((instr >> 8) & 0xF) as usize;
        let rm = (instr & 0xF) as usize;
        let mut result = self.regs[rm].wrapping_mul(self.regs[rs]);
        if instr & (1 << 21) != 0 {
            result = result.wrapping_add(self.regs[rn]);
        }
        self.regs[rd] = result;
        if instr & (1 << 20) != 0 {
            self.set_nz(result);
        }
    }

    fn arm_multiply_long(&mut self, instr: u32) {
        let hi = ((instr >> 16) & 0xF) as usize;
        let lo = ((instr >> 12) & 0xF) as usize;
        let a = self.regs[((instr >> 8) & 0xF) as usize];
        let b = self.regs[(instr & 0xF) as usize];
        let product = if instr & (1 << 22) != 0 {
            (a as i32 as i64 * b as i32 as i64) as u64
        } else {
            a as u64 * b as u64
        };
        let acc = if instr & (1 << 21) != 0 { (self.regs[hi] as u64) << 32 | self.regs[lo] as u64 } else { 0 };
        let total = product.wrapping_add(acc);
        self.regs[lo] = total as u32;
        self.regs[hi] = (total >> 32) as u32;
        if instr & (1 << 20) != 0 {
            self.set_flag(FLAG_N, total >> 63 != 0);
            self.set_flag(FLAG_Z, total == 0);
        }
    }

    fn arm_swap<B: Bus>(&mut self, bus: &mut B, instr: u32) {
        let addr = self.regs[((instr >> 16) & 0xF) as usize];
        let rd = ((instr >> 12) & 0xF) as usize;
        let source = self.regs[(instr & 0xF) as usize];
        if instr & (1 << 22) != 0 {
            let old = bus.read8(addr) as u32;
            bus.write8(addr, source as u8);
            self.regs[rd] = old;
        } else {
            let old = bus.read32(addr & !3).rotate_right((addr & 3) * 8);
            bus.write32(addr & !3, source);
            self.regs[rd] = old;
        }
    }

    fn arm_single_transfer<B: Bus>(&mut self, bus: &mut B, instr: u32, pc: u32) {
        let pre = instr & (1 << 24) != 0;
        let up = instr & (1 << 23) != 0;
        let byte = instr & (1 << 22) != 0;
        let load = instr & (1 << 20) != 0;
        let rn = ((instr >> 16) & 0xF) as usize;
        let rd = ((instr >> 12) & 0xF) as usize;

        let offset = if instr & (1 << 25) == 0 {
            instr & 0xFFF
        } else {
            let carry = self.cpsr & FLAG_C != 0;
            shift(Shift::from_bits(instr >> 5), self.regs[(instr & 0xF) as usize], (instr >> 7) & 0x1F, carry, true).0
        };
        let base = self.regs[rn];
        let moved = if up { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let addr = if pre { moved } else { base };
        let writeback = !pre || instr & (1 << 21) != 0;

        if load {
            let value = if byte {
                bus.read8(addr) as u32
            } else {
                bus.read32(addr & !3).rotate_right((addr & 3) * 8)
            };
            if writeback {
                self.regs[rn] = moved;
            }
            if rd == 15 {
                if self.v5() { self.branch_exchange(value) } else { self.branch_to(value) }
            } else {
                self.regs[rd] = value;
            }
        } else {
            let value = if rd == 15 { pc.wrapping_add(12) } else { self.regs[rd] };
            if byte {
                bus.write8(addr, value as u8);
            } else {
                bus.write32(addr & !3, value);
            }
            if writeback {
                self.regs[rn] = moved;
            }
        }
    }

    fn arm_halfword_transfer<B: Bus>(&mut self, bus: &mut B, instr: u32, pc: u32) {
        let pre = instr & (1 << 24) != 0;
        let up = instr & (1 << 23) != 0;
        let load = instr & (1 << 20) != 0;
        let rn = ((instr >> 16) & 0xF) as usize;
        let rd = ((instr >> 12) & 0xF) as usize;
        let kind = (instr >> 5) & 3;

        let offset = if instr & (1 << 22) != 0 {
            ((instr >> 4) & 0xF0) | (instr & 0xF)
        } else {
            self.regs[(instr & 0xF) as usize]
        };
        let base = self.regs[rn];
        let moved = if up { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let addr = if pre { moved } else { base };
        let writeback = !pre || instr & (1 << 21) != 0;

        if load {
            let value = match kind {
                1 => bus.read16(addr & !1) as u32,
                2 => bus.read8(addr) as i8 as u32,
                _ => bus.read16(addr & !1) as i16 as u32,
            };
            if writeback {
                self.regs[rn] = moved;
            }
            self.regs[rd] = value;
        } else if kind == 1 {
            let value = if rd == 15 { pc.wrapping_add(12) } else { self.regs[rd] };
            bus.write16(addr & !1, value as u16);
            if writeback {
                self.regs[rn] = moved;
            }
        } else if self.v5() && rd < 14 && rd % 2 == 0 {
            // LDRD (kind 2) / STRD (kind 3)
            if kind == 2 {
                self.regs[rd] = bus.read32(addr & !3);
                self.regs[rd + 1] = bus.read32((addr & !3).wrapping_add(4));
            } else {
                bus.write32(addr & !3, self.regs[rd]);
                bus.write32((addr & !3).wrapping_add(4), self.regs[rd + 1]);
            }
            if writeback {
                self.regs[rn] = moved;
            }
        } else {
            self.undefined_arm(pc);
        }
    }

    fn arm_block_transfer<B: Bus>(&mut self, bus: &mut B, instr: u32, pc: u32) {
        let pre = instr & (1 << 24) != 0;
        let up = instr & (1 << 23) != 0;
        let s_bit = instr & (1 << 22) != 0;
        let writeback = instr & (1 << 21) != 0;
        let load = instr & (1 << 20) != 0;
        let rn = ((instr >> 16) & 0xF) as usize;
        let list = instr & 0xFFFF;
        let bytes = list.count_ones() * 4;

        let base = self.regs[rn];
        let (start, new_base) = match (up, pre) {
            (true, false) => (base, base.wrapping_add(bytes)),
            (true, true) => (base.wrapping_add(4), base.wrapping_add(bytes)),
            (false, false) => (base.wrapping_sub(bytes).wrapping_add(4), base.wrapping_sub(bytes)),
            (false, true) => (base.wrapping_sub(bytes), base.wrapping_sub(bytes)),
        };
        let user_bank = s_bit && !(load && list & 0x8000 != 0);
        let mut addr = start;

        if load {
            // Writing back first lets a loaded base register win, as on hardware.
            if writeback {
                self.regs[rn] = new_base;
            }
            for r in (0..16).filter(|r| list & (1 << r) != 0) {
                let value = bus.read32(addr & !3);
                addr = addr.wrapping_add(4);
                if r == 15 {
                    if s_bit {
                        let spsr = self.spsr();
                        self.set_cpsr(spsr);
                        self.branch_to(value);
                    } else if self.v5() {
                        self.branch_exchange(value);
                    } else {
                        self.branch_to(value);
                    }
                } else if user_bank {
                    self.set_user_reg(r, value);
                } else {
                    self.regs[r] = value;
                }
            }
        } else {
            for r in (0..16).filter(|r| list & (1 << r) != 0) {
                let value = if r == 15 {
                    pc.wrapping_add(12)
                } else if user_bank {
                    self.user_reg(r)
                } else {
                    self.regs[r]
                };
                bus.write32(addr & !3, value);
                addr = addr.wrapping_add(4);
            }
            if writeback {
                self.regs[rn] = new_base;
            }
        }
    }
}

fn arith((result, carry, overflow): (u32, bool, bool)) -> (u32, bool, Option<bool>) {
    (result, carry, Some(overflow))
}

