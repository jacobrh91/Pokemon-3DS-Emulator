//! Game card slot. Commands are executed as plain (unencrypted) reads: the
//! KEY1/KEY2 layers only protect the wire format, not the data.

use crate::system::irq::IRQ_CARD_DONE;
use crate::system::Hardware;
use crate::system::dma::DmaTiming;

const ROMCTRL_BUSY: u32 = 1 << 31;
const ROMCTRL_WORD_READY: u32 = 1 << 23;
const AUXSPI_IRQ: u16 = 1 << 14;

#[derive(Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Card {
    pub rom_ctrl: u32,
    pub aux_spi_cnt: u16,
    pub command: [u8; 8],
    data: Vec<u8>,
    pos: usize,
    /// Bus cycles until a started transfer's data is ready.
    delay: u32,
}

fn block_size(rom_ctrl: u32) -> usize {
    match (rom_ctrl >> 24) & 7 {
        0 => 0,
        7 => 4,
        n => 0x100 << n,
    }
}

/// Bus cycles for a transfer: the 8-byte command, the first-block gap (bits 0-12),
/// the data and the gap before each further 512-byte block (bits 16-21). Bit 27 picks
/// the bus clock: 5 cycles per byte, or 8 when set.
fn transfer_cycles(rom_ctrl: u32, size: usize) -> u32 {
    let per_byte = if rom_ctrl & (1 << 27) != 0 { 8 } else { 5 };
    let first_gap = rom_ctrl & 0x1FFF;
    let block_gap = (rom_ctrl >> 16) & 0x3F;
    let blocks = (size as u32).div_ceil(0x200);
    (8 + first_gap + size as u32 + block_gap * blocks) * per_byte
}

impl Hardware {
    pub fn card_chip_id(&self) -> u32 {
        let megabytes = (self.cart.rom.len() >> 20).max(1) as u32;
        0xC2 | ((megabytes - 1).min(0xFF) << 8)
    }

    /// EXMEMCNT bit 11 selects which CPU owns the slot (0 = ARM9).
    pub fn card_slot_owner(&self) -> usize {
        ((self.io_byte(0, 0x205) >> 3) & 1) as usize
    }

    pub fn card_words_ready(&self) -> u32 {
        if self.card.delay != 0 {
            return 0;
        }
        ((self.card.data.len() - self.card.pos) / 4) as u32
    }

    pub fn card_write_rom_ctrl(&mut self, value: u32) {
        let was_busy = self.card.rom_ctrl & ROMCTRL_BUSY != 0;
        // Bit 31 is write-start; bits 31 and 23 read back as our own status.
        self.card.rom_ctrl = (value & !(ROMCTRL_BUSY | ROMCTRL_WORD_READY)) | (self.card.rom_ctrl & (ROMCTRL_BUSY | ROMCTRL_WORD_READY));
        if value & ROMCTRL_BUSY != 0 && !was_busy {
            self.card_start_command(value);
        }
    }

    pub(in crate::system) fn card_start_command(&mut self, rom_ctrl: u32) {
        let size = block_size(rom_ctrl);
        let cmd = self.card.command;
        if self.trace_io {
            self.events.push(format!("CARD cmd {cmd:02x?} size {size:#x}"));
        }
        let mut data = vec![0xFF; size];
        match cmd[0] {
            0x00 => {
                for (i, b) in data.iter_mut().enumerate() {
                    *b = self.cart.rom.get(i % 0x1000).copied().unwrap_or(0xFF);
                }
            }
            0x90 | 0xB8 => {
                let id = self.card_chip_id().to_le_bytes();
                for (i, b) in data.iter_mut().enumerate() {
                    *b = id[i % 4];
                }
            }
            0xB7 => {
                let mut addr = u32::from_be_bytes([cmd[1], cmd[2], cmd[3], cmd[4]]) as usize;
                // The first 32 KiB of the card (header + secure area) can't be
                // read this way; it mirrors one 512-byte page of the secure area.
                if addr < 0x8000 {
                    addr = 0x8000 + (addr & 0x1FF);
                }
                let base = addr & !0xFFF;
                for (i, b) in data.iter_mut().enumerate() {
                    // Reads wrap inside their 4 KiB page.
                    let a = base + ((addr + i) & 0xFFF);
                    *b = self.cart.rom.get(a).copied().unwrap_or(0xFF);
                }
            }
            _ => {}
        }
        self.card.data = data;
        self.card.pos = 0;
        if size == 0 {
            self.card_finish();
        } else {
            // The transfer takes real time, so a long load spans several VBlanks.
            self.card.rom_ctrl |= ROMCTRL_BUSY;
            self.card.delay = transfer_cycles(rom_ctrl, size);
        }
    }

    /// Advance a transfer in progress by `cycles` bus cycles.
    pub fn card_tick(&mut self, cycles: u32) {
        if self.card.delay == 0 {
            return;
        }
        self.card.delay = self.card.delay.saturating_sub(cycles);
        if self.card.delay == 0 {
            self.card.rom_ctrl |= ROMCTRL_WORD_READY;
            self.dma_trigger(self.card_slot_owner(), DmaTiming::Card);
        }
    }

    pub(in crate::system) fn card_finish(&mut self) {
        self.card.rom_ctrl &= !(ROMCTRL_BUSY | ROMCTRL_WORD_READY);
        if self.card.aux_spi_cnt & AUXSPI_IRQ != 0 {
            self.irq[self.card_slot_owner()].raise(IRQ_CARD_DONE);
        }
    }

    pub fn card_read_data(&mut self) -> u32 {
        self.effects = self.effects.wrapping_add(1);
        let c = &mut self.card;
        if c.pos + 4 > c.data.len() {
            return 0xFFFF_FFFF;
        }
        let word = u32::from_le_bytes(c.data[c.pos..c.pos + 4].try_into().unwrap());
        c.pos += 4;
        if c.pos >= c.data.len() {
            self.card_finish();
        }
        word
    }
}

#[cfg(test)]
mod tests {
    use crate::system::tests::test_hardware;
    use super::transfer_cycles;

    #[test]
    fn slower_clock_and_gaps_lengthen_a_transfer() {
        assert_eq!(transfer_cycles(0, 0x200), (8 + 0x200) * 5);
        assert_eq!(transfer_cycles(1 << 27, 0x200), (8 + 0x200) * 8);
        assert_eq!(transfer_cycles(0x10, 4), (8 + 0x10 + 4) * 5);
    }

    #[test]
    fn b7_reads_rom_at_requested_address() {
        let mut hw = test_hardware();
        hw.cart.rom[0x9000..0x9004].copy_from_slice(&[1, 2, 3, 4]);
        hw.card.command = [0xB7, 0, 0, 0x90, 0x00, 0, 0, 0];
        hw.card_write_rom_ctrl((1 << 31) | (1 << 24));
        assert_eq!(hw.card.rom_ctrl & (1 << 23), 0, "no data until the transfer time has passed");
        hw.card_tick(transfer_cycles(1 << 24, 0x200));
        assert_ne!(hw.card.rom_ctrl & (1 << 23), 0);
        assert_eq!(hw.card_read_data(), 0x0403_0201);
    }

    #[test]
    fn transfer_completes_after_last_word() {
        let mut hw = test_hardware();
        hw.card.command = [0xB7, 0, 0, 0x90, 0x00, 0, 0, 0];
        hw.card_write_rom_ctrl((1 << 31) | (7 << 24));
        hw.card_tick(1000);
        hw.card_read_data();
        assert_eq!(hw.card.rom_ctrl & (1 << 31), 0);
    }
}
