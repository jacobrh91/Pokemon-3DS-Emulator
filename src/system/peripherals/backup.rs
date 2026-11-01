//! Save memory behind the card slot's SPI bus, modelled as a serial NOR flash
//! (the 4 Mbit part Pokemon HeartGold/SoulSilver use). Operations complete
//! instantly, so the status register never reports "write in progress".

use std::path::Path;

const PAGE: usize = 256;
const SECTOR: usize = 64 * 1024;
const BLOCK_32K: usize = 32 * 1024;
const SUBSECTOR: usize = 4 * 1024;
const DEFAULT_JEDEC_ID: [u8; 3] = [0x20, 0x40, 0x12];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
enum Phase {
    Idle,
    Address { remaining: u8 },
    Data,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Backup {
    pub data: Vec<u8>,
    /// Set when `data` changed since it was last written to disk.
    pub dirty: bool,
    phase: Phase,
    command: u8,
    address: usize,
    write_enabled: bool,
    id_index: usize,
    pub last_output: u8,
    jedec_id: [u8; 3],
    /// When set, every command is recorded here as it starts (a debugging aid).
    #[serde(skip)]
    pub command_log: Option<Vec<String>>,
    #[serde(skip)]
    reported_unknown: Vec<u8>,
    #[serde(skip)]
    transaction_bytes: u32,
}

impl Backup {
    pub fn new(size: usize) -> Backup {
        Backup {
            data: vec![0xFF; size],
            dirty: false,
            phase: Phase::Idle,
            command: 0,
            address: 0,
            write_enabled: false,
            id_index: 0,
            last_output: 0,
            jedec_id: DEFAULT_JEDEC_ID,
            command_log: None,
            reported_unknown: Vec::new(),
            transaction_bytes: 0,
        }
    }

    /// Replace the contents with a save file's, keeping the chip size.
    pub fn load(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.data.len());
        self.data[..n].copy_from_slice(&bytes[..n]);
        self.dirty = false;
    }

    pub fn save_to(&mut self, path: impl AsRef<Path>) -> std::io::Result<()> {
        std::fs::write(path, &self.data)?;
        self.dirty = false;
        Ok(())
    }

    fn status(&self) -> u8 {
        (self.write_enabled as u8) << 1
    }

    /// Clock one byte through the chip. `hold` is the card controller's CS-hold
    /// bit: when clear, this is the last byte of the transaction.
    pub fn transfer(&mut self, byte: u8, hold: bool) -> u8 {
        let out = match self.phase {
            Phase::Idle => {
                self.start_command(byte);
                0
            }
            Phase::Address { remaining } => {
                self.address = (self.address << 8) | byte as usize;
                if remaining == 1 {
                    self.address %= self.data.len();
                    if let Some(log) = &mut self.command_log {
                        if !matches!(self.command, 0x03 | 0x0B) {
                            log.push(format!("   address {:06x}", self.address));
                        }
                    }
                    self.phase = Phase::Data;
                    self.after_address();
                } else {
                    self.phase = Phase::Address { remaining: remaining - 1 };
                }
                0
            }
            Phase::Data => {
                self.transaction_bytes += 1;
                self.data_byte(byte)
            }
        };
        if !hold {
            self.end_transaction();
        }
        self.last_output = out;
        out
    }

    fn start_command(&mut self, command: u8) {
        if let Some(log) = &mut self.command_log {
            if !matches!(command, 0x00 | 0x03 | 0x05) {
                log.push(format!("cmd {command:02x} (write-enabled: {})", self.write_enabled));
            }
        }
        self.command = command;
        self.address = 0;
        self.id_index = 0;
        match command {
            0x06 => self.write_enabled = true,
            0x04 => self.write_enabled = false,
            0x05 | 0x01 | 0x9F | 0x9E => self.phase = Phase::Data,
            0x03 | 0x0B | 0x02 | 0x0A | 0xD8 | 0xDB | 0x20 | 0x52 => self.phase = Phase::Address { remaining: 3 },
            0xC7 | 0x60 if self.write_enabled => {
                self.data.fill(0xFF);
                self.dirty = true;
            }
            0x00 | 0xFF | 0xC7 | 0x60 | 0xAB | 0xB9 => {}
            other => {
                // A command this chip model doesn't know would silently drop data.
                if !self.reported_unknown.contains(&other) {
                    self.reported_unknown.push(other);
                    eprintln!("warning: unsupported save-chip command {other:#04x}");
                }
            }
        }
    }

