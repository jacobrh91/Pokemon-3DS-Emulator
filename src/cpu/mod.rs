//! ARM core shared by the DS's two CPUs: the ARM946E-S (ARMv5TE, `Arch::V5TE`)
//! and the ARM7TDMI (ARMv4T, `Arch::V4T`). Timing is one cycle per instruction
//! for now; memory wait states belong to the bus owner.

pub mod bus;
mod alu;
mod arm;
mod thumb;
#[cfg(test)]
mod tests;

use crate::cpu::bus::Bus;

pub const FLAG_N: u32 = 1 << 31;
pub const FLAG_Z: u32 = 1 << 30;
pub const FLAG_C: u32 = 1 << 29;
pub const FLAG_V: u32 = 1 << 28;
pub const FLAG_Q: u32 = 1 << 27;
pub const FLAG_I: u32 = 1 << 7;
pub const FLAG_F: u32 = 1 << 6;
pub const FLAG_T: u32 = 1 << 5;

/// For each condition code, one bit per value of the flags (N, Z, C, V as a 4-bit number)
/// saying whether the condition passes: a lookup instead of a branchy comparison.
const COND_PASS: [u16; 16] = {
    let mut table = [0u16; 16];
    let mut cond = 0;
    while cond < 16 {
        let mut flags = 0;
        while flags < 16 {
            let (n, z, c, v) = (flags & 8 != 0, flags & 4 != 0, flags & 2 != 0, flags & 1 != 0);
            let pass = match cond {
                0x0 => z,
                0x1 => !z,
                0x2 => c,
                0x3 => !c,
                0x4 => n,
                0x5 => !n,
                0x6 => v,
                0x7 => !v,
                0x8 => c && !z,
                0x9 => !c || z,
                0xA => n == v,
                0xB => n != v,
                0xC => !z && n == v,
                0xD => z || n != v,
                _ => true,
            };
            if pass {
                table[cond] |= 1 << flags;
            }
            flags += 1;
        }
        cond += 1;
    }
    table
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Arch {
    V4T,
    V5TE,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exception {
    Undefined,
    SoftwareInterrupt,
    PrefetchAbort,
    DataAbort,
    Irq,
    Fiq,
}

impl Exception {
    fn mode_bits(self) -> u32 {
        match self {
            Exception::Undefined => 0x1B,
            Exception::SoftwareInterrupt => 0x13,
            Exception::PrefetchAbort | Exception::DataAbort => 0x17,
            Exception::Irq => 0x12,
            Exception::Fiq => 0x11,
        }
    }

    fn vector(self) -> u32 {
        match self {
            Exception::Undefined => 0x04,
            Exception::SoftwareInterrupt => 0x08,
            Exception::PrefetchAbort => 0x0C,
            Exception::DataAbort => 0x10,
            Exception::Irq => 0x18,
            Exception::Fiq => 0x1C,
        }
    }
}

/// The CP15 state the ARM9 firmware and games touch. Caches and the MPU
/// are not modelled; their registers are accepted and ignored.
#[derive(Clone, Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cp15 {
    pub control: u32,
    pub dtcm: u32,
    pub itcm: u32,
}

impl Cp15 {
    const ID: u32 = 0x4105_9461;
    const CACHE_TYPE: u32 = 0x0F0D_2112;
    const TCM_SIZE: u32 = 0x0014_0180;
    const HIGH_VECTORS: u32 = 1 << 13;
}

/// Index into the banked register arrays: user/system, FIQ, IRQ, SVC, abort, undefined.
fn bank_of(mode_bits: u32) -> usize {
    match mode_bits & 0x1F {
        0x11 => 1,
        0x12 => 2,
        0x13 => 3,
        0x17 => 4,
        0x1B => 5,
        _ => 0,
    }
}

#[derive(Clone)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cpu {
    /// `regs[15]` is the address of the next instruction between steps, and
    /// the architectural PC (+8 ARM / +4 Thumb) while an instruction executes.
    pub regs: [u32; 16],
    pub cpsr: u32,
    pub arch: Arch,
    pub cp15: Cp15,
    /// Set by the ARM9 wait-for-interrupt CP15 write; cleared when `irq_line` goes high.
    pub halted: bool,
    /// Set when a CP15 write may have moved the TCM windows; the system clears it.
    #[serde(skip)]
    pub cp15_dirty: bool,
    /// How many times each exception has been taken (undefined, SWI, prefetch
    /// abort, data abort, IRQ, FIQ); a debugging aid.
    pub exception_counts: [u32; 6],
    /// Level of the interrupt request input, driven by the interrupt controller.
    pub irq_line: bool,
    /// Level that wakes a halted CPU even when IRQs are masked (IE & IF != 0).
    pub wake_line: bool,
    spsr: [u32; 6],
    banked_sp: [u32; 6],
    banked_lr: [u32; 6],
    fiq_hi: [u32; 5],
    usr_hi: [u32; 5],
    branched: bool,
}

