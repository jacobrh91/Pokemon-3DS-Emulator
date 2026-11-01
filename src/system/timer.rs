use super::irq::IRQ_TIMER0;
use super::Hardware;

const PRESCALER_SHIFT: [u32; 4] = [0, 6, 8, 10];

#[derive(Clone, Copy, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Timer {
    pub reload: u16,
    pub counter: u16,
    pub control: u16,
    pub(super) accum: u32,
}

impl Timer {
    pub fn enabled(&self) -> bool {
        self.control & 0x80 != 0
    }

    pub(super) fn cascade(&self) -> bool {
        self.control & 0x04 != 0
    }

    pub fn write_control(&mut self, control: u16) {
        if !self.enabled() && control & 0x80 != 0 {
            self.counter = self.reload;
            self.accum = 0;
        }
        self.control = control;
    }
}

impl Hardware {
    /// Bring a CPU's timers up to date with the cycles that have passed since the last
    /// time, and work out how many more cycles can pass before one could overflow.
    pub(super) fn flush_timers(&mut self, cpu: usize) {
        let pending = std::mem::take(&mut self.timer_pending[cpu]);
        self.tick_timers(cpu, pending);
        self.timer_budget[cpu] = self.cycles_to_next_overflow(cpu);
    }

    /// Cycles until the soonest overflow of a running timer that counts bus cycles
    /// (cascaded timers only move when another overflows).
    pub(super) fn cycles_to_next_overflow(&self, cpu: usize) -> u32 {
        (0..4)
            .filter(|&t| self.timers_on[cpu] >> t & 1 != 0)
            .filter_map(|t| {
                let timer = &self.timers[cpu][t];
                if timer.cascade() && t != 0 {
                    return None;
                }
                let shift = PRESCALER_SHIFT[(timer.control & 3) as usize];
                let ticks = 0x1_0000 - timer.counter as u64;
                Some(((ticks << shift) - timer.accum as u64).clamp(1, u32::MAX as u64) as u32)
            })
            .min()
            .unwrap_or(u32::MAX)
    }

    /// Note which of a CPU's timers are running, so ticking can skip idle ones cheaply.
    pub(super) fn refresh_timers_on(&mut self, cpu: usize) {
        self.timers_on[cpu] = (0..4).fold(0, |mask, t| mask | (self.timers[cpu][t].enabled() as u8) << t);
    }

    /// Advance one CPU's four timers by `cycles` bus-clock cycles.
    #[inline]
    pub fn tick_timers(&mut self, cpu: usize, cycles: u32) {
        if self.timers_on[cpu] == 0 {
            return;
        }
        for t in 0..4 {
            if self.timers_on[cpu] >> t & 1 == 0 {
                continue;
            }
            let timer = &mut self.timers[cpu][t];
            if timer.cascade() && t != 0 {
                continue;
            }
            let shift = PRESCALER_SHIFT[(timer.control & 3) as usize];
            timer.accum += cycles;
            let ticks = timer.accum >> shift;
            timer.accum &= (1 << shift) - 1;
            if ticks != 0 {
                self.add_timer_ticks(cpu, t, ticks);
            }
        }
    }

    fn add_timer_ticks(&mut self, cpu: usize, t: usize, ticks: u32) {
        let timer = &mut self.timers[cpu][t];
        let total = timer.counter as u32 + ticks;
        if total < 0x1_0000 {
            timer.counter = total as u16;
            return;
        }
        let period = 0x1_0000 - timer.reload as u32;
        let over = total - 0x1_0000;
        let overflows = 1 + over / period;
        timer.counter = (timer.reload as u32 + over % period) as u16;
        if timer.control & 0x40 != 0 {
            self.irq[cpu].raise(IRQ_TIMER0 << t);
        }
        if t < 3 {
            let next = self.timers[cpu][t + 1];
            if next.enabled() && next.cascade() {
                self.add_timer_ticks(cpu, t + 1, overflows);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_hardware;

    #[test]
    fn timer_overflows_and_raises_irq() {
        let mut hw = test_hardware();
        hw.timers[0][0].reload = 0xFFF0;
        hw.timers[0][0].write_control(0x80 | 0x40);
        hw.refresh_timers_on(0);
        hw.tick_timers(0, 0x10);
        assert_eq!(hw.timers[0][0].counter, 0xFFF0);
        assert_ne!(hw.irq[0].flags & (1 << 3), 0);
    }

    #[test]
    fn cascade_counts_overflows() {
        let mut hw = test_hardware();
        hw.timers[1][0].reload = 0xFFFF;
        hw.timers[1][0].write_control(0x80);
        hw.timers[1][1].write_control(0x80 | 0x04);
        hw.refresh_timers_on(1);
        hw.tick_timers(1, 3);
        assert_eq!(hw.timers[1][1].counter, 3);
    }
}