    /// Erase commands act as soon as their address is complete.
    fn after_address(&mut self) {
        if !self.write_enabled {
            return;
        }
        match self.command {
            0xD8 => self.erase(self.address & !(SECTOR - 1), SECTOR),
            0x52 => self.erase(self.address & !(BLOCK_32K - 1), BLOCK_32K),
            0x20 => self.erase(self.address & !(SUBSECTOR - 1), SUBSECTOR),
            0xDB => self.erase(self.address & !(PAGE - 1), PAGE),
            _ => {}
        }
    }

    fn erase(&mut self, start: usize, len: usize) {
        self.data[start..start + len].fill(0xFF);
        self.dirty = true;
    }

    fn data_byte(&mut self, byte: u8) -> u8 {
        match self.command {
            0x05 => self.status(),
            0x9F | 0x9E => {
                let id = self.jedec_id.get(self.id_index).copied().unwrap_or(0);
                self.id_index += 1;
                id
            }
            0x03 | 0x0B => {
                let value = self.data[self.address];
                self.address = (self.address + 1) % self.data.len();
                value
            }
            0x02 | 0x0A if self.write_enabled => {
                // Page program (0x02) can only clear bits. Page write (0x0A) replaces just
                // the bytes it is sent and leaves the rest of the page alone; the game relies
                // on that when it writes a block's footer into an already-written page.
                if self.command == 0x0A {
                    self.data[self.address] = byte;
                } else {
                    self.data[self.address] &= byte;
                }
                self.dirty = true;
                self.address = (self.address & !(PAGE - 1)) | ((self.address + 1) & (PAGE - 1));
                0
            }
            _ => 0,
        }
    }

    fn end_transaction(&mut self) {
        if let Some(log) = &mut self.command_log {
            if matches!(self.command, 0x02 | 0x0A | 0xD8 | 0x52 | 0x20 | 0xDB | 0x01 | 0xC7 | 0x60) {
                log.push(format!("   done: {} data bytes", self.transaction_bytes));
            }
        }
        self.transaction_bytes = 0;
        if matches!(self.command, 0x02 | 0x0A | 0xD8 | 0x52 | 0x20 | 0xDB | 0x01 | 0xC7 | 0x60) {
            self.write_enabled = false;
        }
        self.phase = Phase::Idle;
    }
}

/// The infrared chip on HeartGold/SoulSilver cartridges (for the Pokewalker),
/// which sits between the card slot and the save flash. The first byte of every
/// transaction is addressed to it: 0x00 passes the rest through to the flash and
/// 0x08 reads its ID. No infrared link is modelled.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct InfraredChip {
    command: Option<u8>,
    pub last_output: u8,
}

const IR_PASS_THROUGH: u8 = 0x00;
const IR_READ_ID: u8 = 0x08;
const IR_ID: u8 = 0xAA;

