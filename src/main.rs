mod files;
mod headless;

use std::process::ExitCode;

use ds_emulator::cartridge::Cartridge;
use ds_emulator::cheats::{Cheats, Perfect, WildMon};
use ds_emulator::frontend;

use files::{new_console, save_path, state_path};
use headless::{HeadlessOptions, parse_script_input, run_headless};

const USAGE: &str = "usage: ds_emulator [--info] [--frames <n> [--screenshot <file.png>] [--wav <file.wav>] [--load-state <file>] [--save-state <file>] [--tap <x>,<y>@<frame>]... [--press <button>@<frame>]...] [--wild <species>@<level>] [--perfect <wild|opponents|both>] <rom.nds>";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut info_only = false;
    let mut headless_frames = None;
    let mut screenshot = None;
    let mut wav = None;
    let mut load_state = None;
    let mut save_state = None;
    let mut script = Vec::new();
    let mut cheats = Cheats::default();
    let mut rom_path = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--info" => info_only = true,
            "--frames" => headless_frames = Some(args.next().ok_or(USAGE)?.parse::<u64>()?),
            "--screenshot" => screenshot = Some(args.next().ok_or(USAGE)?),
            "--wav" => wav = Some(args.next().ok_or(USAGE)?),
            "--load-state" => load_state = Some(args.next().ok_or(USAGE)?),
            "--save-state" => save_state = Some(args.next().ok_or(USAGE)?),
            "--tap" => script.push(parse_script_input(&args.next().ok_or(USAGE)?, true)?),
            "--press" => script.push(parse_script_input(&args.next().ok_or(USAGE)?, false)?),
            "--wild" => cheats.wild = Some(parse_wild(&args.next().ok_or(USAGE)?)?),
            "--perfect" => {
                let choice = args.next().ok_or(USAGE)?;
                cheats.perfect = Perfect::from_key(&choice).ok_or_else(|| format!("unknown --perfect choice {choice:?}"))?;
            }
            _ => rom_path = Some(arg),
        }
    }
    let rom_path = rom_path.ok_or(USAGE)?;
    let cart = Cartridge::load(&rom_path)?;
    let h = &cart.header;

    println!("{} [{}] maker {}", h.title, h.game_code, h.maker_code);
    println!(
        "ARM9: rom {:#x} entry {:#010x} load {:#010x} size {:#x}",
        h.arm9.rom_offset, h.arm9.entry, h.arm9.ram_address, h.arm9.size
    );
    println!(
        "ARM7: rom {:#x} entry {:#010x} load {:#010x} size {:#x}",
        h.arm7.rom_offset, h.arm7.entry, h.arm7.ram_address, h.arm7.size
    );
    println!("header CRC {:#06x} ({})", h.header_crc, if h.header_crc_ok { "ok" } else { "BAD" });

    if info_only {
        return Ok(());
    }
    if let Some(frames) = headless_frames {
        return run_headless(
            cart,
            &rom_path,
            frames,
            HeadlessOptions {
                screenshot: screenshot.as_deref(),
                wav: wav.as_deref(),
                load_state: load_state.as_deref(),
                save_state: save_state.as_deref(),
                script: &script,
                cheats,
            },
        );
    }
    let title = h.title.clone();
    let mut nds = new_console(cart, &rom_path);
    nds.cheats = cheats;
    if std::env::var_os("DS_FLASH_LOG").is_some() {
        nds.hw.backup.command_log = Some(Vec::new());
    }
    if let Some(path) = &load_state {
        nds.load_state(&std::fs::read(path)?)?;
    }
    frontend::run(&mut nds, &title, Some(&save_path(&rom_path)), &state_path(&rom_path))
}

/// Parse `species@level`.
fn parse_wild(spec: &str) -> Result<WildMon, Box<dyn std::error::Error>> {
    let (species, level) = spec.split_once('@').ok_or(USAGE)?;
    Ok(WildMon::new(species.parse()?, level.parse()?))
}
