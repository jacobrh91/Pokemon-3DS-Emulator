//! How each CPU sees memory: the address maps, TCM windows, shared WRAM split, VRAM banks
//! and the fast paths the interpreter and JIT use, plus the `Bus` adapters for each CPU.

pub mod vram;

use super::*;
use vram::Region;

/// Where each memory that code can run from starts in `Hardware::code_map`.
const CODE_ITCM: usize = MAIN_RAM_SIZE >> CODE_PAGE_SHIFT;
const CODE_WRAM7: usize = CODE_ITCM + (0x8000 >> CODE_PAGE_SHIFT);
const CODE_SHARED: usize = CODE_WRAM7 + (0x1_0000 >> CODE_PAGE_SHIFT);
const CODE_PAGES: usize = CODE_SHARED + (0x8000 >> CODE_PAGE_SHIFT);

pub(in crate::system) fn new_code_map() -> Vec<u8> {
    vec![0; CODE_PAGES]
}

#[inline(always)]
fn rd(buf: &[u8], off: usize, size: u32) -> u32 {
    match size {
        1 => buf[off] as u32,
        2 => u16::from_le_bytes([buf[off], buf[off + 1]]) as u32,
        _ => u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()),
    }
}

#[inline(always)]
fn wr(buf: &mut [u8], off: usize, size: u32, value: u32) {
    match size {
        1 => buf[off] = value as u8,
        2 => buf[off..off + 2].copy_from_slice(&(value as u16).to_le_bytes()),
        _ => buf[off..off + 4].copy_from_slice(&value.to_le_bytes()),
    }
}

impl Hardware {
    /// The page of `code_map` holding `pc` as `cpu` sees it, if it is RAM the JIT can
    /// compile from (and so watch for writes): main RAM, ITCM, or either WRAM.
    pub(crate) fn code_page(&self, cpu: usize, pc: u32) -> Option<usize> {
        let page = |base: usize, offset: usize| Some(base + (offset >> CODE_PAGE_SHIFT));
        if cpu == ARM9 {
            if pc < self.itcm_limit {
                return page(CODE_ITCM, pc as usize & 0x7FFF);
            }
            if pc.wrapping_sub(self.dtcm_lo) < self.dtcm_len {
                return None;
            }
        }
        match pc >> 24 {
            0x02 => page(0, pc as usize & (MAIN_RAM_SIZE - 1)),
            0x03 if cpu == ARM7 && (pc >= 0x0380_0000 || self.wramcnt == 0) => page(CODE_WRAM7, pc as usize & 0xFFFF),
            0x03 => self.shared_wram_offset(cpu, pc).and_then(|offset| page(CODE_SHARED, offset)),
            _ => None,
        }
    }

    /// Recompute the cached TCM windows from the CP15 registers.
    pub(crate) fn refresh_tcm(&mut self) {
        self.code_flush = true;
        self.itcm_limit = if self.tcm_enable.0 { 512u32 << ((self.itcm_reg >> 1) & 0x1F) } else { 0 };
        self.dtcm_lo = self.dtcm_reg & 0xFFFF_F000;
        self.dtcm_len = if self.tcm_enable.1 { 512 << ((self.dtcm_reg >> 1) & 0x1F) } else { 0 };
    }

    pub(crate) fn sync_tcm(&mut self, cpu: &Cpu) {
        self.dtcm_reg = cpu.cp15.dtcm;
        self.itcm_reg = cpu.cp15.itcm;
        self.tcm_enable = (cpu.cp15.control & (1 << 18) != 0, cpu.cp15.control & (1 << 16) != 0);
        self.refresh_tcm();
    }

    /// Offset into the shared WRAM array for `cpu`, or None if that CPU sees
    /// nothing there under the current WRAMCNT split.
    pub(crate) fn shared_wram_offset(&self, cpu: usize, addr: u32) -> Option<usize> {
        let a = addr as usize;
        match (self.wramcnt, cpu) {
            (0, ARM9) | (3, ARM7) => Some(a & 0x7FFF),
            (1, ARM9) | (2, ARM7) => Some(0x4000 + (a & 0x3FFF)),
            (2, ARM9) | (1, ARM7) => Some(a & 0x3FFF),
            _ => None,
        }
    }

