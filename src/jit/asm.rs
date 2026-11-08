//! A small AArch64 assembler: just the instructions the translator emits.
//! Registers are numbered 0..=30; 31 is the zero register (or the stack pointer
//! where the encoding says so). 32-bit forms unless the name ends in `_x`.

pub type Reg = u32;
pub const ZR: Reg = 31;
pub const SP: Reg = 31;
pub const LR: Reg = 30;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shift {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
    Ror = 3,
}

#[derive(Clone, Copy)]
pub enum Logic {
    And = 0,
    Orr = 1,
    Eor = 2,
    Ands = 3,
}

/// Condition codes, numbered exactly as in the ARM instruction set.
pub const COND_EQ: u32 = 0;
pub const COND_NE: u32 = 1;
pub const COND_LO: u32 = 3;
pub const COND_HI: u32 = 8;
pub const COND_AL: u32 = 14;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Label(usize);

#[derive(Clone, Copy)]
enum Fixup {
    B,
    Cond(u32),
    Cb { rt: Reg },
    Tb { rt: Reg },
}

/// Loads and stores addressed by a base register plus a 64-bit index register.
#[derive(Clone, Copy)]
pub enum MemOp {
    Ldr = 0xB860_0800,
    Ldrh = 0x7860_0800,
    Ldrb = 0x3860_0800,
    Ldrsh = 0x78E0_0800,
    Ldrsb = 0x38E0_0800,
    Str = 0xB820_0800,
    Strh = 0x7820_0800,
    Strb = 0x3820_0800,
}

pub struct Asm {
    code: Vec<u32>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Label, Fixup)>,
}

impl Asm {
    pub fn new() -> Asm {
        Asm { code: Vec::new(), labels: Vec::new(), fixups: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.code.len()
    }

    pub fn emit(&mut self, word: u32) {
        self.code.push(word);
    }

    /// Overwrite a word emitted earlier.
    pub fn set(&mut self, at: usize, word: u32) {
        self.code[at] = word;
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    pub fn bind(&mut self, label: Label) {
        self.labels[label.0] = Some(self.code.len());
    }

    /// Resolve branches and hand back the finished code.
    pub fn finish(mut self) -> Vec<u32> {
        for &(at, label, kind) in &self.fixups {
            let target = self.labels[label.0].expect("unbound label");
            let delta = target as i64 - at as i64;
            self.code[at] |= match kind {
                Fixup::B => (delta as u32) & 0x03FF_FFFF,
                Fixup::Cond(cond) => (((delta as u32) & 0x7_FFFF) << 5) | cond,
                Fixup::Cb { rt, .. } => (((delta as u32) & 0x7_FFFF) << 5) | rt,
                Fixup::Tb { rt, .. } => (((delta as u32) & 0x3FFF) << 5) | rt,
            };
        }
        self.code
    }

    // --- arithmetic ---------------------------------------------------

    pub fn add_sub_imm(&mut self, sub: bool, set: bool, rd: Reg, rn: Reg, imm: u32) {
        let (imm12, sh) = if imm < 4096 { (imm, 0) } else { (imm >> 12, 1) };
        debug_assert!(imm < 4096 || (imm & 0xFFF == 0 && imm >> 12 < 4096));
        self.emit(0x1100_0000 | (sub as u32) << 30 | (set as u32) << 29 | sh << 22 | imm12 << 10 | rn << 5 | rd);
    }

    /// Whether an add/sub immediate can encode `imm`.
    pub fn fits_imm(imm: u32) -> bool {
        imm < 4096 || (imm & 0xFFF == 0 && imm >> 12 < 4096)
    }

    pub fn add_imm(&mut self, rd: Reg, rn: Reg, imm: u32, set: bool) {
        self.add_sub_imm(false, set, rd, rn, imm);
    }

    pub fn sub_imm(&mut self, rd: Reg, rn: Reg, imm: u32, set: bool) {
        self.add_sub_imm(true, set, rd, rn, imm);
    }

    pub fn addsub_reg(&mut self, sub: bool, set: bool, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x0B00_0000 | (sub as u32) << 30 | (set as u32) << 29 | rm << 16 | rn << 5 | rd);
    }

    /// ADC / SBC, reading the host carry flag.
    pub fn addsub_carry(&mut self, sub: bool, set: bool, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1A00_0000 | (sub as u32) << 30 | (set as u32) << 29 | rm << 16 | rn << 5 | rd);
    }

