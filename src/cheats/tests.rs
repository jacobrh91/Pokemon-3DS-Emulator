use super::*;
use crate::cpu::Arch;
use crate::system::tests::test_hardware;

const WILD_FIRST_AT: u32 = 0x0224_7910;
const WILD_SECOND_AT: u32 = 0x0224_7A18;
const CREATE_MON_AT: u32 = 0x0206_DE38;
const NATURE_AT: u32 = 0x0206_E108;
const PERSONALITY_AT: u32 = 0x0206_E14C;
const BOX_AT: u32 = 0x0206_DED0;

/// Copy `pattern` into memory at `at`, filling wildcards with `fill`.
fn place(hw: &mut Hardware, at: u32, pattern: Pattern, fill: u8) {
    for (i, byte) in pattern.iter().enumerate() {
        hw.main_ram[(at - RAM_BASE) as usize + i] = byte.unwrap_or(fill);
    }
}

fn game_with_all_routines() -> Hardware {
    let mut hw = test_hardware();
    for (at, kind) in [
        (WILD_FIRST_AT, Kind::WildFirst),
        (WILD_SECOND_AT, Kind::WildSecond),
        (CREATE_MON_AT, Kind::CreateMon),
        (NATURE_AT, Kind::CreateMonNature),
        (PERSONALITY_AT, Kind::CreateMonPersonality),
        (BOX_AT, Kind::CreateBoxMon),
    ] {
        place(&mut hw, at, kind.pattern(), 0x55);
    }
    hw
}

fn cpu() -> Cpu {
    Cpu::new(Arch::V5TE)
}

fn refreshed(cheats: &mut Cheats, hw: &mut Hardware) -> Jit {
    let mut jit = Jit::default();
    cheats.refresh(0, &mut jit, hw);
    jit
}

#[test]
fn patterns_match_exact_bytes_and_wildcards() {
    let pattern = pattern![0x10, _, 0x30];
    assert!(matches(pattern, &[0x10, 0x99, 0x30, 0x77]));
    assert!(!matches(pattern, &[0x10, 0x99, 0x31]));
    assert!(!matches(pattern, &[0x10, 0x99]), "too short");
}

#[test]
fn only_halfword_aligned_places_count() {
    let mut ram = vec![0u8; 64];
    ram[8..11].copy_from_slice(&[1, 2, 3]);
    ram[21..24].copy_from_slice(&[1, 2, 3]);
    assert_eq!(find_all(&ram, pattern![1, 2, 3]), vec![RAM_BASE + 8]);
}

#[test]
fn the_routines_are_found_where_they_are() {
    let mut hw = game_with_all_routines();
    let mut cheats = Cheats { wild: Some(WildMon::new(143, 50)), perfect: Perfect::Both, ..Cheats::default() };
    cheats.scan(&hw);
    for kind in Kind::ALL {
        assert!(cheats.address(kind).is_some(), "{kind:?}");
    }
    assert_eq!(cheats.address(Kind::WildSecond), Some(WILD_SECOND_AT));
    // A second copy makes a place ambiguous, so it is not taken.
    place(&mut hw, 0x0230_0000, CREATE_BOX_MON, 0);
    let mut again = Cheats::default();
    again.scan(&hw);
    assert_eq!(again.address(Kind::CreateBoxMon), None);
}

#[test]
fn species_and_level_are_kept_inside_what_the_game_has() {
    assert_eq!(WildMon::new(0, 0), WildMon { species: 1, level: 1 });
    assert_eq!(WildMon::new(900, 200), WildMon { species: species::COUNT, level: MAX_LEVEL });
    assert_eq!(WildMon::new(143, 50), WildMon { species: 143, level: 50 });
}

#[test]
fn perfect_choices_step_round_and_round() {
    assert_eq!(Perfect::Off.stepped(1), Perfect::Wild);
    assert_eq!(Perfect::Both.stepped(1), Perfect::Off);
    assert_eq!(Perfect::Off.stepped(-1), Perfect::Both);
    for choice in Perfect::ALL {
        assert_eq!(Perfect::from_key(choice.key()), Some(choice));
    }
    assert_eq!(Perfect::from_key("nobody"), None);
}

#[test]
fn nothing_is_hooked_while_every_cheat_is_off() {
    let mut hw = game_with_all_routines();
    let mut cheats = Cheats::default();
    assert!(refreshed(&mut cheats, &mut hw).hooks.is_empty());
}

