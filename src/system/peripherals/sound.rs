//! The ARM7's 16 sound channels and mixer, producing stereo at 32768 Hz
//! (bus clock / 1024). Capture and the speaker bias are not modelled.

use crate::system::Hardware;

/// Bus cycles per output sample.
const CYCLES_PER_SAMPLE: u32 = 1024;
/// Channel timers run at half the bus clock.
const TICKS_PER_SAMPLE: u32 = CYCLES_PER_SAMPLE / 2;
/// Roughly two seconds of stereo output; older samples are dropped past this.
const MAX_BUFFERED: usize = 32768 * 2 * 2;

const IMA_INDEX_ADJUST: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];
const IMA_STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

#[derive(Clone, Copy, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Channel {
    pub playing: bool,
    source: u32,
    format: u8,
    /// Sample position (bytes for PCM8, halfwords for PCM16, nibbles for ADPCM).
    pos: u32,
    total: u32,
    loop_start: u32,
    repeat: u8,
    /// Timer ticks accumulated towards the next sample.
    ticks: u32,
    /// Current output, signed 16-bit.
    sample: i32,
    adpcm_sample: i32,
    adpcm_index: i32,
    adpcm_loop_sample: i32,
    adpcm_loop_index: i32,
    lfsr: u32,
    duty_pos: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Sound {
    pub channels: [Channel; 16],
    cycles: u32,
    /// Interleaved left/right samples waiting to be played.
    pub out: Vec<i16>,
}

impl Sound {
    pub fn new() -> Sound {
        Sound { channels: [Channel::default(); 16], cycles: 0, out: Vec::new() }
    }
}

impl Default for Sound {
    fn default() -> Self {
        Self::new()
    }
}

