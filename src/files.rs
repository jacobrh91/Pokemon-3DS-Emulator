//! Where a game's files live (beside the ROM) and how a console is started from them.

use std::path::{Path, PathBuf};

use ds_emulator::cartridge::Cartridge;
use ds_emulator::system::Nds;

pub fn state_path(rom_path: &str) -> PathBuf {
    Path::new(rom_path).with_extension("state")
}

pub fn save_path(rom_path: &str) -> PathBuf {
    Path::new(rom_path).with_extension("sav")
}

/// Load the save file that sits beside the ROM, if there is one.
fn load_save(nds: &mut Nds, rom_path: &str) {
    if let Ok(bytes) = std::fs::read(save_path(rom_path)) {
        nds.hw.backup.load(&bytes);
    }
}

/// A console booted from `cart`, with the game's save loaded.
pub fn new_console(cart: Cartridge, rom_path: &str) -> Nds {
    let mut nds = Nds::direct_boot(cart);
    // The JIT is exact, but this is the way to rule it out when something looks wrong.
    nds.jit.enabled = std::env::var_os("DS_NO_JIT").is_none();
    load_save(&mut nds, rom_path);
    nds
}