#[test]
fn each_option_hooks_only_what_it_needs() {
    let mut hw = game_with_all_routines();

    let mut species_only = Cheats { wild: Some(WildMon::default()), ..Cheats::default() };
    assert_eq!(refreshed(&mut species_only, &mut hw).hooks, [WILD_FIRST_AT, WILD_SECOND_AT]);

    let mut opponents = Cheats { perfect: Perfect::Opponents, ..Cheats::default() };
    assert_eq!(refreshed(&mut opponents, &mut hw).hooks, [BOX_AT]);

    let mut wild = Cheats { perfect: Perfect::Wild, ..Cheats::default() };
    assert_eq!(refreshed(&mut wild, &mut hw).hooks, [CREATE_MON_AT, NATURE_AT, PERSONALITY_AT]);
}

#[test]
fn the_wild_routines_set_species_and_level() {
    let mut hw = game_with_all_routines();
    let mut cheats = Cheats { wild: Some(WildMon::new(143, 50)), ..Cheats::default() };
    refreshed(&mut cheats, &mut hw);
    for at in [WILD_FIRST_AT, WILD_SECOND_AT] {
        let mut cpu = cpu();
        cpu.regs[0] = 13;
        cpu.regs[1] = 3;
        cheats.fire(at, &mut cpu, &mut hw);
        assert_eq!((cpu.regs[0], cpu.regs[1]), (143, 50));
    }
}

#[test]
fn only_create_mon_called_from_the_wild_routines_gets_perfect_ivs() {
    let mut hw = game_with_all_routines();
    let mut cheats = Cheats { perfect: Perfect::Wild, ..Cheats::default() };
    refreshed(&mut cheats, &mut hw);
    let mut call = |lr: u32, at: u32| {
        let mut cpu = cpu();
        cpu.regs[3] = 32;
        cpu.regs[14] = lr;
        cheats.fire(at, &mut cpu, &mut hw);
        cpu.regs[3]
    };
    assert_eq!(call(WILD_FIRST_AT + 0xEA, CREATE_MON_AT), 31);
    assert_eq!(call(WILD_SECOND_AT + 0x94, PERSONALITY_AT), 31);
    assert_eq!(call(WILD_SECOND_AT + 0xDE, NATURE_AT), 31);
    assert_eq!(call(0x0220_225A, CREATE_MON_AT), 32, "a gift or an egg");
    assert_eq!(call(WILD_SECOND_AT + WILD_SECOND_EXTENT, CREATE_MON_AT), 32, "past the wild code");
}

#[test]
fn opponents_are_recognised_by_their_ot_id_type() {
    let mut hw = game_with_all_routines();
    let mut cheats = Cheats { perfect: Perfect::Opponents, ..Cheats::default() };
    refreshed(&mut cheats, &mut hw);
    for (ot_id_type, expected) in [(2, 31), (0, 32), (1, 32)] {
        let mut cpu = cpu();
        cpu.regs[13] = 0x0230_0000;
        hw.write(ARM9, 0x0230_0008, 4, ot_id_type);
        cpu.regs[3] = 32;
        cheats.fire(BOX_AT, &mut cpu, &mut hw);
        assert_eq!(cpu.regs[3], expected, "OT-ID type {ot_id_type}");
    }
}

#[test]
fn code_that_replaced_an_overlay_is_left_alone() {
    let mut hw = game_with_all_routines();
    let mut cheats = Cheats { wild: Some(WildMon::new(143, 50)), ..Cheats::default() };
    let mut jit = refreshed(&mut cheats, &mut hw);
    // Another overlay is loaded over the wild routines.
    hw.main_ram[(WILD_FIRST_AT - RAM_BASE) as usize..][..16].fill(0);
    let mut cpu = cpu();
    cpu.regs[0] = 13;
    cheats.fire(WILD_FIRST_AT, &mut cpu, &mut hw);
    assert_eq!(cpu.regs[0], 13);
    // And the next refresh drops the hook until the routine is found again.
    cheats.refresh(1, &mut jit, &mut hw);
    assert_eq!(jit.hooks, [WILD_SECOND_AT]);
}

#[test]
fn turning_a_cheat_off_removes_its_hooks() {
    let mut hw = game_with_all_routines();
    let mut cheats = Cheats { wild: Some(WildMon::default()), ..Cheats::default() };
    let mut jit = refreshed(&mut cheats, &mut hw);
    assert!(!jit.hooks.is_empty());
    cheats.wild = None;
    cheats.refresh(1, &mut jit, &mut hw);
    assert!(jit.hooks.is_empty());
}

