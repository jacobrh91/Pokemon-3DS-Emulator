//! The hardware around the CPUs that isn't memory: the cartridge slot and its save chip, the
//! real-time clock, the SPI bus (firmware, power, touch screen), Wi-Fi, the CPU-to-CPU IPC
//! FIFOs, and the sound mixer.

pub mod backup;
pub mod card;
pub mod ipc;
pub mod rtc;
pub mod sound;
pub mod spi;
pub mod wifi;
