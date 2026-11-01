//! Loads and stores: calling the access helpers and leaving the block when they ask.

use super::super::asm::*;
use super::helpers::*;
use super::*;

impl<const C: usize> Translator<C> {
    /// Load or store through a helper. The address is in host register `addr`; a store
    /// reads its value from guest register `rd`. With `writeback`, `SAVED` holds the
    /// base register's new value, which goes to that guest register.
    pub(super) fn access(&mut self, kind: Access, load: bool, rd: u32, addr: Reg, writeback: Option<u32>, index: u32) {
        self.a.mov_reg_x(0, HW);
        self.a.mov_reg(1, addr);
        if !load {
            self.load_reg(2, rd);
        }
        let target = match (load, kind) {
            (true, Access::Word) => load32::<C> as usize,
            (true, Access::Half) => load16::<C> as usize,
            (true, Access::HalfSigned) => load16_signed::<C> as usize,
            (true, Access::Byte) => load8::<C> as usize,
            (true, Access::ByteSigned) => load8_signed::<C> as usize,
            (false, Access::Word) => store32::<C> as usize,
            (false, Access::Half | Access::HalfSigned) => store16::<C> as usize,
            (false, Access::Byte | Access::ByteSigned) => store8::<C> as usize,
        };
        self.call(target as u64);

        let leave = self.exit_to(index + 1, Exit::SetPc(self.next_pc()));
        if load {
            if let Some(base) = writeback.filter(|&base| base != rd) {
                self.store_reg(SAVED, base);
            }
            self.store_reg(0, rd);
            self.a.tbnz(0, 32, leave);
        } else {
            if let Some(base) = writeback {
                self.store_reg(SAVED, base);
            }
            self.a.cbnz(0, leave);
        }
    }

    /// `T_C = base + offset` where the base is guest register `rn` and the offset a constant.
    pub(super) fn address_imm(&mut self, rn: u32, offset: u32) {
        if rn == 15 {
            self.a.mov_imm(T_C, self.pc_read().wrapping_add(offset));
        } else {
            self.load_reg(T_A, rn);
            self.a.add_sub_imm(false, false, T_C, T_A, offset);
        }
    }
}
