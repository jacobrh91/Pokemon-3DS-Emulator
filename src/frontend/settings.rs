//! The dock's volume, mute and speed and the cheat menu's choices, remembered between runs.

use std::path::Path;

use super::overlay::cheat_menu::CheatMenu;
use super::overlay::dock::{Dock, SpeedSetting};
use crate::cheats::{Cheats, Perfect, WildMon};

/// Volume, mute and fast-forward speed, and the cheat choices (unless command-line flags
/// already set them), kept beside the save state.
pub(super) fn load_settings(path: &Path, dock: &mut Dock, menu: &mut CheatMenu, cheats: &mut Cheats) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let use_saved_cheats = !cheats.is_active();
    let (mut choice, mut wild_on) = (menu.choice(), menu.wild_on());
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
            Some(("wild", value)) => {
                if let Some(parsed) = value.split_once('@').and_then(|(species, level)| Some(WildMon::new(species.parse().ok()?, level.parse().ok()?))) {
                    choice = parsed;
                }
            }
            Some(("wild_on", value)) => wild_on = value == "1",
            Some(("perfect", value)) => {
                if let (true, Some(perfect)) = (use_saved_cheats, Perfect::from_key(value)) {
                    cheats.perfect = perfect;
                }
            }
            _ => {}
        }
    }
    if use_saved_cheats {
        menu.set(choice, wild_on, cheats);
    }
}

pub(super) fn save_settings(path: &Path, dock: &Dock, menu: &CheatMenu, cheats: &Cheats) {
    let text = format!(
        "volume={:.2}\nmuted={}\nspeed={}\nwild={}@{}\nwild_on={}\nperfect={}\n",
        dock.volume(),
        dock.is_muted() as u8,
        dock.speed().label(),
        menu.choice().species,
        menu.choice().level,
        menu.wild_on() as u8,
        cheats.perfect.key()
    );
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ds_emulator_{name}_{}.settings", std::process::id()))
    }

    #[test]
    fn cheat_choices_survive_a_restart() {
        let path = temp_file("roundtrip");
        let mut cheats = Cheats::new(None, Perfect::Both);
        let mut menu = CheatMenu::default();
        menu.set(WildMon::new(143, 50), true, &mut cheats);
        save_settings(&path, &Dock::new(0.5), &menu, &cheats);

        let (mut dock, mut loaded_menu, mut loaded) = (Dock::new(1.0), CheatMenu::default(), Cheats::default());
        load_settings(&path, &mut dock, &mut loaded_menu, &mut loaded);
        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.wild, Some(WildMon::new(143, 50)));
        assert_eq!(loaded.perfect, Perfect::Both);
        assert_eq!(loaded_menu.choice(), WildMon::new(143, 50));
    }

    #[test]
    fn the_choice_is_remembered_while_the_cheat_is_off() {
        let path = temp_file("off");
        let mut cheats = Cheats::default();
        let mut menu = CheatMenu::default();
        menu.set(WildMon::new(150, 70), false, &mut cheats);
        save_settings(&path, &Dock::new(1.0), &menu, &cheats);

        let (mut dock, mut loaded_menu, mut loaded) = (Dock::new(1.0), CheatMenu::default(), Cheats::default());
        load_settings(&path, &mut dock, &mut loaded_menu, &mut loaded);
        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.wild, None);
        assert_eq!(loaded_menu.choice(), WildMon::new(150, 70));
    }

    #[test]
    fn command_line_flags_win_over_the_saved_choices() {
        let path = temp_file("flags");
        let mut cheats = Cheats::default();
        let mut menu = CheatMenu::default();
        menu.set(WildMon::new(143, 50), true, &mut cheats);
        save_settings(&path, &Dock::new(1.0), &menu, &Cheats::new(cheats.wild, Perfect::Wild));

        let mut flags = Cheats::new(Some(WildMon::new(6, 36)), Perfect::Off);
        let (mut dock, mut loaded_menu) = (Dock::new(1.0), CheatMenu::new(&flags));
        load_settings(&path, &mut dock, &mut loaded_menu, &mut flags);
        std::fs::remove_file(&path).ok();
        assert_eq!(flags.wild, Some(WildMon::new(6, 36)));
        assert_eq!(flags.perfect, Perfect::Off);
    }
}