    pub fn logic(&mut self, op: Logic, invert_rm: bool, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x0A00_0000 | (op as u32) << 29 | (invert_rm as u32) << 21 | rm << 16 | rn << 5 | rd);
    }

    /// `rd = rn * rm`
    pub fn mul(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1B00_7C00 | rm << 16 | rn << 5 | rd);
    }

    pub fn mov_reg(&mut self, rd: Reg, rm: Reg) {
        self.logic(Logic::Orr, false, rd, ZR, rm);
    }

    pub fn mvn_reg(&mut self, rd: Reg, rm: Reg) {
        self.logic(Logic::Orr, true, rd, ZR, rm);
    }

    pub fn mov_reg_x(&mut self, rd: Reg, rm: Reg) {
        self.emit(0xAA00_03E0 | rm << 16 | rd);
    }

    /// Sets the flags from `rn` itself (N, Z; C and V cleared).
    pub fn tst(&mut self, rn: Reg) {
        self.logic(Logic::Ands, false, ZR, rn, rn);
    }

    pub fn mov_imm(&mut self, rd: Reg, imm: u32) {
        let (lo, hi) = (imm & 0xFFFF, imm >> 16);
        if hi == 0 {
            self.emit(0x5280_0000 | lo << 5 | rd);
        } else if lo == 0 {
            self.emit(0x5280_0000 | 1 << 21 | hi << 5 | rd);
        } else if hi == 0xFFFF {
            self.emit(0x1280_0000 | (!lo & 0xFFFF) << 5 | rd);
        } else {
            self.emit(0x5280_0000 | lo << 5 | rd);
            self.emit(0x7280_0000 | 1 << 21 | hi << 5 | rd);
        }
    }

    pub fn mov_imm64(&mut self, rd: Reg, imm: u64) {
        self.emit(0xD280_0000 | ((imm & 0xFFFF) as u32) << 5 | rd);
        for hw in 1..4 {
            let part = ((imm >> (16 * hw)) & 0xFFFF) as u32;
            if part != 0 {
                self.emit(0xF280_0000 | (hw as u32) << 21 | part << 5 | rd);
            }
        }
    }

    // --- shifts and bitfields ----------------------------------------

    /// Shift by a constant 1..=31 (0 is a plain move).
    pub fn shift_imm(&mut self, kind: Shift, rd: Reg, rn: Reg, n: u32) {
        debug_assert!(n < 32);
        if n == 0 {
            return self.mov_reg(rd, rn);
        }
        self.emit(match kind {
            Shift::Lsl => 0x5300_0000 | ((32 - n) & 31) << 16 | (31 - n) << 10 | rn << 5 | rd,
            Shift::Lsr => 0x5300_0000 | n << 16 | 31 << 10 | rn << 5 | rd,
            Shift::Asr => 0x1300_0000 | n << 16 | 31 << 10 | rn << 5 | rd,
            Shift::Ror => 0x1380_0000 | rn << 16 | n << 10 | rn << 5 | rd,
        });
    }

    pub fn shift_reg(&mut self, kind: Shift, rd: Reg, rn: Reg, rm: Reg) {
        self.emit(0x1AC0_2000 | (kind as u32) << 10 | rm << 16 | rn << 5 | rd);
    }

    /// `rd = rn >> 32` for the 64-bit register `rn`.
    pub fn lsr_x32(&mut self, rd: Reg, rn: Reg) {
        self.emit(0xD340_FC00 | 32 << 16 | rn << 5 | rd);
    }

    /// Unsigned bitfield extract: `rd = (rn >> lsb) & ((1 << width) - 1)`.
    pub fn ubfx(&mut self, rd: Reg, rn: Reg, lsb: u32, width: u32) {
        self.emit(0x5300_0000 | lsb << 16 | (lsb + width - 1) << 10 | rn << 5 | rd);
    }

    /// Bitfield insert: the low `width` bits of `rn` replace bits `lsb..` of `rd`.
    pub fn bfi(&mut self, rd: Reg, rn: Reg, lsb: u32, width: u32) {
        self.emit(0x3300_0000 | ((32 - lsb) & 31) << 16 | (width - 1) << 10 | rn << 5 | rd);
    }

    // --- memory -------------------------------------------------------

    pub fn ldr_w(&mut self, rt: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 4 == 0 && offset / 4 < 4096);
        self.emit(0xB940_0000 | offset / 4 << 10 | rn << 5 | rt);
    }

    /// 64-bit load / store with an immediate offset.
    pub fn ldr_x(&mut self, rt: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 8 == 0 && offset / 8 < 4096);
        self.emit(0xF940_0000 | offset / 8 << 10 | rn << 5 | rt);
    }

    pub fn str_x(&mut self, rt: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 8 == 0 && offset / 8 < 4096);
        self.emit(0xF900_0000 | offset / 8 << 10 | rn << 5 | rt);
    }

    /// `rt <-> [rn, rm]`, with `rm` scaled by the access size when `scaled`.
    pub fn mem_reg(&mut self, op: MemOp, rt: Reg, rn: Reg, rm: Reg, scaled: bool) {
        self.emit(op as u32 | rm << 16 | 0b011 << 13 | (scaled as u32) << 12 | rn << 5 | rt);
    }

    /// `add rd, rn, #imm` on the full 64-bit registers.
    pub fn add_imm_x(&mut self, rd: Reg, rn: Reg, imm: u32) {
        debug_assert!(imm < 4096);
        self.emit(0x9100_0000 | imm << 10 | rn << 5 | rd);
    }

    pub fn str_w(&mut self, rt: Reg, rn: Reg, offset: u32) {
        debug_assert!(offset % 4 == 0 && offset / 4 < 4096);
        self.emit(0xB900_0000 | offset / 4 << 10 | rn << 5 | rt);
    }

    /// `stp rt, rt2, [sp, #-bytes]!`
    pub fn stp_push(&mut self, rt: Reg, rt2: Reg, bytes: u32) {
        let imm7 = (-(bytes as i32) / 8) as u32 & 0x7F;
        self.emit(0xA980_0000 | imm7 << 15 | rt2 << 10 | SP << 5 | rt);
    }

    /// `ldp rt, rt2, [sp], #bytes`
    pub fn ldp_pop(&mut self, rt: Reg, rt2: Reg, bytes: u32) {
        self.emit(0xA8C0_0000 | (bytes / 8) << 15 | rt2 << 10 | SP << 5 | rt);
    }

    /// `stp rt, rt2, [sp, #offset]`
    pub fn stp(&mut self, rt: Reg, rt2: Reg, offset: u32) {
        self.emit(0xA900_0000 | (offset / 8) << 15 | rt2 << 10 | SP << 5 | rt);
    }

    /// `ldp rt, rt2, [sp, #offset]`
    pub fn ldp(&mut self, rt: Reg, rt2: Reg, offset: u32) {
        self.emit(0xA940_0000 | (offset / 8) << 15 | rt2 << 10 | SP << 5 | rt);
    }

    pub fn mrs_nzcv(&mut self, rt: Reg) {
        self.emit(0xD53B_4200 | rt);
    }

    pub fn msr_nzcv(&mut self, rt: Reg) {
        self.emit(0xD51B_4200 | rt);
    }

    // --- control flow -------------------------------------------------

    fn branch_with(&mut self, word: u32, label: Label, kind: Fixup) {
        self.fixups.push((self.code.len(), label, kind));
        self.emit(word);
    }

    pub fn b(&mut self, label: Label) {
        self.branch_with(0x1400_0000, label, Fixup::B);
    }

    pub fn b_cond(&mut self, cond: u32, label: Label) {
        self.branch_with(0x5400_0000, label, Fixup::Cond(cond));
    }

    pub fn cbz(&mut self, rt: Reg, label: Label) {
        self.branch_with(0x3400_0000, label, Fixup::Cb { rt });
    }

    pub fn cbnz(&mut self, rt: Reg, label: Label) {
        self.branch_with(0x3500_0000, label, Fixup::Cb { rt });
    }

    /// Branch if bit `bit` (0..=63) of the 64-bit register is set.
    pub fn tbnz(&mut self, rt: Reg, bit: u32, label: Label) {
        let word = 0x3700_0000 | (bit >> 5) << 31 | (bit & 31) << 19;
        self.branch_with(word, label, Fixup::Tb { rt });
    }

    /// The word for a branch from the instruction at byte address `from` to `to`.
    pub fn branch_word(from: usize, to: usize) -> u32 {
        let delta = (to as i64 - from as i64) / 4;
        0x1400_0000 | (delta as u32 & 0x03FF_FFFF)
    }

    pub fn blr(&mut self, rn: Reg) {
        self.emit(0xD63F_0000 | rn << 5);
    }

    pub fn ret(&mut self) {
        self.emit(0xD65F_03C0);
    }
}
