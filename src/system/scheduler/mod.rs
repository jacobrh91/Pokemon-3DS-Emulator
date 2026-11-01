//! The console as a whole and the order things happen in: scanline timing, interleaving the
//! two CPUs in short slices, and the per-CPU run loop (interrupts, spin-loop skipping, JIT).

mod idle;

use super::*;

/// Bus-clock cycles (33.514 MHz) per scanline, and where HBlank starts.
const LINE_CYCLES: u32 = 2130;
const HBLANK_START: u32 = 1536;
const LINES_PER_FRAME: u16 = 263;
const VISIBLE_LINES: u16 = 192;
/// CPUs are interleaved in slices of this many bus cycles.
const SLICE: u32 = 32;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Nds {
    pub arm9: Cpu,
    pub arm7: Cpu,
    pub hw: Hardware,
    /// Pending IntrWait flags per CPU while the simulated BIOS call is blocked.
    wait: [Option<u32>; 2],
    pub frames: u64,
    /// Debug aids: stop the ARM9 when it reaches this address, and keep a
    /// ring of the PCs each CPU executed.
    #[serde(skip)]
    pub break9: Option<u32>,
    #[serde(skip)]
    pub break_hit: bool,
    #[serde(skip)]
    pub pc_trace: [std::collections::VecDeque<u32>; 2],
    #[serde(skip)]
    pub trace_pcs: bool,
    /// Skip spin loops (see `idle`). Only turned off to check that skipping changes nothing.
    #[serde(skip, default = "default_true")]
    pub idle_skip: bool,
    /// Refresh the interrupt lines before every instruction and tick timers every slice,
    /// as the first version did, instead of only when needed. Only used to check they agree.
    #[serde(skip)]
    pub eager_irq: bool,
    /// Compiled code for ARM-state programs in main RAM.
    #[serde(skip)]
    pub jit: crate::jit::Jit,
}

fn default_true() -> bool {
    true
}

impl Nds {
    /// Build a console with `cart` booted the way the firmware would leave it.
    pub fn direct_boot(cart: Cartridge) -> Nds {
        let mut nds = Nds {
            arm9: Cpu::new(Arch::V5TE),
            arm7: Cpu::new(Arch::V4T),
            hw: Hardware::new(cart),
            wait: [None; 2],
            frames: 0,
            break9: None,
            break_hit: false,
            pc_trace: Default::default(),
            trace_pcs: false,
            idle_skip: true,
            eager_irq: false,
            jit: Default::default(),
        };
        bios::direct_boot(&mut nds.arm9, &mut nds.arm7, &mut nds.hw);
        nds
    }

