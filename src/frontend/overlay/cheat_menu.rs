//! The cheat menu: a panel over the top screen, opened with Tab, for the wild-Pokémon cheat
//! (species and level) and the perfect-stats cheat. Rows are picked with Up and Down and
//! changed with Left and Right.

use super::canvas::{Canvas, Rect, text_width};
use crate::cheats::{Cheats, MAX_LEVEL, WildMon, species};

const PANEL_COLOR: u32 = 0x0020_2428;
const PANEL_ALPHA: f32 = 0.92;
const SELECTED_COLOR: u32 = 0x0046_B4FF;
const TEXT_COLOR: u32 = 0x00F4_F6F8;
const DIM_COLOR: u32 = 0x0090_9498;
const SELECTED_TEXT_COLOR: u32 = 0x0010_1418;

const MARGIN: i32 = 12;
const ROW_HEIGHT: i32 = 30;
const PANEL_WIDTH: i32 = 480;
/// Space above the hints, and the height of each of their lines.
const HINT_GAP: i32 = 6;
const HINT_LINE: i32 = 18;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Wild,
    Species,
    Level,
    Perfect,
}

impl Row {
    const ALL: [Row; 4] = [Row::Wild, Row::Species, Row::Level, Row::Perfect];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKey {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Toggle,
}

pub struct CheatMenu {
    pub open: bool,
    row: usize,
    /// The species and level that are used while the wild-Pokémon cheat is on, and remembered
    /// while it is off.
    choice: WildMon,
    wild_on: bool,
}

impl Default for CheatMenu {
    fn default() -> CheatMenu {
        CheatMenu { open: false, row: 0, choice: WildMon::default(), wild_on: false }
    }
}

impl CheatMenu {
    /// A menu showing what `cheats` already has set.
    pub fn new(cheats: &Cheats) -> CheatMenu {
        let mut menu = CheatMenu::default();
        menu.adopt(cheats);
        menu
    }

    /// Take the wild-Pokémon setting of `cheats` (command-line flags, say) as the menu's.
    pub fn adopt(&mut self, cheats: &Cheats) {
        if let Some(wild) = cheats.wild {
            self.choice = wild;
        }
        self.wild_on = cheats.wild.is_some();
    }

    pub fn choice(&self) -> WildMon {
        self.choice
    }

    pub fn wild_on(&self) -> bool {
        self.wild_on
    }

    /// Set the remembered choice and whether it is on (from the settings file).
    pub fn set(&mut self, choice: WildMon, on: bool, cheats: &mut Cheats) {
        self.choice = choice;
        self.wild_on = on;
        self.sync(cheats);
    }

    pub fn selected(&self) -> Row {
        Row::ALL[self.row]
    }

    fn sync(&self, cheats: &mut Cheats) {
        cheats.wild = self.wild_on.then_some(self.choice);
    }

    /// Act on a key; returns whether a setting changed.
    pub fn press(&mut self, key: MenuKey, cheats: &mut Cheats) -> bool {
        let rows = Row::ALL.len();
        let (step, big) = match key {
            MenuKey::Up => {
                self.row = (self.row + rows - 1) % rows;
                return false;
            }
            MenuKey::Down => {
                self.row = (self.row + 1) % rows;
                return false;
            }
            MenuKey::Left => (-1, false),
            MenuKey::Right | MenuKey::Toggle => (1, false),
            MenuKey::PageUp => (1, true),
            MenuKey::PageDown => (-1, true),
        };
        let amount = if big { 10 } else { 1 };
        match self.selected() {
            Row::Wild => self.wild_on = !self.wild_on,
            Row::Species => {
                let count = i32::from(species::COUNT);
                let next = (i32::from(self.choice.species) - 1 + step * amount).rem_euclid(count) + 1;
                self.choice = WildMon::new(next as u16, self.choice.level);
            }
            Row::Level => {
                let next = (i32::from(self.choice.level) + step * amount).clamp(1, i32::from(MAX_LEVEL));
                self.choice = WildMon::new(self.choice.species, next as u8);
            }
            Row::Perfect => cheats.perfect = cheats.perfect.stepped(step),
        }
        self.sync(cheats);
        true
    }