impl Cpu {
    pub fn new(arch: Arch) -> Cpu {
        Cpu {
            regs: [0; 16],
            cpsr: 0x13 | FLAG_I | FLAG_F,
            arch,
            cp15: Cp15 { control: 0x0001_2078, dtcm: 0, itcm: 0 },
            halted: false,
            cp15_dirty: true,
            exception_counts: [0; 6],
            irq_line: false,
            wake_line: false,
            spsr: [0; 6],
            banked_sp: [0; 6],
            banked_lr: [0; 6],
            fiq_hi: [0; 5],
            usr_hi: [0; 5],
            branched: false,
        }
    }

    pub fn in_thumb(&self) -> bool {
        self.cpsr & FLAG_T != 0
    }

    fn v5(&self) -> bool {
        self.arch == Arch::V5TE
    }

    /// Run one instruction (or take a pending IRQ). Returns cycles consumed.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> u32 {
        if self.wake_line || self.irq_line {
            self.halted = false;
        }
        if self.irq_line {
            if self.cpsr & FLAG_I == 0 {
                self.enter_exception(Exception::Irq, self.regs[15].wrapping_add(4));
                return 3;
            }
        }
        if self.halted {
            return 1;
        }
        self.branched = false;
        if self.in_thumb() {
            self.step_thumb(bus);
        } else {
            self.step_arm(bus);
        }
        1
    }

    #[inline(always)]
    pub(crate) fn check_cond(&self, cond: u32) -> bool {
        COND_PASS[(cond & 15) as usize] >> (self.cpsr >> 28) & 1 != 0
    }

    pub(crate) fn set_flag(&mut self, flag: u32, on: bool) {
        if on {
            self.cpsr |= flag;
        } else {
            self.cpsr &= !flag;
        }
    }

    pub(crate) fn set_nz(&mut self, value: u32) {
        self.set_flag(FLAG_N, value >> 31 != 0);
        self.set_flag(FLAG_Z, value == 0);
    }

    // --- mode banking -------------------------------------------------

    /// Replace the whole CPSR, swapping register banks if the mode changes.
    pub fn set_cpsr(&mut self, value: u32) {
        let (old, new) = (bank_of(self.cpsr), bank_of(value));
        if old != new {
            self.banked_sp[old] = self.regs[13];
            self.banked_lr[old] = self.regs[14];
            if old == 1 {
                self.fiq_hi.copy_from_slice(&self.regs[8..13]);
                self.regs[8..13].copy_from_slice(&self.usr_hi);
            }
            if new == 1 {
                self.usr_hi.copy_from_slice(&self.regs[8..13]);
                self.regs[8..13].copy_from_slice(&self.fiq_hi);
            }
            self.regs[13] = self.banked_sp[new];
            self.regs[14] = self.banked_lr[new];
        }
        self.cpsr = value;
    }

    /// The current mode's SPSR; user and system mode have none, so read CPSR.
    pub(crate) fn spsr(&self) -> u32 {
        match bank_of(self.cpsr) {
            0 => self.cpsr,
            b => self.spsr[b],
        }
    }

    pub(crate) fn set_spsr(&mut self, value: u32) {
        match bank_of(self.cpsr) {
            0 => {}
            b => self.spsr[b] = value,
        }
    }

    /// Read a register as user mode sees it (for LDM/STM with the S bit).
    pub(crate) fn user_reg(&self, r: usize) -> u32 {
        match (r, bank_of(self.cpsr)) {
            (8..=12, 1) => self.usr_hi[r - 8],
            (13, b) if b != 0 => self.banked_sp[0],
            (14, b) if b != 0 => self.banked_lr[0],
            _ => self.regs[r],
        }
    }

    pub(crate) fn set_user_reg(&mut self, r: usize, value: u32) {
        match (r, bank_of(self.cpsr)) {
            (8..=12, 1) => self.usr_hi[r - 8] = value,
            (13, b) if b != 0 => self.banked_sp[0] = value,
            (14, b) if b != 0 => self.banked_lr[0] = value,
            _ => self.regs[r] = value,
        }
    }

    // --- control flow -------------------------------------------------

    /// Jump within the current instruction set.
    pub(crate) fn branch_to(&mut self, addr: u32) {
        self.regs[15] = if self.in_thumb() { addr & !1 } else { addr & !3 };
        self.branched = true;
    }

    /// Jump, selecting ARM or Thumb from bit 0 of the target.
    pub(crate) fn branch_exchange(&mut self, addr: u32) {
        self.set_flag(FLAG_T, addr & 1 != 0);
        self.branch_to(addr);
    }

    fn vector_base(&self) -> u32 {
        if self.v5() && self.cp15.control & Cp15::HIGH_VECTORS != 0 { 0xFFFF_0000 } else { 0 }
    }

    /// Enter an exception; `lr` is the value the handler will return through.
    pub fn enter_exception(&mut self, exception: Exception, lr: u32) {
        self.exception_counts[exception as usize] += 1;
        let old = self.cpsr;
        let mut new = (old & !(0x1F | FLAG_T)) | exception.mode_bits() | FLAG_I;
        if exception == Exception::Fiq {
            new |= FLAG_F;
        }
        self.set_cpsr(new);
        self.set_spsr(old);
        self.regs[14] = lr;
        self.regs[15] = self.vector_base() + exception.vector();
        self.branched = true;
    }

    /// Leave an exception handler the way `SUBS pc, lr, #n` does: restore
    /// CPSR from SPSR and jump to `target` in the restored instruction set.
    pub fn exception_return(&mut self, target: u32) {
        let spsr = self.spsr();
        self.set_cpsr(spsr);
        self.regs[15] = if self.in_thumb() { target & !1 } else { target & !3 };
    }

    /// Jump to `addr` as a native BIOS routine would (ARMv4 style, no exchange).
    pub fn branch_hle(&mut self, addr: u32) {
        self.regs[15] = if self.in_thumb() { addr & !1 } else { addr & !3 };
    }

    /// Jump to `addr`, selecting ARM/Thumb from bit 0 (ARMv5 `LDR pc`).
    pub fn branch_exchange_hle(&mut self, addr: u32) {
        self.set_flag(FLAG_T, addr & 1 != 0);
        self.branch_hle(addr);
    }

    pub fn reset(&mut self) {
        self.set_cpsr(0x13 | FLAG_I | FLAG_F);
        self.regs[15] = self.vector_base();
        self.halted = false;
    }

    // --- CP15 ---------------------------------------------------------

    pub(crate) fn cp15_read(&self, crn: u32, crm: u32, op2: u32) -> u32 {
        match (crn, crm, op2) {
            (0, 0, 0) => Cp15::ID,
            (0, 0, 1) => Cp15::CACHE_TYPE,
            (0, 0, 2) => Cp15::TCM_SIZE,
            (1, 0, 0) => self.cp15.control,
            (9, 1, 0) => self.cp15.dtcm,
            (9, 1, 1) => self.cp15.itcm,
            _ => 0,
        }
    }

    pub(crate) fn cp15_write(&mut self, crn: u32, crm: u32, op2: u32, value: u32) {
        if crn == 1 || crn == 9 {
            self.cp15_dirty = true;
        }
        match (crn, crm, op2) {
            (1, 0, 0) => self.cp15.control = value,
            (9, 1, 0) => self.cp15.dtcm = value,
            (9, 1, 1) => self.cp15.itcm = value,
            (7, 0, 4) | (7, 8, 2) => self.halted = true,
            _ => {}
        }
    }
}