impl Hardware {
    pub(in crate::system) fn sound_reg(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.io[1][off..off + 4].try_into().unwrap())
    }

    /// A channel's CNT register was written: start or stop it.
    pub(in crate::system) fn sound_cnt_written(&mut self, ch: usize) {
        let base = 0x400 + ch * 0x10;
        let cnt = self.sound_reg(base);
        if cnt & 0x8000_0000 == 0 {
            self.sound.channels[ch].playing = false;
            return;
        }
        if self.sound.channels[ch].playing {
            return;
        }
        let source = self.sound_reg(base + 4) & 0x07FF_FFFC;
        let pnt = (self.sound_reg(base + 8) >> 16) & 0xFFFF;
        let len = self.sound_reg(base + 12) & 0x3F_FFFF;
        let format = ((cnt >> 29) & 3) as u8;
        let (total, loop_start) = match format {
            0 => ((pnt + len) * 4, pnt * 4),
            1 => ((pnt + len) * 2, pnt * 2),
            // ADPCM: the first word is a header, not audio.
            2 => (((pnt + len) * 8).saturating_sub(8), (pnt * 8).saturating_sub(8)),
            _ => (u32::MAX, 0),
        };
        let mut channel = Channel {
            playing: total > 0,
            source,
            format,
            total,
            loop_start,
            repeat: ((cnt >> 27) & 3) as u8,
            lfsr: 0x7FFF,
            ..Channel::default()
        };
        if format == 2 {
            let header = self.read(1, source, 4);
            channel.adpcm_sample = header as u16 as i16 as i32;
            channel.adpcm_index = ((header >> 16) & 0x7F).min(88) as i32;
            channel.adpcm_loop_sample = channel.adpcm_sample;
            channel.adpcm_loop_index = channel.adpcm_index;
            channel.sample = channel.adpcm_sample;
        }
        self.sound.channels[ch] = channel;
    }

    /// Advance the mixer by `cycles` bus cycles, producing output samples.
    pub fn sound_advance(&mut self, cycles: u32) {
        self.sound.cycles += cycles;
        while self.sound.cycles >= CYCLES_PER_SAMPLE {
            self.sound.cycles -= CYCLES_PER_SAMPLE;
            self.mix_sample();
        }
    }

    pub(in crate::system) fn mix_sample(&mut self) {
        let master = self.sound_reg(0x500) & 0xFFFF;
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for n in 0..16 {
            if !self.sound.channels[n].playing {
                continue;
            }
            let timer = (self.sound_reg(0x400 + n * 0x10 + 8) & 0xFFFF) as u32;
            let period = (0x1_0000 - timer).max(1);
            let mut ch = self.sound.channels[n];
            ch.ticks += TICKS_PER_SAMPLE;
            while ch.ticks >= period && ch.playing {
                ch.ticks -= period;
                self.advance_channel(n, &mut ch);
            }
            self.sound.channels[n] = ch;

            let cnt = self.sound_reg(0x400 + n * 0x10);
            let volume = (cnt & 0x7F) as f32 / 127.0;
            let shift = [0, 1, 2, 4][((cnt >> 8) & 3) as usize];
            let pan = ((cnt >> 16) & 0x7F) as f32 / 127.0;
            let level = ch.sample as f32 / 32768.0 * volume / (1 << shift) as f32;
            left += level * (1.0 - pan);
            right += level * pan;
        }
        let gain = if master & 0x8000 != 0 { (master & 0x7F) as f32 / 127.0 * 0.75 } else { 0.0 };
        for v in [left, right] {
            self.sound.out.push(((v * gain).clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        if self.sound.out.len() > MAX_BUFFERED {
            let excess = self.sound.out.len() - MAX_BUFFERED;
            self.sound.out.drain(..excess);
        }
    }

    pub(in crate::system) fn advance_channel(&mut self, n: usize, ch: &mut Channel) {
        match ch.format {
            0 => ch.sample = (self.read(1, ch.source + ch.pos, 1) as u8 as i8 as i32) << 8,
            1 => ch.sample = self.read(1, ch.source + ch.pos * 2, 2) as u16 as i16 as i32,
            2 => {
                let byte = self.read(1, ch.source + 4 + ch.pos / 2, 1);
                let nibble = if ch.pos & 1 == 0 { byte & 0xF } else { byte >> 4 } as i32;
                let step = IMA_STEP[ch.adpcm_index as usize];
                let mut diff = step >> 3;
                if nibble & 1 != 0 {
                    diff += step >> 2;
                }
                if nibble & 2 != 0 {
                    diff += step >> 1;
                }
                if nibble & 4 != 0 {
                    diff += step;
                }
                ch.adpcm_sample = (ch.adpcm_sample + if nibble & 8 != 0 { -diff } else { diff }).clamp(-0x7FFF, 0x7FFF);
                ch.adpcm_index = (ch.adpcm_index + IMA_INDEX_ADJUST[(nibble & 7) as usize]).clamp(0, 88);
                ch.sample = ch.adpcm_sample;
            }
            _ if n >= 14 => {
                let carry = ch.lfsr & 1;
                ch.lfsr >>= 1;
                if carry != 0 {
                    ch.lfsr ^= 0x6000;
                    ch.sample = -0x7FFF;
                } else {
                    ch.sample = 0x7FFF;
                }
            }
            _ if n >= 8 => {
                let duty = (self.sound_reg(0x400 + n * 0x10) >> 24) & 7;
                ch.sample = if (ch.duty_pos & 7) >= 7 - duty && duty != 7 { 0x7FFF } else { -0x7FFF };
                ch.duty_pos += 1;
            }
            _ => ch.sample = 0,
        }
        ch.pos += 1;
        if ch.format == 2 && ch.pos == ch.loop_start {
            ch.adpcm_loop_sample = ch.adpcm_sample;
            ch.adpcm_loop_index = ch.adpcm_index;
        }
        if ch.pos >= ch.total {
            if ch.repeat == 1 {
                ch.pos = ch.loop_start;
                ch.adpcm_sample = ch.adpcm_loop_sample;
                ch.adpcm_index = ch.adpcm_loop_index;
            } else {
                ch.playing = false;
                if ch.repeat == 2 {
                    // One-shot sounds hold their last value until restarted.
                    self.io[1][0x400 + n * 0x10 + 3] &= 0x7F;
                }
            }
        }
        if !ch.playing {
            self.io[1][0x400 + n * 0x10 + 3] &= 0x7F;
        }
    }
}

/// BIOS sound helper tables (SWI 0x1A-0x1C), computed rather than dumped.
pub fn sine_table(index: u32) -> u32 {
    let i = (index & 0x3F) as f64;
    ((i * std::f64::consts::FRAC_PI_2 / 64.0).sin() * 32768.0).round().min(32767.0) as u32
}

pub fn pitch_table(index: u32) -> u32 {
    let i = (index % 768) as f64;
    (65536.0 * ((i / 768.0).exp2() - 1.0)).round() as u32
}

/// Channel volume for `index` = dB + 723, in tenths of a dB. The sound driver divides
/// by 2, 4 or 16 in hardware below -6, -12 and -24 dB, so the table makes up for each
/// divider and is a sawtooth, not a smooth curve.
pub fn volume_table(index: u32) -> u32 {
    let db = index.min(723) as i32 - 723;
    let divider = match db {
        ..-240 => 16.0,
        -240..-120 => 4.0,
        -120..-60 => 2.0,
        _ => 1.0,
    };
    (127.0 * 10f64.powf(db as f64 / 200.0) * divider).round().min(127.0) as u32
}

#[cfg(test)]
mod tests {
    use crate::system::tests::test_hardware;
    use super::*;

    fn start_pcm8_loop(hw: &mut Hardware, ch: usize, data: &[u8], timer: u16) {
        for (i, b) in data.iter().enumerate() {
            hw.write(1, 0x0200_0000 + i as u32, 1, *b as u32);
        }
        let base = 0x0400_0400 + ch as u32 * 0x10;
        hw.io_write(1, 0x0400_0500, 2, 0x8000 | 0x7F);
        hw.io_write(1, base + 4, 4, 0x0200_0000);
        hw.io_write(1, base + 8, 2, timer as u32);
        hw.io_write(1, base + 10, 2, 0);
        hw.io_write(1, base + 12, 4, (data.len() / 4) as u32);
        // Full volume, centre pan, loop, 8-bit PCM, start.
        hw.io_write(1, base, 4, 0x7F | 64 << 16 | 1 << 27 | 1 << 31);
    }

    #[test]
    fn pcm8_channel_produces_audio_and_loops() {
        let mut hw = test_hardware();
        start_pcm8_loop(&mut hw, 0, &[0x40, 0x40, 0x40, 0x40], 0xFC00);
        assert_ne!(hw.io_read(1, 0x0400_0403, 1) & 0x80, 0, "busy bit set while playing");
        hw.sound_advance(CYCLES_PER_SAMPLE * 100);
        assert_eq!(hw.sound.out.len(), 200);
        assert!(hw.sound.out.iter().any(|&s| s > 1000), "expected audible positive samples");
        assert!(hw.sound.channels[0].playing, "looping channel keeps playing");
    }

    #[test]
    fn one_shot_channel_stops_and_clears_busy() {
        let mut hw = test_hardware();
        start_pcm8_loop(&mut hw, 1, &[0x20; 4], 0xFC00);
        let base = 0x0400_0410;
        hw.io_write(1, base, 4, 0x7F | 2 << 27 | 1 << 31); // restart as one-shot
        hw.sound.channels[1].playing = false;
        hw.sound_cnt_written(1);
        hw.sound_advance(CYCLES_PER_SAMPLE * 200);
        assert!(!hw.sound.channels[1].playing);
        assert_eq!(hw.io_read(1, 0x0400_0413, 1) & 0x80, 0);
    }

    #[test]
    fn master_disable_silences_output() {
        let mut hw = test_hardware();
        start_pcm8_loop(&mut hw, 0, &[0x40; 4], 0xFC00);
        hw.io_write(1, 0x0400_0500, 2, 0);
        hw.sound_advance(CYCLES_PER_SAMPLE * 10);
        assert!(hw.sound.out.iter().all(|&s| s == 0));
    }

    #[test]
    fn pan_steers_between_speakers() {
        let mut hw = test_hardware();
        start_pcm8_loop(&mut hw, 0, &[0x40; 4], 0xFC00);
        hw.io_write(1, 0x0400_0402, 2, 0x8800); // pan hard left, still looping and started
        hw.sound_advance(CYCLES_PER_SAMPLE * 10);
        let (l, r) = (hw.sound.out[18], hw.sound.out[19]);
        assert!(l > 0 && r == 0, "left {l} right {r}");
    }

    #[test]
    fn noise_channel_alternates() {
        let mut hw = test_hardware();
        let base = 0x0400_0400 + 14 * 0x10;
        hw.io_write(1, 0x0400_0500, 2, 0x8000 | 0x7F);
        hw.io_write(1, base + 8, 2, 0xFF00);
        hw.io_write(1, base, 4, 0x7F | 64 << 16 | 3 << 29 | 1 << 31);
        hw.sound_advance(CYCLES_PER_SAMPLE * 50);
        let pos = hw.sound.out.iter().filter(|&&s| s > 0).count();
        let neg = hw.sound.out.iter().filter(|&&s| s < 0).count();
        assert!(pos > 5 && neg > 5);
    }

    #[test]
    fn adpcm_decodes_toward_the_encoded_slope() {
        let mut hw = test_hardware();
        // Header: sample 0, index 20; then nibbles 0x7 (large positive step) repeated.
        hw.write(1, 0x0200_0000, 4, 20 << 16);
        for i in 0..4 {
            hw.write(1, 0x0200_0004 + i * 4, 4, 0x7777_7777);
        }
        let base = 0x0400_0400;
        hw.io_write(1, 0x0400_0500, 2, 0x8000 | 0x7F);
        hw.io_write(1, base + 4, 4, 0x0200_0000);
        hw.io_write(1, base + 8, 2, 0xFC00);
        hw.io_write(1, base + 10, 2, 1);
        hw.io_write(1, base + 12, 4, 4);
        hw.io_write(1, base, 4, 0x7F | 64 << 16 | 2 << 27 | 2 << 29 | 1 << 31);
        hw.sound_advance(CYCLES_PER_SAMPLE * 20);
        assert!(hw.sound.channels[0].sample > 0);
    }

    #[test]
    fn bios_tables_match_known_entries() {
        assert_eq!([pitch_table(0), pitch_table(1), pitch_table(2), pitch_table(3), pitch_table(4)], [0, 59, 118, 178, 237]);
        assert_eq!(sine_table(0), 0);
        assert_eq!(sine_table(1), 804);
        assert_eq!(volume_table(723), 127);
        // Half amplitude at -6 dB, and the value jumps back up once the /2 divider applies.
        assert_eq!(volume_table(723 - 60), 64);
        assert_eq!(volume_table(723 - 61), 126);
    }
}
