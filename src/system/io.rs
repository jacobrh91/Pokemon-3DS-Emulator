//! Memory-mapped I/O registers. Every access is turned into an aligned
//! 32-bit register access with a byte mask, so each register is handled once
//! regardless of whether software used LDRB, LDRH or LDR.

use super::dma::DmaTiming;
use super::irq::IRQ_HBLANK;
use super::memory::vram::VramMap;
use super::Hardware;

const ARM9: usize = 0;
const IO_SIZE: usize = 0x1100;

pub fn new_backing() -> [Vec<u8>; 2] {
    let mut io = [vec![0u8; IO_SIZE], vec![0u8; IO_SIZE]];
    io[0][0x300] = 1; // POSTFLG: booted
    io[1][0x300] = 1;
    io
}

fn size_mask(size: u32) -> u32 {
    match size {
        1 => 0xFF,
        2 => 0xFFFF,
        _ => 0xFFFF_FFFF,
    }
}

impl Hardware {
    pub fn io_byte(&self, cpu: usize, off: usize) -> u8 {
        self.io[cpu][off]
    }

    fn io_word(&self, cpu: usize, off: usize) -> u32 {
        u32::from_le_bytes(self.io[cpu][off..off + 4].try_into().unwrap())
    }

    fn set_io_word(&mut self, cpu: usize, off: usize, value: u32) {
        self.io[cpu][off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn merge_io_word(&mut self, cpu: usize, off: usize, value: u32, mask: u32) -> u32 {
        let merged = (self.io_word(cpu, off) & !mask) | (value & mask);
        self.set_io_word(cpu, off, merged);
        merged
    }

    fn log_io(&mut self, cpu: usize, write: bool, addr: u32, value: u32) {
        if self.trace_io {
            if self.io_ring.len() == 200_000 {
                self.io_ring.pop_front();
            }
            self.io_ring.push_back((cpu as u8, write, addr, value, self.cur_pc[cpu]));
        }
    }

    pub fn io_read(&mut self, cpu: usize, addr: u32, size: u32) -> u32 {
        let value = self.io_read_inner(cpu, addr, size);
        self.log_io(cpu, false, addr, value);
        value
    }

    pub fn io_write(&mut self, cpu: usize, addr: u32, size: u32, value: u32) {
        self.log_io(cpu, true, addr, value);
        self.io_write_inner(cpu, addr, size, value);
    }

    fn io_read_inner(&mut self, cpu: usize, addr: u32, size: u32) -> u32 {
        let off = addr & 0x00FF_FFFF;
        if cpu == 1 && off >> 20 == 8 {
            return self.wifi.read(off as usize, size);
        }
        if off >= 0x10_0000 {
            return match off & !3 {
                0x10_0000 => self.ipc_fifo_recv(cpu),
                0x10_0010 => self.card_read_data(),
                _ => 0,
            };
        }
        let word = self.reg_read(cpu, off & !3);
        (word >> ((off & 3) * 8)) & size_mask(size)
    }

    fn io_write_inner(&mut self, cpu: usize, addr: u32, size: u32, value: u32) {
        let off = addr & 0x00FF_FFFF;
        if cpu == 1 && off >> 20 == 8 {
            return self.wifi.write(off as usize, size, value);
        }
        if off >= 0x10_0000 {
            return;
        }
        let shift = (off & 3) * 8;
        self.reg_write(cpu, off & !3, value << shift, size_mask(size) << shift);
    }

    fn reg_read(&mut self, cpu: usize, reg: u32) -> u32 {
        let off = reg as usize;
        if off >= IO_SIZE {
            return 0;
        }
        match reg {
            0x004 => self.dispstat_read(cpu) | (self.video.vcount as u32) << 16,
            0x0B0..=0x0DF => {
                let ch = (reg as usize - 0xB0) / 12;
                if (reg - 0xB0) % 12 == 8 { self.dma[cpu][ch].control } else { 0 }
            }
            0x100 | 0x104 | 0x108 | 0x10C => {
                self.flush_timers(cpu);
                let t = self.timers[cpu][(reg as usize - 0x100) / 4];
                t.counter as u32 | (t.control as u32) << 16
            }
            0x130 => (!self.keys as u32 & 0x3FF) | (self.io_word(cpu, off) & 0xFFFF_0000),
            0x134 if cpu == 1 => {
                // EXTKEYIN: X and Y buttons, pen (bit 6), all active low.
                let mut ext = 0x7Fu32 & !((self.keys as u32 >> 10) & 3);
                if self.touch.is_some() {
                    ext &= !0x40;
                }
                (self.io_word(cpu, off) & 0xFFFF) | ext << 16
            }
            0x138 if cpu == 1 => self.rtc.read() as u32,
            0x180 => self.ipc_sync_read(cpu),
            0x184 => self.ipc_fifo_cnt_read(cpu),
            0x1A0 => {
                let data = self.infrared.as_ref().map_or(self.backup.last_output, |ir| ir.last_output);
                self.card.aux_spi_cnt as u32 | (data as u32) << 16
            },
            0x1A4 => self.card.rom_ctrl,
            0x1C0 if cpu == 1 => self.spi.control as u32 | (self.spi.data as u32) << 16,
            // GXSTAT: the geometry FIFO always looks empty (commands run on arrival).
            0x600 if cpu == ARM9 => 0x0600_0000 | self.gx.status() | (self.io_word(cpu, off) & 0xC000_0000),
            0x620 | 0x624 | 0x628 | 0x62C if cpu == ARM9 => self.gx.test_result[(reg as usize - 0x620) / 4] as u32,
            0x640..=0x67C if cpu == ARM9 => self.gx.clip_matrix_result((off - 0x640) / 4),
            0x680..=0x6A0 if cpu == ARM9 => self.gx.vector_matrix_result((off - 0x680) / 4),
            0x630 if cpu == ARM9 => (self.gx.test_result[0] as u32 & 0xFFFF) | (self.gx.test_result[1] as u32) << 16,
            0x634 if cpu == ARM9 => self.gx.test_result[2] as u32 & 0xFFFF,
            0x400..=0x4FC if cpu == 1 && reg & 0xF == 0 => {
                // SOUNDxCNT: the busy bit follows the channel.
                let word = self.io_word(cpu, off) & 0x7FFF_FFFF;
                word | (self.sound.channels[(off - 0x400) / 0x10].playing as u32) << 31
            }
            0x208 => self.irq[cpu].ime as u32,
            0x210 => self.irq[cpu].ie,
            0x214 => self.irq[cpu].flags,
            0x240 if cpu == 1 => {
                let map = &self.vram_cnt;
                ((map[2] & 0x87 == 0x82) as u32) | ((map[3] & 0x87 == 0x82) as u32) << 1 | (self.wramcnt as u32) << 8
            }
            _ => self.io_word(cpu, off),
        }
    }

    fn reg_write(&mut self, cpu: usize, reg: u32, value: u32, mask: u32) {
        let off = reg as usize;
        if off >= IO_SIZE {
            return;
        }
        match reg {
            0x004 => {
                let old = self.video.dispstat[cpu] as u32;
                let new = (old & !mask) | (value & mask);
                self.video.dispstat[cpu] = (new & 0xFFB8) as u16;
            }
            0x0B0..=0x0DF => {
                let ch = (reg as usize - 0xB0) / 12;
                match (reg - 0xB0) % 12 {
                    0 => self.dma[cpu][ch].source = (self.dma[cpu][ch].source & !mask) | (value & mask),
                    4 => self.dma[cpu][ch].dest = (self.dma[cpu][ch].dest & !mask) | (value & mask),
                    _ => {
                        let old = self.dma[cpu][ch].control;
                        self.dma_write_control(cpu, ch, (old & !mask) | (value & mask));
                    }
                }
            }
            0x100 | 0x104 | 0x108 | 0x10C => {
                self.flush_timers(cpu);
                let t = (reg as usize - 0x100) / 4;
                if mask & 0xFFFF != 0 {
                    let r = self.timers[cpu][t].reload as u32;
                    self.timers[cpu][t].reload = ((r & !mask) | (value & mask)) as u16;
                }
                if mask >> 16 != 0 {
                    let c = (self.timers[cpu][t].control as u32) << 16;
                    let new = (c & !mask) | (value & mask);
                    self.timers[cpu][t].write_control((new >> 16) as u16 & 0xC7);
                    self.refresh_timers_on(cpu);
                    self.timer_budget[cpu] = self.cycles_to_next_overflow(cpu);
                }
            }
            0x130 => {
                self.merge_io_word(cpu, off, value, mask & 0xFFFF_0000);
            }
            0x180 => {
                if mask & 0xFF00 != 0 {
                    self.ipc_sync_write(cpu, value);
                }
            }
            0x184 => self.ipc_fifo_cnt_write(cpu, value & mask & 0xFFFF),
            0x188 => self.ipc_fifo_send(cpu, value),
            0x1A0 => {
                if mask & 0xFFFF != 0 {
                    let old = self.card.aux_spi_cnt as u32;
                    self.card.aux_spi_cnt = ((old & !mask) | (value & mask)) as u16 & 0xE0C3;
                }
                let cnt = self.card.aux_spi_cnt;
                // Bit 15 enables the port; bit 13 selects the backup chip over the ROM.
                if mask & 0x00FF_0000 != 0 && cnt & 0x8000 != 0 && cnt & 0x2000 != 0 {
                    let (byte, hold) = ((value >> 16) as u8, cnt & 0x40 != 0);
                    match &mut self.infrared {
                        Some(ir) => ir.transfer(byte, hold, &mut self.backup),
                        None => self.backup.transfer(byte, hold),
                    };
                }
            }
            0x1A4 => {
                let merged = (self.card.rom_ctrl & !mask) | (value & mask);
                self.card_write_rom_ctrl(merged);
            }
            0x1A8 | 0x1AC => {
                let merged = self.merge_io_word(cpu, off, value, mask);
                let base = (reg - 0x1A8) as usize;
                self.card.command[base..base + 4].copy_from_slice(&merged.to_le_bytes());
            }
            0x138 if cpu == 1 => {
                if mask & 0xFFFF != 0 {
                    let merged = (self.rtc.read() as u32 & !mask) | (value & mask);
                    self.rtc.write(merged as u16);
                }
            }
            0x1C0 if cpu == 1 => self.spi_write(value, mask),
            0x028 | 0x02C | 0x038 | 0x03C | 0x1028 | 0x102C | 0x1038 | 0x103C if cpu == ARM9 => {
                self.merge_io_word(cpu, off, value, mask);
                let engine = (reg >> 12) as usize;
                let bg = ((reg & 0xFF) >= 0x30) as usize;
                let axis = ((reg & 4) != 0) as usize;
                self.affine_cur[engine][bg][axis] = self.affine_reg(engine, bg, axis);
            }
            // Geometry command ports: commands are discarded for now, but the FIFO "drains"
            // at once, so a requested FIFO interrupt fires.
            0x400..=0x43C if cpu == ARM9 => {
                self.gx.fifo_write(value);
                self.gx_fifo_irq();
            }
            0x440..=0x5FC if cpu == ARM9 => {
                self.gx.port_write(((reg - 0x400) / 4) as u8, value);
                self.gx_fifo_irq();
            }
            0x600 if cpu == ARM9 => {
                if value & mask & 0x8000 != 0 {
                    self.gx.clear_stack_error();
                }
                self.merge_io_word(cpu, off, value, mask & 0xC000_0000);
                self.gx_fifo_irq();
            }
            0x400..=0x4FC if cpu == 1 && reg & 0xF == 0 => {
                self.merge_io_word(cpu, off, value, mask);
                self.sound_cnt_written((off - 0x400) / 0x10);
            }
            0x208 => {
                if mask & 1 != 0 {
                    self.irq[cpu].ime = value & 1 != 0;
                }
            }
            0x210 => self.irq[cpu].ie = (self.irq[cpu].ie & !mask) | (value & mask),
            0x214 => self.irq[cpu].flags &= !(value & mask),
            0x240 | 0x244 | 0x248 if cpu == ARM9 => {
                self.merge_io_word(cpu, off, value, mask);
                self.update_vram_banks();
            }
            0x280 | 0x290 | 0x294 | 0x298 | 0x29C | 0x2B0 | 0x2B8 | 0x2BC if cpu == ARM9 => {
                self.merge_io_word(cpu, off, value, mask);
                self.math_update();
            }
            _ => {
                self.merge_io_word(cpu, off, value, mask);
            }
        }
    }

    /// GXSTAT bits 30-31 pick when the geometry FIFO interrupts: 1 = below half
    /// full, 2 = empty. The stub FIFO satisfies both whenever asked.
    pub(super) fn gx_fifo_irq(&mut self) {
        if self.io_word(ARM9, 0x600) >> 30 != 0 {
            self.irq[ARM9].raise(1 << 21);
        }
    }

    fn update_vram_banks(&mut self) {
        let io = &self.io[ARM9];
        let cnt = [io[0x240], io[0x241], io[0x242], io[0x243], io[0x244], io[0x245], io[0x246], io[0x248], io[0x249]];
        self.vram_cnt = cnt;
        self.vram_map = VramMap::rebuild(&cnt);
        // Code compiled from shared WRAM may now be looking at different memory.
        if self.wramcnt != io[0x247] & 3 {
            self.wramcnt = io[0x247] & 3;
            self.code_flush = true;
        }
    }

    fn dispstat_read(&self, cpu: usize) -> u32 {
        let v = &self.video;
        let target = (v.dispstat[cpu] >> 8 | (v.dispstat[cpu] & 0x80) << 1) & 0x1FF;
        (v.dispstat[cpu] as u32 & 0xFFB8)
            | v.in_vblank as u32
            | (v.in_hblank as u32) << 1
            | ((v.vcount == target) as u32) << 2
    }

    /// Fire the scanline-end events for the line that just finished.
    pub fn video_hblank(&mut self) {
        self.video.in_hblank = true;
        for cpu in 0..2 {
            if self.video.dispstat[cpu] & (1 << 4) != 0 {
                self.irq[cpu].raise(IRQ_HBLANK);
            }
        }
        if self.video.vcount < 192 {
            self.dma_trigger(ARM9, DmaTiming::HBlank);
        }
    }

    fn math_update(&mut self) {
        let io = &mut self.io[ARM9];
        let rd32 = |io: &Vec<u8>, o: usize| u32::from_le_bytes(io[o..o + 4].try_into().unwrap());
        let rd64 = |io: &Vec<u8>, o: usize| u64::from_le_bytes(io[o..o + 8].try_into().unwrap());

        let divcnt = rd32(io, 0x280);
        let (numer, denom): (i64, i64) = match divcnt & 3 {
            0 => (rd32(io, 0x290) as i32 as i64, rd32(io, 0x298) as i32 as i64),
            1 => (rd64(io, 0x290) as i64, rd32(io, 0x298) as i32 as i64),
            _ => (rd64(io, 0x290) as i64, rd64(io, 0x298) as i64),
        };
        let (quot, rem) = if denom == 0 {
            (if numer < 0 { 1 } else { -1 }, numer)
        } else {
            (numer.wrapping_div(denom), numer.wrapping_rem(denom))
        };
        io[0x2A0..0x2A8].copy_from_slice(&quot.to_le_bytes());
        io[0x2A8..0x2B0].copy_from_slice(&rem.to_le_bytes());
        let cnt = (divcnt & 3) | ((denom == 0) as u32) << 14;
        io[0x280..0x284].copy_from_slice(&cnt.to_le_bytes());

        let sqrtcnt = rd32(io, 0x2B0);
        let param = if sqrtcnt & 1 == 0 { rd32(io, 0x2B8) as u64 } else { rd64(io, 0x2B8) };
        let root = isqrt(param) as u32;
        io[0x2B4..0x2B8].copy_from_slice(&root.to_le_bytes());
    }
}

pub fn isqrt(n: u64) -> u64 {
    let n = n as u128;
    let mut x = (n as f64).sqrt() as u128;
    while x * x > n {
        x -= 1;
    }
    while (x + 1) * (x + 1) <= n {
        x += 1;
    }
    x as u64
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_hardware;
    use super::isqrt;

    #[test]
    fn byte_writes_merge_into_registers() {
        let mut hw = test_hardware();
        hw.io_write(0, 0x0400_0210, 1, 0x11);
        hw.io_write(0, 0x0400_0211, 1, 0x22);
        assert_eq!(hw.io_read(0, 0x0400_0210, 4), 0x2211);
    }

    #[test]
    fn if_is_write_one_to_clear() {
        let mut hw = test_hardware();
        hw.irq[0].flags = 0b1010;
        hw.io_write(0, 0x0400_0214, 4, 0b0010);
        assert_eq!(hw.irq[0].flags, 0b1000);
    }

    #[test]
    fn hardware_divider() {
        let mut hw = test_hardware();
        hw.io_write(0, 0x0400_0290, 4, 100);
        hw.io_write(0, 0x0400_0298, 4, 7);
        assert_eq!(hw.io_read(0, 0x0400_02A0, 4), 14);
        assert_eq!(hw.io_read(0, 0x0400_02A8, 4), 2);
    }

    #[test]
    fn hardware_sqrt() {
        let mut hw = test_hardware();
        hw.io_write(0, 0x0400_02B8, 4, 1_000_000);
        assert_eq!(hw.io_read(0, 0x0400_02B4, 4), 1000);
        assert_eq!(isqrt(u64::MAX), 0xFFFF_FFFF);
    }

    #[test]
    fn keyinput_is_active_low() {
        let mut hw = test_hardware();
        hw.keys = 0b1;
        assert_eq!(hw.io_read(0, 0x0400_0130, 2), 0x3FE);
    }
}