    pub(in crate::system) fn vram_offset(&self, cpu: usize, addr: u32) -> Option<usize> {
        let off = (addr & 0x00FF_FFFF) as usize;
        if cpu == ARM7 {
            return self.vram_map.lookup(Region::Arm7, off & 0x3_FFFF);
        }
        match (addr >> 21) & 7 {
            0 => self.vram_map.lookup(Region::BgA, off & 0x7_FFFF),
            1 => self.vram_map.lookup(Region::BgB, off & 0x1F_FFFF),
            2 => self.vram_map.lookup(Region::ObjA, off & 0x3_FFFF),
            3 => self.vram_map.lookup(Region::ObjB, off & 0x1F_FFFF),
            _ => vram::lcdc_offset(addr),
        }
    }

    pub fn read(&mut self, cpu: usize, addr: u32, size: u32) -> u32 {
        match (cpu, size) {
            (ARM9, 1) => self.read_c::<ARM9, 1>(addr),
            (ARM9, 2) => self.read_c::<ARM9, 2>(addr),
            (ARM9, _) => self.read_c::<ARM9, 4>(addr),
            (_, 1) => self.read_c::<ARM7, 1>(addr),
            (_, 2) => self.read_c::<ARM7, 2>(addr),
            (_, _) => self.read_c::<ARM7, 4>(addr),
        }
    }

    /// The hot path of every CPU load and fetch: TCM and main RAM are handled inline for
    /// a given CPU and access size, everything else goes to `read_other`.
    #[inline(always)]
    pub(crate) fn read_c<const CPU: usize, const SIZE: u32>(&mut self, addr: u32) -> u32 {
        if CPU == ARM9 {
            if addr < self.itcm_limit {
                return rd(&self.itcm, (addr & 0x7FFF) as usize, SIZE);
            }
            let in_dtcm = addr.wrapping_sub(self.dtcm_lo);
            if in_dtcm < self.dtcm_len {
                return rd(&self.dtcm, (in_dtcm & 0x3FFF) as usize, SIZE);
            }
        }
        match addr >> 24 {
            0x02 => return rd(&self.main_ram, addr as usize & (MAIN_RAM_SIZE - 1), SIZE),
            // The ARM7 runs from its own WRAM, and from shared WRAM when it owns all of it.
            0x03 if CPU == ARM7 && addr >= 0x0380_0000 => return rd(&self.wram7, addr as usize & 0xFFFF, SIZE),
            0x03 if CPU == ARM7 && self.wramcnt == 3 => return rd(&self.shared_wram, addr as usize & 0x7FFF, SIZE),
            _ => {}
        }
        self.read_other(CPU, addr, SIZE)
    }

    #[inline(never)]
    pub(in crate::system) fn read_other(&mut self, cpu: usize, addr: u32, size: u32) -> u32 {
        match addr >> 24 {
            0x02 => rd(&self.main_ram, addr as usize & (MAIN_RAM_SIZE - 1), size),
            0x03 => {
                if cpu == ARM7 && addr >= 0x0380_0000 {
                    rd(&self.wram7, addr as usize & 0xFFFF, size)
                } else if cpu == ARM7 && self.wramcnt == 0 {
                    rd(&self.wram7, addr as usize & 0xFFFF, size)
                } else {
                    match self.shared_wram_offset(cpu, addr) {
                        Some(off) => rd(&self.shared_wram, off, size),
                        None => 0,
                    }
                }
            }
            0x04 => {
                self.io_ops += 1;
                self.io_read(cpu, addr, size)
            }
            0x05 if cpu == ARM9 => rd(&self.palette, addr as usize & 0x7FF, size),
            0x06 => match self.vram_offset(cpu, addr) {
                Some(off) => rd(&self.vram, off, size),
                None => 0,
            },
            0x07 if cpu == ARM9 => rd(&self.oam, addr as usize & 0x7FF, size),
            0x08 | 0x09 => 0xFFFF_FFFF >> (32 - size * 8),
            _ => {
                if addr >> 16 != 0xFFFF && addr >= 0x4000 {
                    self.note(format!("ARM{} R{} {addr:#010x}", if cpu == ARM9 { 9 } else { 7 }, size * 8));
                }
                0
            }
        }
    }

    pub fn write(&mut self, cpu: usize, addr: u32, size: u32, value: u32) {
        match (cpu, size) {
            (ARM9, 1) => self.write_c::<ARM9, 1>(addr, value),
            (ARM9, 2) => self.write_c::<ARM9, 2>(addr, value),
            (ARM9, _) => self.write_c::<ARM9, 4>(addr, value),
            (_, 1) => self.write_c::<ARM7, 1>(addr, value),
            (_, 2) => self.write_c::<ARM7, 2>(addr, value),
            (_, _) => self.write_c::<ARM7, 4>(addr, value),
        }
    }

