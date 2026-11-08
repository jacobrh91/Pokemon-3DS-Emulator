//! Built-in cheats for Pokémon HeartGold and SoulSilver, which act on the game's own code the
//! way the Game Boy Advance emulator's do: each finds a routine by its bytes, and when the
//! ARM9 reaches it the emulator changes a register.
//!
//! | Cheat | Hooks | Changes |
//! |---|---|---|
//! | Wild Pokémon is *species* at level *n* | the two wild-encounter routines | `r0` (species), `r1` (level) at entry |
//! | Perfect stats, wild | `CreateMon` and its two variants, when the wild routines call them | `r3` (`fixed_iv`) |
//! | Perfect stats, opponents | `CreateBoxMon` for Pokémon with OT-ID type 2 | `r3` (`fixed_iv`) |
//!
//! # How the game makes a Pokémon
//!
//! `CreateBoxMon(boxmon, species, level, fixed_iv, has_fixed_personality, personality,
//! ot_id_type, fixed_ot_id)` gives every IV `fixed_iv` when that is below 32, and draws each at
//! random otherwise. `CreateMon` and two variants (one fixing the nature, one the personality)
//! pass their own `fixed_iv` on. Wild encounters pass 32 and trainers' Pokémon are made with
//! OT-ID type 2 (`OT_ID_RANDOM_NO_SHINY`), so storing 31 in `r3` at the right entry point makes
//! all six IVs 31.
//!
//! # Where the wild Pokémon are made
//!
//! Two routines in the field overlay build every wild encounter (grass, surfing, fishing, ...):
//! both take the species in `r0` and the level in `r1` and call one of the `CreateMon` variants
//! with `fixed_iv` 32. A `CreateMon` whose caller lies inside them is therefore a wild Pokémon.
//!
//! # Finding the code
//!
//! The overlay is only in memory while the game needs it and its address could differ between
//! builds, so the routines are found by scanning main RAM for their opening bytes (which hold
//! no `BL` or literal-pool load and so do not change), every [`SCAN_INTERVAL`] frames while one
//! is missing. A hook checks the bytes again when it fires, so code that replaced an overlay
//! is left alone.

pub mod species;

use crate::cpu::Cpu;
use crate::jit::Jit;
use crate::system::{ARM9, Hardware};

pub const MAX_LEVEL: u8 = 100;

/// The IV `CreateBoxMon` is given for a perfect Pokémon.
const PERFECT_IV: u32 = 31;
/// OT-ID type of the Pokémon in trainers' parties.
const OT_ID_RANDOM_NO_SHINY: u32 = 2;
/// Frames between scans for a routine that has not been found.
const SCAN_INTERVAL: u64 = 60;
/// How far into the second wild routine its `CreateMon` calls reach.
const WILD_SECOND_EXTENT: u32 = 0x150;
const RAM_BASE: u32 = 0x0200_0000;

/// An array of bytes to match: numbers match themselves and `_` any byte.
macro_rules! pattern {
    (@byte _) => { None };
    (@byte $byte:expr) => { Some($byte) };
    ($($byte:tt),+ $(,)?) => { &[$(pattern!(@byte $byte)),+] };
}

type Pattern = &'static [Option<u8>];

/// `CreateMon(mon, species, level, fixed_iv, ...)`.
const CREATE_MON: Pattern = pattern![0x0F, 0xB4, 0x78, 0xB5, 0x8B, 0xB0, 0x05, 0x1C, 0x0C, 0x1C, 0x1E, 0x1C];
/// The variant that fixes the nature.
const CREATE_MON_NATURE: Pattern = pattern![0xF8, 0xB5, 0x86, 0xB0, 0x04, 0x92, 0x05, 0x93, 0x0C, 0x9C, 0x06, 0x1C, 0x0F, 0x1C];
/// The variant that fixes the personality.
const CREATE_MON_PERSONALITY: Pattern = pattern![
    0xF0, 0xB5, 0x89, 0xB0, 0x05, 0x91, 0x06, 0x92, 0x07, 0x93, 0x04, 0x90, 0x0A, 0xA8, 0x00, 0x7E,
];
/// `CreateBoxMon`, which `CreateMon` calls.
const CREATE_BOX_MON: Pattern = pattern![0x0F, 0xB4, 0xF0, 0xB5, 0x83, 0xB0, 0x05, 0x1C, _, _, _, _, 0x28, 0x1C];
/// The wild-encounter routine that calls `CreateMon` itself.
const WILD_FIRST: Pattern = pattern![0xF8, 0xB5, 0x8A, 0xB0, 0x04, 0x90, 0x10, 0x9E, 0x05, 0x91, 0x0B, 0x20, 0x06, 0x92];
/// The one that calls the two variants.
const WILD_SECOND: Pattern = pattern![
    0xF8, 0xB5, 0x88, 0xB0, 0x03, 0x90, 0x0F, 0x98, 0x04, 0x91, 0x0F, 0x90, 0x0B, 0x20, 0x05, 0x92,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    WildFirst,
    WildSecond,
    CreateMon,
    CreateMonNature,
    CreateMonPersonality,
    CreateBoxMon,
}

