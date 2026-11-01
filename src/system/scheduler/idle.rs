//! Spin-loop detection. A CPU that comes back to the same instruction with
//! identical registers and no side effects in between (no stores, no reads that
//! change hardware) will do exactly the same again, until something outside it
//! changes. Outside state only changes between scheduler slices, so whole
//! iterations of the loop can be skipped with no difference at all: the CPU ends
//! up in exactly the state, at exactly the cycle count, it would have reached.

/// A loop head (the target of a backward branch) may take over the probe after this many cycles.
const MIN_HOLD: u32 = 4;

pub struct IdleProbe {
    pc: u32,
    regs: [u32; 16],
    cpsr: u32,
    effects: u64,
    armed_at: u32,
    armed: bool,
}

impl IdleProbe {
    pub fn new() -> IdleProbe {
        // Starts due for arming, so the very first instruction of a slice is the probe point.
        IdleProbe { pc: u32::MAX, regs: [0; 16], cpsr: 0, effects: 0, armed_at: 0, armed: false }
    }

    /// Call before executing the instruction at `regs[15]`. `effects` counts the
    /// side effects the hardware has seen and `elapsed` the cycles run so far. If the
    /// CPU is spinning, returns the cycles one trip round the loop takes. `backward` says
    /// the code just jumped back to this address, which makes it a likely loop head.
    #[inline(always)]
    pub fn check(&mut self, regs: &[u32; 16], cpsr: u32, effects: u64, elapsed: u32, backward: bool) -> Option<u32> {
        if regs[15] == self.pc {
            return self.recurred(regs, cpsr, effects, elapsed);
        }
        // Only loop heads are worth watching, and a loop always jumps back to its head.
        if backward {
            let waited = elapsed.wrapping_sub(self.armed_at);
            if !self.armed || waited >= MIN_HOLD {
                self.arm(regs, cpsr, effects, elapsed);
            }
        }
        None
    }

    /// Back at the probe's instruction: the same state again means a spin.
    #[inline(never)]
    fn recurred(&mut self, regs: &[u32; 16], cpsr: u32, effects: u64, elapsed: u32) -> Option<u32> {
        if self.cpsr == cpsr && self.effects == effects && self.regs == *regs {
            let period = elapsed - self.armed_at;
            return (period > 0).then_some(period);
        }
        self.arm(regs, cpsr, effects, elapsed);
        None
    }

    #[inline]
    fn arm(&mut self, regs: &[u32; 16], cpsr: u32, effects: u64, elapsed: u32) {
        self.armed_at = elapsed;
        self.pc = regs[15];
        self.regs = *regs;
        self.cpsr = cpsr;
        self.effects = effects;
        self.armed = true;
    }
}

impl Default for IdleProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs_at(pc: u32, r1: u32) -> [u32; 16] {
        let mut r = [0; 16];
        r[1] = r1;
        r[15] = pc;
        r
    }

    #[test]
    fn identical_state_at_the_same_pc_is_a_spin() {
        let mut p = IdleProbe::new();
        assert!(p.check(&regs_at(0x100, 7), 0, 0, 0, true).is_none());
        assert!(p.check(&regs_at(0x104, 7), 0, 0, 1, true).is_none());
        assert!(p.check(&regs_at(0x108, 7), 0, 0, 2, true).is_none());
        assert_eq!(p.check(&regs_at(0x100, 7), 0, 0, 3, true), Some(3), "one trip round is 3 cycles");
    }

    #[test]
    fn a_changing_register_means_progress() {
        let mut p = IdleProbe::new();
        for r1 in 0..100 {
            assert!(p.check(&regs_at(0x100, r1), 0, 0, 2 * r1, true).is_none(), "a counting loop is not idle");
            assert!(p.check(&regs_at(0x104, r1), 0, 0, 2 * r1 + 1, true).is_none());
        }
    }

    #[test]
    fn side_effects_or_flag_changes_mean_progress() {
        let mut p = IdleProbe::new();
        assert!(p.check(&regs_at(0x100, 1), 0, 5, 0, true).is_none());
        assert!(p.check(&regs_at(0x100, 1), 0, 6, 2, true).is_none(), "a store happened in the loop");
        assert!(p.check(&regs_at(0x100, 1), 1 << 30, 6, 4, true).is_none(), "flags changed");
        assert!(p.check(&regs_at(0x100, 1), 1 << 30, 6, 6, true).is_some());
    }

    #[test]
    fn long_loops_are_found_from_their_heads() {
        let mut p = IdleProbe::new();
        let mut spun = false;
        // A 20-instruction loop, started at an arbitrary place.
        for i in 0..400u32 {
            spun |= p.check(&regs_at(0x200 + 4 * (i % 20), 3), 0, 0, i, i % 20 == 0).is_some();
        }
        assert!(spun);
    }

    /// The optimised run loop (spin-loop skipping, lazy interrupt refresh) must match the
    /// plain one exactly. The two runs go frame by frame in step, because the game reads
    /// the host clock and a gap between runs would show up.
    #[test]
    fn skipping_changes_nothing_on_the_real_game() {
        use crate::cartridge::Cartridge;
        use crate::system::Nds;
        let make = |optimised: bool| {
            let mut nds = Nds::direct_boot(Cartridge::load("roms/Pokemon SoulSilver.nds").ok()?);
            nds.idle_skip = optimised;
            nds.eager_irq = !optimised;
            Some(nds)
        };
        let (Some(mut fast), Some(mut plain)) = (make(true), make(false)) else { return };
        for frame in 0..700 {
            // Some input, so the game isn't only idling on its title screen.
            let keys = if frame % 90 < 20 { 1 << 3 } else { 0 };
            fast.hw.keys = keys;
            plain.hw.keys = keys;
            fast.run_frame();
            plain.run_frame();
            assert_eq!(fast.hw.cycles, plain.hw.cycles, "cycle counts differ at frame {frame}");
            assert_eq!(fast.arm9.regs, plain.arm9.regs, "ARM9 differs at frame {frame}");
            assert_eq!(fast.arm7.regs, plain.arm7.regs, "ARM7 differs at frame {frame}");
            assert_eq!(fast.framebuffer(), plain.framebuffer(), "picture differs at frame {frame}");
            assert_eq!(fast.take_audio(), plain.take_audio(), "audio differs at frame {frame}");
        }
    }
}
