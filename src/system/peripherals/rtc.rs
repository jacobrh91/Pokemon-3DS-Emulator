//! Real-time clock on the ARM7 (S-3511A style), bit-banged through one register.
//! Time comes from the host's local clock.

#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(serde::Serialize, serde::Deserialize)]
enum Phase {
    /// Chip select low.
    Idle,
    Command,
    ReadData,
    WriteData,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Rtc {
    reg: u16,
    data_out: bool,
    phase: Phase,
    command: u8,
    bit_count: u32,
    msb_first: bool,
    register: usize,
    out: [u8; 7],
    out_len: usize,
    out_bit: usize,
    in_byte: u8,
    in_index: usize,
    status1: u8,
    status2: u8,
    adjust: u8,
    free: u8,
}

fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

/// Local date and time as (year since 2000, month, day, weekday, hour, minute, second).
fn local_time() -> (u32, u32, u32, u32, u32, u32, u32) {
    // SAFETY: localtime_r only writes into the tm we pass it.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        (
            (tm.tm_year as u32).saturating_sub(100) % 100,
            tm.tm_mon as u32 + 1,
            tm.tm_mday as u32,
            tm.tm_wday as u32,
            tm.tm_hour as u32,
            tm.tm_min as u32,
            tm.tm_sec as u32,
        )
    }
}

impl Rtc {
    pub fn new() -> Rtc {
        Rtc {
            reg: 0,
            data_out: false,
            phase: Phase::Idle,
            command: 0,
            bit_count: 0,
            msb_first: false,
            register: 0,
            out: [0; 7],
            out_len: 0,
            out_bit: 0,
            in_byte: 0,
            in_index: 0,
            status1: 0,
            status2: 0,
            adjust: 0,
            free: 0,
        }
    }

    pub fn read(&self) -> u16 {
        // Pin levels follow what the console drives, except data while the chip is talking.
        let mut v = self.reg;
        if self.reg & 0x10 == 0 {
            v = (v & !1) | self.data_out as u16;
        }
        v
    }

    pub fn write(&mut self, value: u16) {
        let old = self.reg;
        self.reg = value & 0x77;
        let chip_select = value & 4 != 0;
        if !chip_select {
            self.phase = Phase::Idle;
            return;
        }
        if old & 4 == 0 {
            self.phase = Phase::Command;
            self.bit_count = 0;
            self.command = 0;
        }
        let clock_rising = value & 2 != 0 && old & 2 == 0;
        let clock_falling = value & 2 == 0 && old & 2 != 0;
        // The console's bits are latched on the rising edge; the chip's own bits
        // change on the falling edge, so software reads them while the clock is low.
        if clock_rising && value & 0x10 != 0 {
            self.clock_in_bit((value & 1) as u8);
        } else if clock_falling && value & 0x10 == 0 && self.phase == Phase::ReadData {
            self.clock_out_bit();
        }
    }

