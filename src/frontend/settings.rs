//! The dock's volume, mute and speed, remembered between runs.

use std::path::Path;

use super::overlay::dock::{Dock, SpeedSetting};

/// Volume, mute and fast-forward speed, kept beside the save state.
pub(super) fn load_settings(path: &Path, dock: &mut Dock) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        match line.split_once('=') {
            Some(("volume", value)) => {
                if let Ok(volume) = value.parse::<f32>() {
                    *dock = Dock::new(volume);
                }
            }
            Some(("muted", value)) => dock.set_muted(value == "1"),
            Some(("speed", value)) => {
                if let Some(speed) = SpeedSetting::from_label(value) {
                    dock.set_speed(speed);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn save_settings(path: &Path, dock: &Dock) {
    let text = format!(
        "volume={:.2}\nmuted={}\nspeed={}\n",
        dock.volume(),
        dock.is_muted() as u8,
        dock.speed().label()
    );
    let _ = std::fs::write(path, text);
}