impl Kind {
    const ALL: [Kind; 6] = [
        Kind::WildFirst,
        Kind::WildSecond,
        Kind::CreateMon,
        Kind::CreateMonNature,
        Kind::CreateMonPersonality,
        Kind::CreateBoxMon,
    ];

    fn pattern(self) -> Pattern {
        match self {
            Kind::WildFirst => WILD_FIRST,
            Kind::WildSecond => WILD_SECOND,
            Kind::CreateMon => CREATE_MON,
            Kind::CreateMonNature => CREATE_MON_NATURE,
            Kind::CreateMonPersonality => CREATE_MON_PERSONALITY,
            Kind::CreateBoxMon => CREATE_BOX_MON,
        }
    }

    fn is_create_mon(self) -> bool {
        matches!(self, Kind::CreateMon | Kind::CreateMonNature | Kind::CreateMonPersonality)
    }
}

/// Whose Pokémon the perfect-stats cheat applies to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Perfect {
    #[default]
    Off,
    Wild,
    Opponents,
    Both,
}

impl Perfect {
    /// The choices in the order the menu lists them.
    pub const ALL: [Perfect; 4] = [Self::Off, Self::Wild, Self::Opponents, Self::Both];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Wild => "WILD POKEMON",
            Self::Opponents => "OPPONENTS POKEMON",
            Self::Both => "BOTH",
        }
    }

    /// The word used for the choice in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Wild => "wild",
            Self::Opponents => "opponents",
            Self::Both => "both",
        }
    }

    pub fn from_key(key: &str) -> Option<Perfect> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }

    /// The step `delta` along [`Perfect::ALL`], wrapping around.
    pub fn stepped(self, delta: i32) -> Perfect {
        let at = Self::ALL.iter().position(|choice| *choice == self).unwrap_or(0) as i32;
        Self::ALL[(at + delta).rem_euclid(Self::ALL.len() as i32) as usize]
    }

    pub fn covers_wild(self) -> bool {
        matches!(self, Self::Wild | Self::Both)
    }

    pub fn covers_opponents(self) -> bool {
        matches!(self, Self::Opponents | Self::Both)
    }
}

/// The Pokémon every wild encounter is turned into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WildMon {
    pub species: u16,
    pub level: u8,
}

impl WildMon {
    /// A choice, kept within what the game has.
    pub fn new(species: u16, level: u8) -> WildMon {
        WildMon { species: species.clamp(1, species::COUNT), level: level.clamp(1, MAX_LEVEL) }
    }
}

impl Default for WildMon {
    fn default() -> WildMon {
        WildMon::new(25, 5)
    }
}

pub struct Cheats {
    pub wild: Option<WildMon>,
    pub perfect: Perfect,
    /// Where each routine was last found.
    found: [Option<u32>; Kind::ALL.len()],
    next_scan: u64,
}

impl Default for Cheats {
    fn default() -> Cheats {
        Cheats { wild: None, perfect: Perfect::Off, found: [None; Kind::ALL.len()], next_scan: 0 }
    }
}

/// Whether `pattern` matches `code` from its start.
fn matches(pattern: Pattern, code: &[u8]) -> bool {
    code.len() >= pattern.len() && pattern.iter().zip(code).all(|(want, byte)| want.is_none_or(|want| want == *byte))
}

/// The address of every halfword-aligned place in `ram` (which starts at `RAM_BASE`) where
/// `pattern` matches.
fn find_all(ram: &[u8], pattern: Pattern) -> Vec<u32> {
    let Some(last) = ram.len().checked_sub(pattern.len()) else { return Vec::new() };
    let first = pattern[0];
    (0..=last)
        .step_by(2)
        .filter(|&at| first.is_none_or(|first| ram[at] == first))
        .filter(|&at| matches(pattern, &ram[at..]))
        .map(|at| RAM_BASE + at as u32)
        .collect()
}

impl Cheats {
    pub fn new(wild: Option<WildMon>, perfect: Perfect) -> Cheats {
        Cheats { wild, perfect, ..Cheats::default() }
    }