    #[inline(always)]
    pub(crate) fn write_c<const CPU: usize, const SIZE: u32>(&mut self, addr: u32, value: u32) {
        self.effects = self.effects.wrapping_add(1);
        if CPU == ARM9 {
            if addr < self.itcm_limit {
                self.note_code_write(CODE_ITCM, (addr & 0x7FFF) as usize);
                return wr(&mut self.itcm, (addr & 0x7FFF) as usize, SIZE, value);
            }
            let in_dtcm = addr.wrapping_sub(self.dtcm_lo);
            if in_dtcm < self.dtcm_len {
                return wr(&mut self.dtcm, (in_dtcm & 0x3FFF) as usize, SIZE, value);
            }
        }
        match addr >> 24 {
            0x02 => {
                let offset = addr as usize & (MAIN_RAM_SIZE - 1);
                self.note_code_write(0, offset);
                return wr(&mut self.main_ram, offset, SIZE, value);
            }
            0x03 if CPU == ARM7 && addr >= 0x0380_0000 => {
                self.note_code_write(CODE_WRAM7, addr as usize & 0xFFFF);
                return wr(&mut self.wram7, addr as usize & 0xFFFF, SIZE, value);
            }
            0x03 if CPU == ARM7 && self.wramcnt == 3 => {
                self.note_code_write(CODE_SHARED, addr as usize & 0x7FFF);
                return wr(&mut self.shared_wram, addr as usize & 0x7FFF, SIZE, value);
            }
            _ => {}
        }
        self.write_other(CPU, addr, SIZE, value)
    }

    /// A store to main RAM; if compiled code came from there it is now stale.
    #[inline(always)]
    pub(in crate::system) fn note_code_write(&mut self, base: usize, offset: usize) {
        let page = base + (offset >> CODE_PAGE_SHIFT);
        if self.code_map[page] != 0 {
            self.code_map[page] = 0;
            self.code_hits.push(page as u32);
        }
    }

    #[inline(never)]
    pub(in crate::system) fn write_other(&mut self, cpu: usize, addr: u32, size: u32, value: u32) {
        match addr >> 24 {
            0x02 => {
                let offset = addr as usize & (MAIN_RAM_SIZE - 1);
                self.note_code_write(0, offset);
                wr(&mut self.main_ram, offset, size, value)
            }
            0x03 => {
                if cpu == ARM7 && (addr >= 0x0380_0000 || self.wramcnt == 0) {
                    self.note_code_write(CODE_WRAM7, addr as usize & 0xFFFF);
                    wr(&mut self.wram7, addr as usize & 0xFFFF, size, value);
                } else if let Some(off) = self.shared_wram_offset(cpu, addr) {
                    self.note_code_write(CODE_SHARED, off);
                    wr(&mut self.shared_wram, off, size, value);
                }
            }
            0x04 => {
                self.io_ops += 1;
                self.io_write(cpu, addr, size, value)
            }
            0x05 if cpu == ARM9 => wr(&mut self.palette, addr as usize & 0x7FF, size, value),
            0x06 => {
                if let Some(off) = self.vram_offset(cpu, addr) {
                    wr(&mut self.vram, off, size, value);
                }
            }
            0x07 if cpu == ARM9 => wr(&mut self.oam, addr as usize & 0x7FF, size, value),
            _ => {
                self.note(format!("ARM{} W{} {addr:#010x}", if cpu == ARM9 { 9 } else { 7 }, size * 8));
            }
        }
    }
}

pub struct Bus9<'a>(pub &'a mut Hardware);
pub struct Bus7<'a>(pub &'a mut Hardware);

macro_rules! impl_bus {
    ($ty:ident, $cpu:expr) => {
        impl Bus for $ty<'_> {
            #[inline(always)]
            fn read8(&mut self, a: u32) -> u8 {
                self.0.read_c::<$cpu, 1>(a) as u8
            }
            #[inline(always)]
            fn read16(&mut self, a: u32) -> u16 {
                self.0.read_c::<$cpu, 2>(a) as u16
            }
            #[inline(always)]
            fn read32(&mut self, a: u32) -> u32 {
                self.0.read_c::<$cpu, 4>(a)
            }
            #[inline(always)]
            fn write8(&mut self, a: u32, v: u8) {
                self.0.write_c::<$cpu, 1>(a, v as u32)
            }
            #[inline(always)]
            fn write16(&mut self, a: u32, v: u16) {
                self.0.write_c::<$cpu, 2>(a, v as u32)
            }
            #[inline(always)]
            fn write32(&mut self, a: u32, v: u32) {
                self.0.write_c::<$cpu, 4>(a, v)
            }
        }
    };
}
impl_bus!(Bus9, ARM9);
impl_bus!(Bus7, ARM7);
