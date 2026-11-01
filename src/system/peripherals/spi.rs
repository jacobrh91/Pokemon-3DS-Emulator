//! ARM7 SPI bus: power management chip, firmware flash and touchscreen controller.

use super::backup::Backup;
use crate::system::irq::IRQ_SPI;
use crate::system::Hardware;

const FIRMWARE_SIZE: usize = 256 * 1024;
const USER_SETTINGS_OFFSET: usize = 0x3FE00;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Spi {
    pub control: u16,
    pub data: u16,
    pub firmware: Backup,
    power: PowerManagement,
    touch: Touchscreen,
}

/// Touchscreen controller: a control byte picks the channel, then the 12-bit
/// result is clocked out over the next two bytes.
#[derive(Default)]
#[derive(serde::Serialize, serde::Deserialize)]
struct Touchscreen {
    channel: u8,
    /// 0 = waiting for a control byte, 1 = next byte is the high half, 2 = the low half.
    step: u8,
    value: u16,
}

impl Touchscreen {
    /// `position` is the touched pixel, if any; the ADC reads 16 * pixel.
    fn transfer(&mut self, byte: u8, hold: bool, position: Option<(u8, u8)>) -> u8 {
        let out = match self.step {
            1 => {
                self.step = 2;
                (self.value >> 5) as u8
            }
            2 => {
                self.step = 0;
                ((self.value & 0x1F) << 3) as u8
            }
            _ => 0,
        };
        if self.step != 1 && byte & 0x80 != 0 {
            self.channel = (byte >> 4) & 7;
            self.value = match (self.channel, position) {
                (1, Some((_, y))) => (y as u16) << 4,
                (5, Some((x, _))) => (x as u16) << 4,
                _ => 0,
            };
            self.step = 1;
        }
        if !hold {
            self.step = 0;
        }
        out
    }
}

/// Register file of the power management chip; a transaction is a register
/// select byte (bit 7 = read) followed by one data byte.
#[derive(Default)]
#[derive(serde::Serialize, serde::Deserialize)]
struct PowerManagement {
    regs: [u8; 8],
    selected: Option<(usize, bool)>,
}

impl PowerManagement {
    fn transfer(&mut self, byte: u8, hold: bool) -> u8 {
        let out = match self.selected {
            None => {
                self.selected = Some(((byte & 7) as usize, byte & 0x80 != 0));
                0
            }
            Some((reg, true)) => self.regs[reg],
            Some((reg, false)) => {
                self.regs[reg] = byte;
                0
            }
        };
        if !hold {
            self.selected = None;
        }
        out
    }
}

impl Spi {
    pub fn new() -> Spi {
        let mut firmware = Backup::new(FIRMWARE_SIZE);
        firmware.data.copy_from_slice(&firmware_image());
        firmware.dirty = false;
        Spi { control: 0, data: 0, firmware, power: PowerManagement::default(), touch: Touchscreen::default() }
    }
}

impl Default for Spi {
    fn default() -> Self {
        Self::new()
    }
}

/// A firmware image with just enough in it for the SDK: a header pointing at
/// two identical user-settings blocks, each with its update counter and CRC.
fn firmware_image() -> Vec<u8> {
    let mut fw = vec![0xFF; FIRMWARE_SIZE];
    fw[..0x100].fill(0);
    fw[0x08..0x0C].copy_from_slice(b"MACh");
    fw[0x20..0x22].copy_from_slice(&((USER_SETTINGS_OFFSET / 8) as u16).to_le_bytes());
    write_wifi_config(&mut fw);
    let settings = crate::system::bios::user_settings();
    for copy in 0..2 {
        let base = USER_SETTINGS_OFFSET + copy * 0x100;
        fw[base..base + 0x100].fill(0);
        fw[base..base + 0x70].copy_from_slice(&settings);
        // Update counter, then the CRC the SDK checks.
        fw[base + 0x70..base + 0x72].copy_from_slice(&(copy as u16).to_le_bytes());
        let crc = crate::cartridge::crc16(&settings);
        fw[base + 0x72..base + 0x74].copy_from_slice(&crc.to_le_bytes());
    }
    fw
}

