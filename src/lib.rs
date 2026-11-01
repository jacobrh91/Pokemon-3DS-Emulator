//! A Nintendo DS emulator.
//!
//! * [`cartridge`]: the ROM and its header
//! * [`cpu`]: the ARM9 / ARM7 interpreter
//! * [`jit`]: native-code translation of hot ARM / Thumb code
//! * [`system`]: the console around the CPUs (memory, IO, timing, peripherals, BIOS)
//! * [`video`]: the 2D and 3D engines
//! * [`capture`]: PNG / WAV output for headless runs
//! * [`frontend`]: the window, input, audio output and dock

pub mod capture;
pub mod cartridge;
pub mod cpu;
pub mod frontend;
pub mod jit;
pub mod system;
pub mod video;
