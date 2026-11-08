//! Loads and stores: calling the access helpers and leaving the block when they ask.

use super::super::asm::*;
use super::helpers::*;
use super::*;
use crate::system::{ARM9, CODE_PAGE_SHIFT, FastPtrs};

// The inline accesses address these fields with scaled immediate offsets.
const _: () = {
    assert!(offset_of!(Hardware, dtcm_lo) < 16000 && offset_of!(Hardware, dtcm_len) < 16000);
    assert!(offset_of!(Hardware, effects) < 32000 && offset_of!(Hardware, fast) < 32000);
};

impl<const C: usize> Translator<C> {
    /// Load or store through a helper. The address is in host register `addr`; a store
    /// reads its value from guest register `rd`. With `writeback`, `SAVED` holds the
    /// base register's new value, which goes to that guest register.
    pub(super) fn access(&mut self, kind: Access, load: bool, rd: u32, addr: Reg, writeback: Option<u32>, index: u32) {
        if !load {
            self.load_reg(2, rd);
        }
        self.memory_access(kind, load, addr);

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

    /// The access itself, with the address in host register `addr` and, for a store, the value
    /// in `w2`. Main RAM is read and written right here; anything else (TCM, IO, an unaligned
    /// word, a store to a page holding compiled code) calls the helper. Either way the result
    /// is where the helpers leave it: a load's value in `x0` (bit 32 set to leave the block),
    /// a store's flag in `w0`. Clobbers `x0`, `x1`, `x16`, `x17` and the host flags.
    pub(super) fn memory_access(&mut self, kind: Access, load: bool, addr: Reg) {
        let slow = self.a.label();
        let done = self.a.label();
        // Main RAM is the 0x02 region, minus the part of it DTCM covers.
        self.a.shift_imm(Shift::Lsr, 16, addr, 24);
        self.a.sub_imm(ZR, 16, 2, true);
        self.a.b_cond(COND_NE, slow);
        if C == ARM9 {
            self.hw_word(16, offset_of!(Hardware, dtcm_lo));
            self.a.addsub_reg(true, false, 16, addr, 16);
            self.hw_word(17, offset_of!(Hardware, dtcm_len));
            self.a.addsub_reg(true, true, ZR, 16, 17);
            self.a.b_cond(COND_LO, slow);
        }
        self.hw_pointer(17, offset_of!(Hardware, fast) + offset_of!(FastPtrs, main_ram));
        let (width, op, scaled): (u32, MemOp, bool) = match (load, kind) {
            (true, Access::Word) => (4, MemOp::Ldr, true),
            (true, Access::Half) => (2, MemOp::Ldrh, true),
            (true, Access::HalfSigned) => (2, MemOp::Ldrsh, true),
            (true, Access::Byte) => (1, MemOp::Ldrb, false),
            (true, Access::ByteSigned) => (1, MemOp::Ldrsb, false),
            (false, Access::Word) => (4, MemOp::Str, true),
            (false, Access::Half | Access::HalfSigned) => (2, MemOp::Strh, true),
            (false, Access::Byte | Access::ByteSigned) => (1, MemOp::Strb, false),
        };
        if load {
            if width == 4 {
                // An unaligned word load rotates; leave that to the helper.
                self.a.tbnz(addr, 0, slow);
                self.a.tbnz(addr, 1, slow);
            }
            let shift = width.trailing_zeros();
            self.a.ubfx(16, addr, shift, 22 - shift);
            self.a.mem_reg(op, 0, 17, 16, scaled);
        } else {
            // Not into a page compiled code came from: the helper invalidates it.
            self.a.ubfx(16, addr, CODE_PAGE_SHIFT as u32, 22 - CODE_PAGE_SHIFT as u32);
            self.hw_pointer(1, offset_of!(Hardware, fast) + offset_of!(FastPtrs, code_map));
            self.a.mem_reg(MemOp::Ldrb, 1, 1, 16, false);
            self.a.cbnz(1, slow);
            // Every store counts as an effect (see `Hardware::write_c`).
            let effects = offset_of!(Hardware, effects) as u32;
            self.a.ldr_x(1, HW, effects);
            self.a.add_imm_x(1, 1, 1);
            self.a.str_x(1, HW, effects);
            let shift = width.trailing_zeros();
            self.a.ubfx(16, addr, shift, 22 - shift);
            self.a.mem_reg(op, 2, 17, 16, scaled);
            self.a.mov_imm(0, 0);
        }
        self.a.b(done);

        self.a.bind(slow);
        self.a.mov_reg_x(0, HW);
        self.a.mov_reg(1, addr);
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
        self.a.bind(done);
        // The fast path compared things.
        self.host = false;
    }

    /// Load a 32-bit field of the `Hardware` into the host register.
    fn hw_word(&mut self, rt: Reg, offset: usize) {
        self.a.ldr_w(rt, HW, offset as u32);
    }

    fn hw_pointer(&mut self, rt: Reg, offset: usize) {
        self.a.ldr_x(rt, HW, offset as u32);
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

    /// LDM and STM (also PUSH, POP and LDMIA/STMIA in Thumb) of the registers in `list`,
    /// which is not empty, has no PC to load, and does not name the base register as R15.
    /// The accesses all happen before the block is allowed to leave.
    pub(super) fn block_transfer(&mut self, load: bool, rn: u32, list: u32, up: bool, pre: bool, writeback: bool, index: u32) {
        self.ensure_mem();
        let bytes = list.count_ones() * 4;
        self.load_reg(T_A, rn);
        let (down, offset) = match (up, pre) {
            (true, false) => (false, 0),
            (true, true) => (false, 4),
            (false, false) => (true, bytes - 4),
            (false, true) => (true, bytes),
        };
        self.a.add_sub_imm(down, false, SAVED, T_A, offset);
        // Addresses are word aligned; a loaded base register wins over writing it back.
        self.a.shift_imm(Shift::Lsr, SAVED, SAVED, 2);
        self.a.shift_imm(Shift::Lsl, SAVED, SAVED, 2);
        let write_back = |t: &mut Self| {
            t.load_reg(T_A, rn);
            t.a.add_sub_imm(!up, false, T_B, T_A, bytes);
            t.store_reg(T_B, rn);
        };
        if load && writeback {
            write_back(self);
        }
        self.a.mov_imm(FLAGS, 0);
        for (k, r) in (0..16).filter(|r| list & (1 << r) != 0).enumerate() {
            self.a.add_imm(T_A, SAVED, 4 * k as u32, false);
            if load {
                self.memory_access(Access::Word, true, T_A);
                self.store_reg(0, r);
                self.a.lsr_x32(0, 0);
            } else {
                if r == 15 {
                    self.a.mov_imm(2, self.pc.wrapping_add(12));
                } else {
                    self.load_reg(2, r);
                }
                self.memory_access(Access::Word, false, T_A);
            }
            self.a.logic(Logic::Orr, false, FLAGS, FLAGS, 0);
        }
        if !load && writeback {
            write_back(self);
        }
        let leave = self.exit_to(index + 1, Exit::SetPc(self.next_pc()));
        self.a.cbnz(FLAGS, leave);
    }
}
