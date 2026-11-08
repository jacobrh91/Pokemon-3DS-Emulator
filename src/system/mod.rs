//! The DS itself: two CPUs, their memory maps, and the hardware between them.
//!
//! * `memory`: address maps, TCM, shared WRAM, VRAM banks, and the CPUs' bus adapters
//! * `peripherals`: the cartridge slot and save chip, clock, firmware/touch SPI, Wi-Fi, IPC, sound
//! * `irq`, `timer`, `dma`: interrupts, timers and DMA
//! * `io`: the register map; `display`: picture rendering
//! * `scheduler`: `Nds`, the scanline/slice timing and the run loop
//! * `bios`: the simulated BIOS and direct boot; `state`: save states

mod bios;
mod display;
mod dma;
mod io;
mod irq;
pub mod memory;
mod peripherals;
mod scheduler;
mod state;
mod timer;

use std::collections::BTreeMap;

use crate::cpu::bus::Bus;
use crate::cartridge::Cartridge;
use crate::cpu::{Arch, Cpu};
use crate::video::gpu;
pub use memory::{Bus7, Bus9};
pub use peripherals::backup::Backup;
pub use scheduler::Nds;
use dma::{Dma, DmaTiming};
use peripherals::{backup, card, ipc::Ipc, rtc, sound, spi, wifi};
use irq::{Irq, IRQ_VBLANK, IRQ_VCOUNT};
use timer::Timer;
use memory::new_code_map;
use memory::vram::{VramMap, VRAM_SIZE};

pub const ARM9: usize = 0;
pub const ARM7: usize = 1;

pub const MAIN_RAM_SIZE: usize = 4 * 1024 * 1024;
/// Main RAM is tracked for writes to compiled code in pages of `1 << CODE_PAGE_SHIFT` bytes.
pub const CODE_PAGE_SHIFT: usize = 8;

const SHARED_WRAM_SIZE: usize = 32 * 1024;
const ARM7_WRAM_SIZE: usize = 64 * 1024;

#[derive(Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Video {
    pub vcount: u16,
    pub dispstat: [u16; 2],
    pub in_vblank: bool,
    pub in_hblank: bool,
}