fn wild_create_mon(cheats: &Cheats, hw: &mut Hardware, personality: u32, ot_id: u32, species: u32) -> u32 {
    let mut cpu = cpu();
    cpu.regs[13] = 0x0210_0000;
    cpu.regs[14] = WILD_FIRST_AT + 0x40;
    cpu.regs[1] = species;
    for (at, value) in [(0, 1), (4, personality), (8, OT_ID_PRESET), (12, ot_id)] {
        hw.write(ARM9, cpu.regs[13] + at, 4, value);
    }
    cheats.fire(CREATE_MON_AT, &mut cpu, hw);
    hw.read(ARM9, cpu.regs[13] + 4, 4)
}

#[test]
fn shiny_personalities_keep_the_nature_and_low_byte() {
    for (personality, ot_id) in [(0x792D_497E, 0xD5E9_60F3), (0x1234_5678, 0), (0xFFFF_FFFF, 0xABCD_EF01), (0x1111_0000, 0xFFFF_0000)] {
        let shiny = shiny_personality(personality, ot_id).expect("not shiny yet");
        assert!(is_shiny(shiny, ot_id), "{shiny:#x} for {ot_id:#x}");
        assert_eq!(shiny % 25, personality % 25);
        assert_eq!(shiny & 0xFF, personality & 0xFF);
    }
    let shiny = shiny_personality(0x792D_497E, 0xD5E9_60F3).unwrap();
    assert_eq!(shiny_personality(shiny, 0xD5E9_60F3), None, "already shiny");
}

#[test]
fn wild_pokemon_get_a_shiny_personality() {
    let mut hw = game_with_all_routines();
    let mut cheats = Cheats::default();
    cheats.shiny = true;
    let _jit = refreshed(&mut cheats, &mut hw);
    let ot_id = 0xD5E9_60F3;
    let personality = wild_create_mon(&cheats, &mut hw, 0x792D_497E, ot_id, 143);
    assert!(is_shiny(personality, ot_id));
    assert_eq!(personality % 25, 0x792D_497E % 25);
    assert_eq!(wild_create_mon(&cheats, &mut hw, 0x792D_497E, ot_id, UNOWN), 0x792D_497E, "Unown keeps its letter");
}

#[test]
fn shiny_is_left_off_unless_asked_for_and_only_for_wild_callers() {
    let mut hw = game_with_all_routines();
    let mut off = Cheats::default();
    off.perfect = Perfect::Wild;
    let _jit = refreshed(&mut off, &mut hw);
    assert_eq!(wild_create_mon(&off, &mut hw, 0x792D_497E, 0xD5E9_60F3, 143), 0x792D_497E);

    let mut cheats = Cheats::default();
    cheats.shiny = true;
    let _jit = refreshed(&mut cheats, &mut hw);
    let mut cpu = cpu();
    cpu.regs[13] = 0x0210_0000;
    cpu.regs[14] = 0x0200_1000;
    for (at, value) in [(0, 1), (4, 0x792D_497E), (8, OT_ID_PRESET), (12, 0xD5E9_60F3)] {
        hw.write(ARM9, cpu.regs[13] + at, 4, value);
    }
    cheats.fire(CREATE_MON_AT, &mut cpu, &mut hw);
    assert_eq!(hw.read(ARM9, cpu.regs[13] + 4, 4), 0x792D_497E, "a gift or a trainer's");
}

#[test]
fn shiny_works_through_the_create_mon_variants_using_the_trainer_id_in_r7() {
    let mut hw = game_with_all_routines();
    let mut cheats = Cheats::default();
    cheats.shiny = true;
    let _jit = refreshed(&mut cheats, &mut hw);
    let ot_id = 0xD5E9_60F3;
    hw.write(ARM9, 0x0210_8000, 4, ot_id);

    let mut variant = cpu();
    variant.regs[7] = 0x0210_8000;
    variant.regs[14] = WILD_SECOND_AT + 0x80;
    cheats.fire(PERSONALITY_AT, &mut variant, &mut hw);

    let mut create = cpu();
    create.regs[13] = 0x0210_0000;
    create.regs[14] = PERSONALITY_AT + 0x96;
    create.regs[1] = 143;
    for (at, value) in [(0, 1), (4, 0x792D_497E), (8, 0), (12, 0)] {
        hw.write(ARM9, create.regs[13] + at, 4, value);
    }
    cheats.fire(CREATE_MON_AT, &mut create, &mut hw);
    let personality = hw.read(ARM9, create.regs[13] + 4, 4);
    assert!(is_shiny(personality, ot_id));
    assert_eq!(personality % 25, 0x792D_497E % 25);

    // The ID is used once: a later call that did not come through a wild routine is untouched.
    hw.write(ARM9, create.regs[13] + 4, 4, 0x792D_497E);
    cheats.fire(CREATE_MON_AT, &mut create, &mut hw);
    assert_eq!(hw.read(ARM9, create.regs[13] + 4, 4), 0x792D_497E);
}