    pub fn is_active(&self) -> bool {
        self.wild.is_some() || self.perfect != Perfect::Off
    }

    fn address(&self, kind: Kind) -> Option<u32> {
        self.found[kind as usize]
    }

    /// The bytes of main RAM at `pc`.
    fn code_at(hw: &Hardware, pc: u32) -> &[u8] {
        hw.main_ram.get((pc.wrapping_sub(RAM_BASE)) as usize..).unwrap_or(&[])
    }

    /// Keep the hooks in step with the options and with what is in memory. Called once a frame.
    pub fn refresh(&mut self, frame: u64, jit: &mut Jit, hw: &mut Hardware) {
        if !self.is_active() {
            jit.set_hooks(Vec::new(), hw);
            return;
        }
        for kind in Kind::ALL {
            // Code that was found and has since been replaced (an overlay swapped out).
            if let Some(pc) = self.address(kind) {
                if !matches(kind.pattern(), Self::code_at(hw, pc)) {
                    self.found[kind as usize] = None;
                }
            }
        }
        if self.wanted().any(|kind| self.address(kind).is_none()) && frame >= self.next_scan {
            self.next_scan = frame + SCAN_INTERVAL;
            self.scan(hw);
        }
        let hooks = self.hooked().filter_map(|kind| self.address(kind)).collect();
        jit.set_hooks(hooks, hw);
    }

    /// The routines that are hooked: the wild routines' arguments are only changed for the
    /// species cheat, and the wild `CreateMon` calls are only recognised once the wild code is
    /// known.
    fn hooked(&self) -> impl Iterator<Item = Kind> + '_ {
        let wild_code = self.wild_code().is_some();
        Kind::ALL.into_iter().filter(move |kind| match kind {
            Kind::WildFirst | Kind::WildSecond => self.wild.is_some(),
            Kind::CreateBoxMon => self.perfect.covers_opponents(),
            _ => self.perfect.covers_wild() && wild_code,
        })
    }

    /// The routines the options need found, in order.
    fn wanted(&self) -> impl Iterator<Item = Kind> + '_ {
        Kind::ALL.into_iter().filter(|kind| match kind {
            Kind::WildFirst | Kind::WildSecond => self.wild.is_some() || self.perfect.covers_wild(),
            Kind::CreateBoxMon => self.perfect.covers_opponents(),
            _ => self.perfect.covers_wild(),
        })
    }

    /// Look for the routines that are missing. A routine is only taken if exactly one place in
    /// memory matches, as in the Game Boy Advance cheats.
    fn scan(&mut self, hw: &Hardware) {
        for kind in Kind::ALL {
            if self.address(kind).is_none() {
                if let [only] = find_all(&hw.main_ram, kind.pattern()).as_slice() {
                    self.found[kind as usize] = Some(*only);
                }
            }
        }
    }

    /// The range of memory the two wild routines occupy.
    fn wild_code(&self) -> Option<std::ops::Range<u32>> {
        let first = self.address(Kind::WildFirst)?;
        let second = self.address(Kind::WildSecond)?;
        Some(first.min(second)..first.max(second) + WILD_SECOND_EXTENT)
    }

    /// The ARM9 reached a hooked address: change what the game is about to do.
    pub fn fire(&self, pc: u32, cpu: &mut Cpu, hw: &mut Hardware) {
        let Some(kind) = Kind::ALL.into_iter().find(|&kind| self.address(kind) == Some(pc)) else { return };
        if !matches(kind.pattern(), Self::code_at(hw, pc)) {
            return;
        }
        match kind {
            Kind::WildFirst | Kind::WildSecond => {
                if let Some(wild) = self.wild {
                    cpu.regs[0] = u32::from(wild.species);
                    cpu.regs[1] = u32::from(wild.level);
                }
            }
            Kind::CreateBoxMon => {
                // The stack holds the arguments from the fifth: has_fixed_personality,
                // personality, ot_id_type.
                let ot_id_type = hw.read(ARM9, cpu.regs[13].wrapping_add(8), 4);
                if self.perfect.covers_opponents() && ot_id_type == OT_ID_RANDOM_NO_SHINY {
                    cpu.regs[3] = PERFECT_IV;
                }
            }
            kind if kind.is_create_mon() => {
                let called_by_wild_code = self.wild_code().is_some_and(|code| code.contains(&cpu.regs[14]));
                if self.perfect.covers_wild() && called_by_wild_code {
                    cpu.regs[3] = PERFECT_IV;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
