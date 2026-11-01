//! The game's save data on disk: the backup chip, save states, restarting, and the flash log.

use std::path::Path;

use crate::system::Nds;

/// Frames between autosaves of the backup chip.
pub(super) const AUTOSAVE_FRAMES: u64 = 300;

/// Frames a "state saved/loaded" notice stays in the window title.
pub(super) const NOTICE_FRAMES: u64 = 120;

pub(super) fn save_if_dirty(nds: &mut Nds, save_path: Option<&Path>) {
    if let Some(path) = save_path {
        if nds.hw.backup.dirty {
            // Keep the previous file once per run, so a bad save can't destroy older progress.
            let backup = path.with_extension("sav.bak");
            if path.exists() && !backup.exists() {
                let _ = std::fs::copy(path, backup);
            }
            if let Err(error) = nds.hw.backup.save_to(path) {
                eprintln!("could not write {}: {error}", path.display());
            }
        }
    }
}

/// Write or restore a save state; returns the message to show.
pub(super) fn state_action(nds: &mut Nds, path: &Path, save: bool) -> String {
    if save {
        match std::fs::write(path, nds.save_state()) {
            Ok(()) => format!("State saved to {}", path.display()),
            Err(error) => format!("Could not save state: {error}"),
        }
    } else {
        match std::fs::read(path).map_err(|e| e.to_string()).and_then(|bytes| nds.load_state(&bytes)) {
            Ok(()) => {
                nds.take_audio();
                "State loaded".to_string()
            }
            Err(error) => format!("Could not load state: {error}"),
        }
    }
}

/// Start the game again from power-on, keeping the save in the backup chip as it is on disk.
pub(super) fn restart(nds: &mut Nds, save_path: Option<&Path>) {
    save_if_dirty(nds, save_path);
    let jit = nds.jit.enabled;
    *nds = Nds::direct_boot(std::mem::take(&mut nds.hw.cart));
    nds.jit.enabled = jit;
    if let Some(bytes) = save_path.and_then(|path| std::fs::read(path).ok()) {
        nds.hw.backup.load(&bytes);
    }
}

/// With `DS_FLASH_LOG=<file>` set, record every save-chip write/erase the game issues.
pub(super) fn write_flash_log(nds: &Nds) {
    if let (Ok(path), Some(log)) = (std::env::var("DS_FLASH_LOG"), &nds.hw.backup.command_log) {
        let _ = std::fs::write(path, log.join("\n"));
    }
}
