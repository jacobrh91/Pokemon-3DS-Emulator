use super::irq::IRQ_DMA0;
use super::Hardware;

#[derive(Clone, Copy, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Dma {
    pub source: u32,
    pub dest: u32,
    /// Full CNT register: count in the low bits, control above.
    pub control: u32,
    cur_source: u32,
    cur_dest: u32,
    cur_count: u32,
}

/// Start timings, already normalised across the two CPUs' encodings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DmaTiming {
    Immediate,
    VBlank,
    HBlank,
    Card,
    /// ARM9 only: feed the 3D geometry command FIFO.
    GxFifo,
    Other,
}

impl Dma {
    pub fn enabled(&self) -> bool {
        self.control & (1 << 31) != 0
    }

    fn timing(&self, cpu: usize) -> DmaTiming {
        if cpu == 0 {
            match (self.control >> 27) & 7 {
                0 => DmaTiming::Immediate,
                1 => DmaTiming::VBlank,
                2 => DmaTiming::HBlank,
                5 => DmaTiming::Card,
                7 => DmaTiming::GxFifo,
                _ => DmaTiming::Other,
            }
        } else {
            match (self.control >> 28) & 3 {
                0 => DmaTiming::Immediate,
                1 => DmaTiming::VBlank,
                2 => DmaTiming::Card,
                _ => DmaTiming::Other,
            }
        }
    }

    fn count_mask(cpu: usize, channel: usize) -> u32 {
        match (cpu, channel) {
            (0, _) => 0x1F_FFFF,
            (_, 3) => 0xFFFF,
            _ => 0x3FFF,
        }
    }
}

impl Hardware {
    /// Write the CNT word of a channel; starts immediate transfers.
    pub fn dma_write_control(&mut self, cpu: usize, ch: usize, control: u32) {
        let was_enabled = self.dma[cpu][ch].enabled();
        self.dma[cpu][ch].control = control;
        if !was_enabled && control & (1 << 31) != 0 {
            let d = &mut self.dma[cpu][ch];
            d.cur_source = d.source;
            d.cur_dest = d.dest;
            let count = d.control & Dma::count_mask(cpu, ch);
            d.cur_count = if count == 0 { Dma::count_mask(cpu, ch) + 1 } else { count };
            match d.timing(cpu) {
                // The geometry FIFO never fills while 3D is a stub, so it always has room.
                DmaTiming::Immediate | DmaTiming::GxFifo => self.dma_run(cpu, ch, u32::MAX),
                // The card's data-request line is level-sensitive: a channel
                // enabled while words are already waiting starts right away.
                DmaTiming::Card if self.card_slot_owner() == cpu => self.dma_trigger(cpu, DmaTiming::Card),
                _ => {}
            }
        }
    }

    /// Run every enabled channel on `cpu` waiting for `timing`.
    pub fn dma_trigger(&mut self, cpu: usize, timing: DmaTiming) {
        for ch in 0..4 {
            let d = self.dma[cpu][ch];
            if !d.enabled() || d.timing(cpu) != timing {
                continue;
            }
            if timing == DmaTiming::Card {
                // One request per word the card has ready, so a channel that
                // moves a single word per request keeps going until the block ends.
                while self.dma[cpu][ch].enabled() && self.card_words_ready() > 0 {
                    let limit = self.card_words_ready();
                    self.dma_run(cpu, ch, limit);
                }
            } else {
                self.dma_run(cpu, ch, u32::MAX);
            }
        }
    }

    fn dma_run(&mut self, cpu: usize, ch: usize, limit: u32) {
        let mut d = self.dma[cpu][ch];
        let word = d.control & (1 << 26) != 0;
        let step = if word { 4 } else { 2 };
        let dest_ctrl = (d.control >> 21) & 3;
        let src_ctrl = (d.control >> 23) & 3;
        let timing = d.timing(cpu);

        // Card-timed channels move one block of words per trigger, bounded by
        // what the card has ready.
        let n = if timing == DmaTiming::Card { d.cur_count.min(limit) } else { d.cur_count };
        for _ in 0..n {
            let value = self.read(cpu, d.cur_source & !(step - 1), step);
            self.write(cpu, d.cur_dest & !(step - 1), step, value);
            d.cur_source = match src_ctrl {
                0 | 3 => d.cur_source.wrapping_add(step),
                1 => d.cur_source.wrapping_sub(step),
                _ => d.cur_source,
            };
            d.cur_dest = match dest_ctrl {
                0 | 3 => d.cur_dest.wrapping_add(step),
                1 => d.cur_dest.wrapping_sub(step),
                _ => d.cur_dest,
            };
        }
        d.cur_count -= n;

        if d.cur_count == 0 {
            if d.control & (1 << 30) != 0 {
                self.irq[cpu].raise(IRQ_DMA0 << ch);
            }
            if d.control & (1 << 25) != 0 && !matches!(timing, DmaTiming::Immediate | DmaTiming::GxFifo) {
                let count = d.control & Dma::count_mask(cpu, ch);
                d.cur_count = if count == 0 { Dma::count_mask(cpu, ch) + 1 } else { count };
                if dest_ctrl == 3 {
                    d.cur_dest = d.dest;
                }
            } else {
                d.control &= !(1 << 31);
            }
        }
        self.dma[cpu][ch] = d;
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_hardware;

    #[test]
    fn immediate_word_copy() {
        let mut hw = test_hardware();
        for i in 0..4u32 {
            hw.write(0, 0x0200_0000 + i * 4, 4, 0x1111 * (i + 1));
        }
        hw.dma[0][0].source = 0x0200_0000;
        hw.dma[0][0].dest = 0x0200_0100;
        hw.dma_write_control(0, 0, (1 << 31) | (1 << 26) | 4);
        assert_eq!(hw.read(0, 0x0200_010C, 4), 0x4444);
        assert!(!hw.dma[0][0].enabled());
    }

    #[test]
    fn fixed_source_fill() {
        let mut hw = test_hardware();
        hw.write(0, 0x0200_0000, 4, 0xDEAD_BEEF);
        hw.dma[0][1].source = 0x0200_0000;
        hw.dma[0][1].dest = 0x0200_0200;
        hw.dma_write_control(0, 1, (1 << 31) | (1 << 26) | (2 << 23) | 3);
        assert_eq!(hw.read(0, 0x0200_0208, 4), 0xDEAD_BEEF);
    }
}
