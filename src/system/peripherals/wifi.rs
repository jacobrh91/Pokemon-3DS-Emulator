//! The ARM7's Wi-Fi hardware, with no radio behind it. Registers and packet
//! RAM behave as plain memory, which satisfies the driver's start-up
//! self-tests; the baseband and RF chips are reached through indirect ports
//! that are never busy. Nothing is ever received, so scans find no one.

const IO_SIZE: usize = 0x1000;
const RAM_SIZE: usize = 0x2000;

const W_ID: usize = 0x000;
const W_POWERSTATE: usize = 0x03C;
const W_POWERFORCE: usize = 0x040;
const W_BB_CNT: usize = 0x158;
const W_BB_WRITE: usize = 0x15A;
const W_BB_READ: usize = 0x15C;
const W_BB_BUSY: usize = 0x15E;
const W_RF_BUSY: usize = 0x180;
/// The chip ID of an original DS.
const CHIP_ID: u16 = 0x1440;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Wifi {
    regs: Vec<u8>,
    ram: Vec<u8>,
    baseband: Vec<u8>,
}

impl Wifi {
    pub fn new() -> Wifi {
        Wifi { regs: vec![0; IO_SIZE], ram: vec![0; RAM_SIZE], baseband: vec![0; 0x100] }
    }

    pub fn read(&self, off: usize, size: u32) -> u32 {
        (0..size as usize).fold(0, |value, i| value | (self.read8(off + i) as u32) << (i * 8))
    }

    pub fn write(&mut self, off: usize, size: u32, value: u32) {
        if off & 0xFFFF == 0x8000 + W_BB_CNT && size == 2 {
            self.baseband_command(value as u16);
        }
        for i in 0..size as usize {
            self.write8(off + i, (value >> (i * 8)) as u8);
        }
        // Forcing the power state takes effect at once: bit 0 set means powered down.
        if off & 0xFFFF == 0x8000 + W_POWERFORCE && value & 0x8000 != 0 {
            self.regs[W_POWERSTATE + 1] = if value & 1 != 0 { 2 } else { 0 };
        }
    }

    fn read8(&self, off: usize) -> u8 {
        match off & 0xFFFF {
            o @ 0x4000..=0x5FFF => self.ram[o - 0x4000],
            o @ 0x8000..=0x8FFF => {
                let reg = o - 0x8000;
                match reg & !1 {
                    W_ID => (CHIP_ID >> ((reg & 1) * 8)) as u8,
                    W_BB_BUSY | W_RF_BUSY => 0,
                    _ => self.regs[reg],
                }
            }
            _ => 0,
        }
    }

    fn write8(&mut self, off: usize, value: u8) {
        match off & 0xFFFF {
            o @ 0x4000..=0x5FFF => self.ram[o - 0x4000] = value,
            o @ 0x8000..=0x8FFF => self.regs[o - 0x8000] = value,
            _ => {}
        }
    }

    /// W_BB_CNT: top nibble 5 writes `W_BB_WRITE` to a register, 6 reads one into `W_BB_READ`.
    fn baseband_command(&mut self, command: u16) {
        let index = (command & 0xFF) as usize;
        match command >> 12 {
            5 => self.baseband[index] = self.regs[W_BB_WRITE],
            6 => {
                self.regs[W_BB_READ] = self.baseband[index];
                self.regs[W_BB_READ + 1] = 0;
            }
            _ => {}
        }
    }
}

impl Default for Wifi {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_packet_ram_read_back() {
        let mut wifi = Wifi::new();
        wifi.write(0x8000 + 0x18, 2, 0xA5A5);
        assert_eq!(wifi.read(0x8000 + 0x18, 2), 0xA5A5);
        wifi.write(0x4000, 2, 0x5A5A);
        assert_eq!(wifi.read(0x4000, 2), 0x5A5A);
    }

    #[test]
    fn baseband_port_round_trips() {
        let mut wifi = Wifi::new();
        wifi.write(0x8000 + W_BB_WRITE, 2, 0x3C);
        wifi.write(0x8000 + W_BB_CNT, 2, 0x5000 | 7);
        wifi.write(0x8000 + W_BB_CNT, 2, 0x6000 | 7);
        assert_eq!(wifi.read(0x8000 + W_BB_READ, 2), 0x3C);
        assert_eq!(wifi.read(0x8000 + W_BB_BUSY, 2), 0);
    }

    #[test]
    fn forcing_power_updates_the_power_state() {
        let mut wifi = Wifi::new();
        wifi.write(0x8000 + W_POWERFORCE, 2, 0x8001);
        assert_eq!(wifi.read(0x8000 + W_POWERSTATE, 2) >> 8, 2);
        wifi.write(0x8000 + W_POWERFORCE, 2, 0x8000);
        assert_eq!(wifi.read(0x8000 + W_POWERSTATE, 2) >> 8, 0);
    }

    #[test]
    fn chip_id_is_read_only() {
        let mut wifi = Wifi::new();
        wifi.write(0x8000, 2, 0);
        assert_eq!(wifi.read(0x8000, 2), CHIP_ID as u32);
    }
}