    /// Audio produced since the last call, as interleaved left/right samples at 32768 Hz.
    pub fn take_audio(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.hw.sound.out)
    }

    /// The last completed picture: top screen then bottom, 256x192 each.
    pub fn framebuffer(&self) -> &[u32] {
        &self.hw.frame
    }

    /// Run one full video frame (263 scanlines).
    pub fn run_frame(&mut self) {
        for _ in 0..LINES_PER_FRAME {
            self.run_scanline();
        }
        self.frames += 1;
    }

    /// Run one scanline's worth of time on both CPUs.
    pub fn run_scanline(&mut self) {
        self.begin_line();
        self.run_cycles(HBLANK_START);
        self.hw.video_hblank();
        self.run_cycles(LINE_CYCLES - HBLANK_START);
        self.end_line();
    }

    fn begin_line(&mut self) {
        let v = &mut self.hw.video;
        v.in_hblank = false;
        match v.vcount {
            0 => v.in_vblank = false,
            VISIBLE_LINES => v.in_vblank = true,
            _ => {}
        }
        let vcount = v.vcount;
        self.hw.gx_fifo_irq();
        if (vcount as usize) < gpu::HEIGHT {
            self.hw.render_scanline(vcount as usize);
        }
        for cpu in 0..2 {
            let ds = self.hw.video.dispstat[cpu];
            let target = (ds >> 8 | (ds & 0x80) << 1) & 0x1FF;
            if ds & (1 << 5) != 0 && vcount == target {
                self.hw.irq[cpu].raise(IRQ_VCOUNT);
            }
            if vcount == VISIBLE_LINES && ds & (1 << 3) != 0 {
                self.hw.irq[cpu].raise(IRQ_VBLANK);
            }
        }
        if vcount == VISIBLE_LINES {
            self.hw.gx_vblank();
            self.hw.dma_trigger(ARM9, DmaTiming::VBlank);
            self.hw.dma_trigger(ARM7, DmaTiming::VBlank);
        }
    }

    fn end_line(&mut self) {
        let v = &mut self.hw.video;
        v.vcount += 1;
        if v.vcount == LINES_PER_FRAME {
            v.vcount = 0;
        }
    }

    fn run_cycles(&mut self, cycles: u32) {
        let mut left = cycles;
        while left > 0 {
            let slice = left.min(SLICE);
            self.run_cpu(ARM9, slice * 2);
            self.run_cpu(ARM7, slice);
            for cpu in [ARM9, ARM7] {
                self.hw.timer_pending[cpu] += slice;
                if self.eager_irq || self.hw.timer_pending[cpu] >= self.hw.timer_budget[cpu] {
                    self.hw.flush_timers(cpu);
                }
            }
            self.hw.sound_advance(slice);
            self.hw.card_tick(slice);
            left -= slice;
        }
    }

    fn run_cpu(&mut self, which: usize, cycles: u32) {
        if which == ARM9 {
            self.run_cpu_for::<ARM9>(cycles);
        } else {
            self.run_cpu_for::<ARM7>(cycles);
        }
    }

    /// Run one CPU for about `cycles` bus cycles. Monomorphised per CPU so the checks
    /// that depend on which one it is cost nothing.
    #[inline(always)]
    fn run_cpu_for<const WHICH: usize>(&mut self, cycles: u32) {
        if self.break_hit {
            return;
        }
        let (cpu, wait) = if WHICH == ARM9 {
            (&mut self.arm9, &mut self.wait[0])
        } else {
            (&mut self.arm7, &mut self.wait[1])
        };
        let hw = &mut self.hw;
        let mut done = 0;
        let idle_skip = self.idle_skip;
        let eager_irq = self.eager_irq;
        let debug = self.trace_pcs || self.break9.is_some();
        let tracing_io = hw.trace_io;
        // Where the BIOS's vectors sit for this CPU.
        let vector_base = if WHICH == ARM9 { 0xFFFF_0000u32 } else { 0 };
        let jit = &mut self.jit;
        let use_jit = jit.enabled && !eager_irq && !debug && !tracing_io;
        let mut probe = idle::IdleProbe::new();
        let mut previous_pc = u32::MAX;
        // Interrupt lines only change when something is written (or read with a side
        // effect), so they are refreshed when the effects counter moves.
        let mut seen_effects = hw.effects.wrapping_sub(1);
        while done < cycles {
            let pc = cpu.regs[15];
            let last_pc = std::mem::replace(&mut previous_pc, pc);
            if debug {
                if self.trace_pcs {
                    let ring = &mut self.pc_trace[WHICH];
                    if ring.len() == 4000 {
                        ring.pop_front();
                    }
                    ring.push_back(pc);
                }
                if WHICH == ARM9 && self.break9 == Some(pc) {
                    self.break_hit = true;
                    return;
                }
            }
            if tracing_io {
                hw.cur_pc[WHICH] = pc;
            }
            if eager_irq || hw.effects != seen_effects {
                seen_effects = hw.effects;
                cpu.irq_line = hw.irq[WHICH].line();
                cpu.wake_line = hw.irq[WHICH].wake();
            }
            if cpu.halted && !cpu.wake_line {
                break;
            }
            if idle_skip && !cpu.irq_line {
                if let Some(period) = probe.check(&cpu.regs, cpu.cpsr, hw.effects, done, pc <= last_pc) {
                    // Spinning on state nothing here can change: skip whole trips round
                    // the loop, leaving the last one to run so the slice ends as it would.
                    let trips = (cycles.saturating_sub(done)).saturating_sub(1) / period;
                    let skipped = trips * period;
                    done += skipped;
                    hw.cycles[WHICH] += skipped as u64;
                }
            }
            if pc.wrapping_sub(vector_base) <= bios::VECTOR_SPAN {
                if let Some(cost) = bios::intercept(WHICH, cpu, hw, wait) {
                    hw.cycles[WHICH] += cost as u64;
                    done += cost;
                    // The BIOS may have changed interrupt state.
                    seen_effects = hw.effects.wrapping_sub(1);
                    continue;
                }
            }
            if use_jit {
                if !hw.code_hits.is_empty() || hw.code_flush {
                    jit.process(hw);
                }
                // Not when an interrupt is about to be taken.
                let irq_due = cpu.irq_line && cpu.cpsr & crate::cpu::FLAG_I == 0;
                if !cpu.halted && !irq_due {
                    let key = pc | (cpu.cpsr >> 5 & 1);
                    if let Some(block) = jit.lookup::<WHICH>(hw, key) {
                        let left = cycles - done;
                        let block = if block.len <= left { Some(block) } else { jit.lookup_short::<WHICH>(hw, key, left) };
                        if let Some(block) = block {
                            let ran = jit.run(block, cpu, hw);
                            hw.cycles[WHICH] += ran as u64;
                            done += ran;
                            if cpu.halted {
                                // Something in the block put the CPU to sleep; see what wakes it.
                                seen_effects = hw.effects.wrapping_sub(1);
                            }
                            continue;
                        }
                    }
                }
            }
            let cost = if WHICH == ARM9 {
                let cost = cpu.step(&mut Bus9(hw));
                if cpu.cp15_dirty {
                    cpu.cp15_dirty = false;
                    hw.sync_tcm(cpu);
                    seen_effects = hw.effects.wrapping_sub(1);
                }
                cost
            } else {
                cpu.step(&mut Bus7(hw))
            };
            hw.cycles[WHICH] += cost as u64;
            done += cost;
        }
    }
}