    fn clock_in_bit(&mut self, bit: u8) {
        match self.phase {
            Phase::Command => {
                self.command |= bit << self.bit_count;
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.decode_command();
                }
            }
            Phase::WriteData => {
                let pos = if self.msb_first { 7 - self.bit_count } else { self.bit_count };
                self.in_byte |= bit << pos;
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.store_byte(self.in_byte);
                    self.in_byte = 0;
                    self.bit_count = 0;
                    self.in_index += 1;
                }
            }
            _ => {}
        }
    }

    fn decode_command(&mut self) {
        let cmd = self.command;
        // The SDK sends the command either low-nibble-first or bit-reversed.
        let (register, read, msb_first) = if cmd & 0x0F == 0x06 {
            (((cmd >> 4) & 7) as usize, cmd & 0x80 != 0, false)
        } else if cmd & 0xF0 == 0x60 {
            (((cmd >> 1) & 7) as usize, cmd & 1 != 0, true)
        } else {
            self.phase = Phase::Idle;
            return;
        };
        self.register = register;
        self.msb_first = msb_first;
        self.bit_count = 0;
        self.in_byte = 0;
        self.in_index = 0;
        if read {
            self.load_output();
            self.phase = Phase::ReadData;
        } else {
            self.phase = Phase::WriteData;
        }
    }

    fn load_output(&mut self) {
        self.out = [0; 7];
        self.out_bit = 0;
        let (y, mo, d, wd, h, mi, s) = local_time();
        match self.register {
            0 => {
                self.out[0] = self.status1;
                self.out_len = 1;
            }
            1 => {
                self.out[0] = self.status2;
                self.out_len = 1;
            }
            2 => {
                self.out = [bcd(y), bcd(mo), bcd(d), wd as u8, bcd(h), bcd(mi), bcd(s)];
                self.out_len = 7;
            }
            3 => {
                self.out[..3].copy_from_slice(&[bcd(h), bcd(mi), bcd(s)]);
                self.out_len = 3;
            }
            4 | 5 => self.out_len = 3,
            6 => {
                self.out[0] = self.adjust;
                self.out_len = 1;
            }
            _ => {
                self.out[0] = self.free;
                self.out_len = 1;
            }
        }
    }

    fn clock_out_bit(&mut self) {
        let byte = self.out_bit / 8;
        if byte >= self.out_len {
            self.data_out = false;
            return;
        }
        let bit = self.out_bit % 8;
        let pos = if self.msb_first { 7 - bit } else { bit };
        self.data_out = (self.out[byte] >> pos) & 1 != 0;
        self.out_bit += 1;
    }

    fn store_byte(&mut self, value: u8) {
        // Setting the date/time isn't supported; only the control registers stick.
        match (self.register, self.in_index) {
            (0, 0) => self.status1 = value & 0x0E,
            (1, 0) => self.status2 = value,
            (6, 0) => self.adjust = value,
            (7, 0) => self.free = value,
            _ => {}
        }
    }
}

impl Default for Rtc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clock `byte` into the chip, LSB first, the way the SDK does.
    fn send(rtc: &mut Rtc, byte: u8) {
        for i in 0..8 {
            let bit = (byte >> i) as u16 & 1;
            rtc.write(0x14 | bit); // clock low, data valid
            rtc.write(0x16 | bit); // clock rising edge
        }
    }

    /// Read a byte the way the SDK does: drop the clock, sample the data pin, raise the clock.
    fn receive(rtc: &mut Rtc) -> u8 {
        let mut v = 0;
        for i in 0..8 {
            rtc.write(0x04);
            v |= ((rtc.read() & 1) as u8) << i;
            rtc.write(0x06);
        }
        v
    }

    #[test]
    fn reads_back_written_status_register() {
        let mut rtc = Rtc::new();
        rtc.write(0x04);
        rtc.write(0x14);
        send(&mut rtc, 0x06 | (0 << 4)); // write status 1
        send(&mut rtc, 0x0A);
        rtc.write(0x00);

        rtc.write(0x04);
        rtc.write(0x14);
        send(&mut rtc, 0x06 | 0x80); // read status 1
        assert_eq!(receive(&mut rtc), 0x0A);
    }

    #[test]
    fn date_time_read_returns_seven_bcd_bytes() {
        let mut rtc = Rtc::new();
        rtc.write(0x04);
        rtc.write(0x14);
        send(&mut rtc, 0x06 | (2 << 4) | 0x80);
        let bytes: Vec<u8> = (0..7).map(|_| receive(&mut rtc)).collect();
        assert!(bytes.iter().all(|b| b & 0x0F <= 9 && b >> 4 <= 9 || *b == bytes[3]));
        assert!(bytes[1] >= 1 && bytes[1] <= 0x12);
        assert!(bytes[2] >= 1 && bytes[2] <= 0x31);
    }

    #[test]
    fn bcd_encoding() {
        assert_eq!(bcd(59), 0x59);
        assert_eq!(bcd(7), 0x07);
    }
}