impl InfraredChip {
    pub fn transfer(&mut self, byte: u8, hold: bool, flash: &mut Backup) -> u8 {
        let out = match self.command {
            None => {
                self.command = Some(byte);
                0
            }
            Some(IR_PASS_THROUGH) => flash.transfer(byte, hold),
            Some(IR_READ_ID) => IR_ID,
            Some(_) => 0,
        };
        if !hold {
            self.command = None;
        }
        self.last_output = out;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(chip: &mut Backup, bytes: &[u8]) -> Vec<u8> {
        let last = bytes.len() - 1;
        bytes.iter().enumerate().map(|(i, &b)| chip.transfer(b, i != last)).collect()
    }

    #[test]
    fn infrared_chip_answers_its_id_and_passes_the_rest_to_flash() {
        let mut flash = Backup::new(0x80000);
        flash.data[0x123] = 0x42;
        let mut ir = InfraredChip::default();
        ir.transfer(0x08, true, &mut flash);
        assert_eq!(ir.transfer(0, false, &mut flash), 0xAA);
        for (byte, hold) in [(0x00, true), (0x03, true), (0, true), (1, true), (0x23, true)] {
            ir.transfer(byte, hold, &mut flash);
        }
        assert_eq!(ir.transfer(0, false, &mut flash), 0x42);
    }

    #[test]
    fn blank_chip_reads_ff() {
        let mut chip = Backup::new(0x80000);
        let out = send(&mut chip, &[0x03, 0, 0, 0, 0, 0]);
        assert_eq!(&out[4..], &[0xFF, 0xFF]);
    }

    #[test]
    fn program_requires_write_enable() {
        let mut chip = Backup::new(0x80000);
        send(&mut chip, &[0x02, 0, 0, 0, 0x12]);
        assert_eq!(chip.data[0], 0xFF);
        send(&mut chip, &[0x06]);
        assert_eq!(send(&mut chip, &[0x05, 0])[1], 0x02);
        send(&mut chip, &[0x02, 0, 0, 0, 0x12]);
        assert_eq!(chip.data[0], 0x12);
        assert!(chip.dirty);
        // Write-enable is consumed by the program command.
        assert_eq!(send(&mut chip, &[0x05, 0])[1], 0x00);
    }

    #[test]
    fn page_write_replaces_only_the_bytes_it_is_sent() {
        let mut chip = Backup::new(0x80000);
        chip.data[0x100..0x104].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        send(&mut chip, &[0x06]);
        send(&mut chip, &[0x0A, 0, 1, 1, 0xAB, 0xCD]);
        assert_eq!(&chip.data[0x100..0x104], &[0x11, 0xAB, 0xCD, 0x44]);
    }

    /// The game writes a block's last full page, then its footer into the same page.
    #[test]
    fn footer_write_does_not_disturb_earlier_bytes_in_its_page() {
        let mut chip = Backup::new(0x80000);
        let mut page = vec![0x0A, 0, 0xF6, 0x00];
        page.extend(std::iter::repeat_n(0x5A, 0x18));
        send(&mut chip, &[0x06]);
        send(&mut chip, &page);
        send(&mut chip, &[0x06]);
        send(&mut chip, &[0x0A, 0, 0xF6, 0x18, 1, 2, 3, 4]);
        assert!(chip.data[0xF600..0xF618].iter().all(|&b| b == 0x5A));
        assert_eq!(&chip.data[0xF618..0xF61C], &[1, 2, 3, 4]);
    }

    #[test]
    fn four_kilobyte_erase_clears_only_its_subsector() {
        let mut chip = Backup::new(0x80000);
        chip.data[0x4F123] = 0;
        chip.data[0x4EFFF] = 0;
        chip.data[0x50000] = 0;
        send(&mut chip, &[0x06]);
        send(&mut chip, &[0x20, 0x04, 0xF1, 0x00]);
        assert_eq!(chip.data[0x4F123], 0xFF);
        assert_eq!(chip.data[0x4EFFF], 0, "previous 4 KiB neighbour untouched");
        assert_eq!(chip.data[0x50000], 0, "next 4 KiB neighbour untouched");
        // The erase consumed the write-enable latch.
        assert_eq!(send(&mut chip, &[0x05, 0])[1], 0x00);
    }

    #[test]
    fn rewriting_after_erase_gives_exactly_the_new_data() {
        let mut chip = Backup::new(0x80000);
        for pass in [0xF0u8, 0x0F] {
            send(&mut chip, &[0x06]);
            send(&mut chip, &[0x20, 0x04, 0x00, 0x00]);
            send(&mut chip, &[0x06]);
            send(&mut chip, &[0x02, 0x04, 0x00, 0x00, pass, pass]);
            assert_eq!(&chip.data[0x40000..0x40002], &[pass, pass]);
        }
    }

    #[test]
    fn sector_erase_sets_ff() {
        let mut chip = Backup::new(0x80000);
        chip.data[0x10005] = 0;
        send(&mut chip, &[0x06]);
        send(&mut chip, &[0xD8, 1, 0, 0]);
        assert_eq!(chip.data[0x10005], 0xFF);
    }

    #[test]
    fn load_accepts_files_with_emulator_footers_and_short_files() {
        let mut chip = Backup::new(0x80000);
        let mut file = vec![0x11u8; 0x80000];
        file.extend_from_slice(b"|-DESMUME SAVE-|"); // footer appended by some emulators
        chip.load(&file);
        assert_eq!(chip.data.len(), 0x80000);
        assert_eq!(chip.data[0x7FFFF], 0x11);

        let mut chip = Backup::new(0x80000);
        chip.load(&[0x22; 0x1000]);
        assert_eq!(chip.data[0xFFF], 0x22);
        assert_eq!(chip.data[0x1000], 0xFF, "bytes past a short file stay erased");
        assert!(!chip.dirty);
    }

    #[test]
    fn jedec_id() {
        let mut chip = Backup::new(0x80000);
        assert_eq!(&send(&mut chip, &[0x9F, 0, 0, 0])[1..], &DEFAULT_JEDEC_ID);
    }
}