/// The wireless configuration block. The ARM7's driver refuses to start unless
/// its length is plausible and its CRC (over the bytes after the CRC) matches.
fn write_wifi_config(fw: &mut [u8]) {
    const LENGTH: usize = 0x138;
    fw[0x100..0x200].fill(0);
    fw[0x2C..0x2E].copy_from_slice(&(LENGTH as u16).to_le_bytes());
    fw[0x36..0x3C].copy_from_slice(&[0x00, 0x09, 0xBF, 0x11, 0x22, 0x33]); // MAC address
    fw[0x3C..0x3E].copy_from_slice(&0x3FFEu16.to_le_bytes()); // channels 1-13
    fw[0x3E..0x40].copy_from_slice(&0xFFFFu16.to_le_bytes());
    fw[0x40] = 2; // RF chip type
    fw[0x41] = 0x18; // bits per RF register
    let crc = crc16_arc(&fw[0x2C..0x2C + LENGTH]);
    fw[0x2A..0x2C].copy_from_slice(&crc.to_le_bytes());
}

/// CRC-16 with polynomial 0xA001 and an initial value of 0.
fn crc16_arc(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |crc, &byte| {
        (0..8).fold(crc ^ byte as u16, |c, _| if c & 1 != 0 { (c >> 1) ^ 0xA001 } else { c >> 1 })
    })
}

impl Hardware {
    pub(in crate::system) fn spi_write(&mut self, value: u32, mask: u32) {
        if mask & 0xFFFF != 0 {
            let old = self.spi.control as u32;
            self.spi.control = ((old & !mask) | (value & mask)) as u16 & 0xCF03;
        }
        if mask & 0xFFFF_0000 == 0 || self.spi.control & 0x8000 == 0 {
            return;
        }
        let byte = (value >> 16) as u8;
        let hold = self.spi.control & 0x0800 != 0;
        let out = match (self.spi.control >> 8) & 3 {
            0 => self.spi.power.transfer(byte, hold),
            1 => self.spi.firmware.transfer(byte, hold),
            2 => self.spi.touch.transfer(byte, hold, self.touch),
            _ => 0,
        };
        self.spi.data = out as u16;
        if self.spi.control & 0x4000 != 0 {
            self.irq[1].raise(IRQ_SPI);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::system::tests::test_hardware;

    fn spi_byte(hw: &mut crate::system::Hardware, byte: u8, hold: bool) -> u8 {
        let control = 0x8000 | 0x0100 | if hold { 0x0800 } else { 0 };
        hw.io_write(1, 0x0400_01C0, 2, control);
        hw.io_write(1, 0x0400_01C2, 2, byte as u32);
        hw.io_read(1, 0x0400_01C2, 2) as u8
    }

    #[test]
    fn touchscreen_reports_position_as_12_bit_adc() {
        let mut hw = test_hardware();
        hw.touch = Some((100, 50));
        let touch_byte = |hw: &mut crate::system::Hardware, b: u8, hold: bool| {
            hw.io_write(1, 0x0400_01C0, 2, 0x8000 | 0x0200 | if hold { 0x0800 } else { 0 });
            hw.io_write(1, 0x0400_01C2, 2, b as u32);
            hw.io_read(1, 0x0400_01C2, 2) as u16
        };
        touch_byte(&mut hw, 0xD0, true); // start, channel 5 (X)
        let high = touch_byte(&mut hw, 0, true);
        let low = touch_byte(&mut hw, 0, false);
        assert_eq!((high << 5) | (low >> 3), 100 << 4);
    }

    #[test]
    fn firmware_user_settings_have_valid_crc() {
        let mut hw = test_hardware();
        // READ 0x3FE00 through the SPI port, as the SDK's NVRAM driver does.
        for (i, b) in [0x03, 0x03, 0xFE, 0x00].into_iter().enumerate() {
            spi_byte(&mut hw, b, true);
            let _ = i;
        }
        let data: Vec<u8> = (0..0x74).map(|i| spi_byte(&mut hw, 0, i != 0x73)).collect();
        let crc = crate::cartridge::crc16(&data[..0x70]);
        assert_eq!(u16::from_le_bytes([data[0x72], data[0x73]]), crc);
        assert_eq!(data[0x64], 1);
    }
}