/// Where compiled code finds the memory it reads and writes directly; set before each run.
#[derive(Default, Clone, Copy)]
pub struct FastPtrs {
    pub main_ram: usize,
    pub code_map: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Hardware {
    #[serde(skip)]
    pub fast: FastPtrs,
    #[serde(skip)]
    pub cart: Cartridge,
    pub main_ram: Vec<u8>,
    pub shared_wram: Vec<u8>,
    pub wram7: Vec<u8>,
    pub itcm: Vec<u8>,
    pub dtcm: Vec<u8>,
    pub palette: Vec<u8>,
    pub vram: Vec<u8>,
    pub oam: Vec<u8>,
    pub wramcnt: u8,
    pub vram_cnt: [u8; 9],
    pub vram_map: VramMap,
    /// ARM9 CP15 TCM region registers, copied after every ARM9 step.
    pub dtcm_reg: u32,
    pub itcm_reg: u32,
    pub tcm_enable: (bool, bool),
    /// Derived from the registers above by `refresh_tcm`: the ITCM window's end (0 when
    /// disabled), and the DTCM window's start and length (0 when disabled).
    #[serde(skip)]
    itcm_limit: u32,
    #[serde(skip)]
    pub(crate) dtcm_lo: u32,
    #[serde(skip)]
    pub(crate) dtcm_len: u32,
    pub irq: [Irq; 2],
    pub timers: [[Timer; 4]; 2],
    /// Bit `t` set when timer `t` of that CPU is running; derived from `timers`.
    #[serde(skip)]
    pub(crate) timers_on: [u8; 2],
    /// Cycles run since each CPU's timers were last brought up to date, and how many may
    /// pass before one could overflow. Ticking in batches gives the same results.
    #[serde(skip)]
    pub(crate) timer_pending: [u32; 2],
    #[serde(skip)]
    pub(crate) timer_budget: [u32; 2],
    pub dma: [[Dma; 4]; 2],
    pub ipc: Ipc,
    pub card: card::Card,
    pub backup: backup::Backup,
    /// Present on cartridges with an infrared port in front of the save chip.
    pub infrared: Option<backup::InfraredChip>,
    pub spi: spi::Spi,
    pub video: Video,
    /// Pressed buttons, active high: A B Select Start Right Left Up Down R L X Y.
    pub keys: u16,
    /// Touched pixel on the bottom screen, if the pen is down.
    pub touch: Option<(u8, u8)>,
    /// Fast-forward: don't draw the 2D picture this frame.
    #[serde(skip)]
    pub skip_render: bool,
    pub rtc: rtc::Rtc,
    pub wifi: wifi::Wifi,
    pub sound: sound::Sound,
    pub gx: crate::video::gx::Gx,
    /// Top screen then bottom screen, 256x192 each, 0x00RRGGBB.
    #[serde(skip)]
    pub frame: Vec<u32>,
    /// Current reference point of BG2/BG3 per engine, in 8.8 fixed point.
    pub affine_cur: [[[i32; 2]; 2]; 2],
    pub io: [Vec<u8>; 2],
    #[serde(skip)]
    pub unhandled: BTreeMap<String, u32>,
    pub cycles: [u64; 2],
    /// Counts stores and side-effecting reads; the spin-loop detector watches it.
    #[serde(skip)]
    pub effects: u64,
    /// Counts accesses to the IO registers, which can change what the CPU should do next;
    /// compiled code leaves its block after one.
    #[serde(skip)]
    pub io_ops: u64,
    /// One flag per 256 bytes of main RAM that compiled code was made from.
    #[serde(skip, default = "new_code_map")]
    pub code_map: Vec<u8>,
    /// Pages of `code_map` written to since the compiled code was last checked.
    #[serde(skip)]
    pub code_hits: Vec<u32>,
    /// The CPU's view of memory changed under the compiled code (TCM moved).
    #[serde(skip)]
    pub code_flush: bool,
    /// Most recent register accesses (cpu, is_write, address, value), for debugging.
    #[serde(skip)]
    pub io_ring: std::collections::VecDeque<(u8, bool, u32, u32, u32)>,
    /// Address of the instruction each CPU is executing, for the register log.
    #[serde(skip)]
    pub cur_pc: [u32; 2],
    #[serde(skip)]
    pub trace_io: bool,
    /// Debug log of notable events (IPC words, card commands) when `trace_io` is on.
    #[serde(skip)]
    pub events: Vec<String>,
}

impl Hardware {    pub fn new(cart: Cartridge) -> Hardware {
        // HeartGold (IPK) and SoulSilver (IPG) have the infrared chip.
        let infrared = cart.header.game_code.starts_with("IP").then(backup::InfraredChip::default);
        Hardware {
            infrared,
            cart,
            fast: FastPtrs::default(),
            main_ram: vec![0; MAIN_RAM_SIZE],
            shared_wram: vec![0; SHARED_WRAM_SIZE],
            wram7: vec![0; ARM7_WRAM_SIZE],
            itcm: vec![0; 32 * 1024],
            dtcm: vec![0; 16 * 1024],
            palette: vec![0; 2048],
            vram: vec![0; VRAM_SIZE],
            oam: vec![0; 2048],
            wramcnt: 0,
            vram_cnt: [0; 9],
            vram_map: VramMap::new(),
            dtcm_reg: 0,
            itcm_reg: 0,
            tcm_enable: (false, false),
            itcm_limit: 0,
            dtcm_lo: 0,
            dtcm_len: 0,
            irq: [Irq::default(); 2],
            timers: [[Timer::default(); 4]; 2],
            timers_on: [0; 2],
            timer_pending: [0; 2],
            timer_budget: [0; 2],
            dma: [[Dma::default(); 4]; 2],
            ipc: Ipc::default(),
            card: card::Card::default(),
            backup: backup::Backup::new(512 * 1024),
            spi: spi::Spi::new(),
            video: Video::default(),
            keys: 0,
            touch: None,
            skip_render: false,
            rtc: rtc::Rtc::new(),
            wifi: wifi::Wifi::new(),
            sound: sound::Sound::new(),
            gx: crate::video::gx::Gx::new(),
            frame: vec![0; gpu::WIDTH * gpu::HEIGHT * 2],
            affine_cur: [[[0; 2]; 2]; 2],
            io: io::new_backing(),
            unhandled: BTreeMap::new(),
            cycles: [0; 2],
            effects: 0,
            io_ops: 0,
            code_map: new_code_map(),
            code_hits: Vec::new(),
            code_flush: false,
            io_ring: std::collections::VecDeque::new(),
            cur_pc: [0; 2],
            trace_io: false,
            events: Vec::new(),
        }
    }

    pub fn note(&mut self, what: String) {
        *self.unhandled.entry(what).or_insert(0) += 1;
    }

}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn test_hardware() -> Hardware {
        let mut rom = vec![0u8; 0x10000];
        let crc = crate::cartridge::crc16(&rom[..0x15E]);
        rom[0x15E..0x160].copy_from_slice(&crc.to_le_bytes());
        Hardware::new(Cartridge::from_bytes(rom).unwrap())
    }

    #[test]
    fn shared_wram_split_follows_wramcnt() {
        let mut hw = test_hardware();
        hw.wramcnt = 1; // ARM9 gets the upper half, ARM7 the lower
        hw.write(ARM9, 0x0300_0000, 4, 0x1234);
        hw.write(ARM7, 0x0300_0000, 4, 0x5678);
        assert_eq!(hw.read(ARM9, 0x0300_0000, 4), 0x1234);
        assert_eq!(hw.read(ARM7, 0x0300_0000, 4), 0x5678);
        hw.wramcnt = 3;
        assert_eq!(hw.read(ARM9, 0x0300_0000, 4), 0);
    }

    #[test]
    fn main_ram_is_mirrored() {
        let mut hw = test_hardware();
        hw.write(ARM9, 0x0200_0010, 4, 0xCAFE);
        assert_eq!(hw.read(ARM7, 0x0240_0010, 4), 0xCAFE);
    }

    #[test]
    fn vram_writes_follow_bank_mapping() {
        let mut hw = test_hardware();
        hw.io_write(ARM9, 0x0400_0240, 1, 0x81); // bank A -> BG-A
        hw.write(ARM9, 0x0600_0004, 4, 0xABCD);
        assert_eq!(hw.read(ARM9, 0x0680_0004, 4), 0xABCD);
        assert_eq!(hw.read(ARM9, 0x0620_0004, 4), 0);
    }
}