    /// The text of each row: its label and its value.
    fn lines(&self, cheats: &Cheats) -> [(&'static str, String); 4] {
        let name = species::name(self.choice.species).unwrap_or("").to_uppercase();
        [
            ("WILD POKEMON CHEAT", if self.wild_on { "ON" } else { "OFF" }.to_string()),
            ("SPECIES", format!("{} {}", self.choice.species, name)),
            ("LEVEL", self.choice.level.to_string()),
            ("PERFECT STATS", cheats.perfect.label().to_string()),
        ]
    }

    /// Where the panel and its rows sit in a buffer of `width` x `height`.
    fn panel(&self, width: usize) -> (Rect, i32) {
        let x = (width as i32 - PANEL_WIDTH) / 2;
        let top = MARGIN + 34;
        let height = top + ROW_HEIGHT * Row::ALL.len() as i32 + HINT_GAP + 2 * HINT_LINE + MARGIN - MARGIN;
        (Rect { x, y: MARGIN, width: PANEL_WIDTH, height }, top)
    }

    pub fn draw(&self, buffer: &mut [u32], width: usize, height: usize, cheats: &Cheats) {
        if !self.open {
            return;
        }
        let mut canvas = Canvas::new(buffer, width, height, 0);
        let (panel, top) = self.panel(width);
        canvas.rounded_rect(panel, 12, PANEL_COLOR, PANEL_ALPHA);
        canvas.text("CHEATS", panel.x + MARGIN, panel.y + 10, TEXT_COLOR);
        let hint = "TAB CLOSES";
        canvas.text(hint, panel.x + panel.width - MARGIN - text_width(hint), panel.y + 10, DIM_COLOR);
        for (index, (label, value)) in self.lines(cheats).iter().enumerate() {
            let selected = index == self.row;
            let y = top + index as i32 * ROW_HEIGHT;
            if selected {
                canvas.rounded_rect(Rect { x: panel.x + 6, y, width: panel.width - 12, height: ROW_HEIGHT - 4 }, 6, SELECTED_COLOR, 1.0);
            }
            let color = if selected { SELECTED_TEXT_COLOR } else { TEXT_COLOR };
            canvas.text(label, panel.x + MARGIN, y + 7, color);
            canvas.text(value, panel.x + panel.width - MARGIN - text_width(value), y + 7, color);
        }
        let hint_y = top + Row::ALL.len() as i32 * ROW_HEIGHT + HINT_GAP;
        canvas.text("UP DOWN PICK  LEFT RIGHT CHANGE", panel.x + MARGIN, hint_y, DIM_COLOR);
        canvas.text("PAGE UP DOWN CHANGE BY 10", panel.x + MARGIN, hint_y + HINT_LINE, DIM_COLOR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cheats::Perfect;

    fn press(menu: &mut CheatMenu, cheats: &mut Cheats, keys: &[MenuKey]) {
        for &key in keys {
            menu.press(key, cheats);
        }
    }

    #[test]
    fn the_wild_row_switches_the_cheat_on_and_off() {
        let mut cheats = Cheats::default();
        let mut menu = CheatMenu::default();
        assert!(menu.press(MenuKey::Toggle, &mut cheats));
        assert_eq!(cheats.wild, Some(WildMon::default()));
        menu.press(MenuKey::Left, &mut cheats);
        assert_eq!(cheats.wild, None);
        assert_eq!(menu.choice(), WildMon::default(), "the choice is kept while it is off");
    }

    #[test]
    fn species_and_level_change_a_cheat_that_is_on() {
        let mut cheats = Cheats::default();
        let mut menu = CheatMenu::default();
        press(&mut menu, &mut cheats, &[MenuKey::Toggle, MenuKey::Down, MenuKey::PageUp, MenuKey::Right]);
        assert_eq!(cheats.wild.map(|w| w.species), Some(25 + 10 + 1));
        press(&mut menu, &mut cheats, &[MenuKey::Down, MenuKey::PageUp, MenuKey::PageUp, MenuKey::Left]);
        assert_eq!(cheats.wild.map(|w| w.level), Some(24));
    }

    #[test]
    fn species_wrap_round_and_level_stops_at_the_ends() {
        let mut cheats = Cheats::default();
        let mut menu = CheatMenu::default();
        press(&mut menu, &mut cheats, &[MenuKey::Down]);
        menu.choice = WildMon::new(1, 5);
        menu.press(MenuKey::Left, &mut cheats);
        assert_eq!(menu.choice().species, species::COUNT);
        menu.press(MenuKey::Right, &mut cheats);
        assert_eq!(menu.choice().species, 1);
        press(&mut menu, &mut cheats, &[MenuKey::Down]);
        for _ in 0..12 {
            menu.press(MenuKey::PageUp, &mut cheats);
        }
        assert_eq!(menu.choice().level, MAX_LEVEL);
        for _ in 0..12 {
            menu.press(MenuKey::PageDown, &mut cheats);
        }
        assert_eq!(menu.choice().level, 1);
    }

    #[test]
    fn the_perfect_row_steps_through_the_choices() {
        let mut cheats = Cheats::default();
        let mut menu = CheatMenu::default();
        press(&mut menu, &mut cheats, &[MenuKey::Up]);
        assert_eq!(menu.selected(), Row::Perfect);
        menu.press(MenuKey::Right, &mut cheats);
        assert_eq!(cheats.perfect, Perfect::Wild);
        menu.press(MenuKey::Toggle, &mut cheats);
        assert_eq!(cheats.perfect, Perfect::Opponents);
        menu.press(MenuKey::Left, &mut cheats);
        menu.press(MenuKey::Left, &mut cheats);
        assert_eq!(cheats.perfect, Perfect::Off);
    }

    #[test]
    fn moving_between_rows_changes_nothing() {
        let mut cheats = Cheats::default();
        let mut menu = CheatMenu::default();
        assert!(!menu.press(MenuKey::Down, &mut cheats));
        assert!(!menu.press(MenuKey::Up, &mut cheats));
        assert!(!cheats.is_active());
    }

    #[test]
    fn a_menu_shows_what_the_flags_set() {
        let cheats = Cheats::new(Some(WildMon::new(143, 50)), Perfect::Off);
        let menu = CheatMenu::new(&cheats);
        assert_eq!((menu.choice(), menu.wild_on()), (WildMon::new(143, 50), true));
    }

    #[test]
    fn drawing_paints_only_while_open() {
        let (width, height) = (512, 768);
        let cheats = Cheats::default();
        let mut menu = CheatMenu::default();
        let mut buffer = vec![0u32; width * height];
        menu.draw(&mut buffer, width, height, &cheats);
        assert!(buffer.iter().all(|&p| p == 0));
        menu.open = true;
        menu.draw(&mut buffer, width, height, &cheats);
        assert!(buffer.iter().any(|&p| p != 0));
    }
}
