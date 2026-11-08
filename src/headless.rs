//! Running without a window: a fixed number of frames with scripted input, then a report,
//! and optionally a screenshot, a WAV of the audio and a save state.

use ds_emulator::cartridge::Cartridge;

use crate::files::{new_console, save_path};

/// Frames a scripted tap or button press is held.
const SCRIPTED_HOLD_FRAMES: u64 = 6;

/// Run without a window and report where both CPUs ended up and which
/// hardware accesses the emulator doesn't handle yet.
pub struct HeadlessOptions<'a> {
    pub screenshot: Option<&'a str>,
    pub wav: Option<&'a str>,
    pub load_state: Option<&'a str>,
    pub save_state: Option<&'a str>,
    pub script: &'a [ScriptedInput],
    pub cheats: ds_emulator::cheats::Cheats,
}

pub fn run_headless(cart: Cartridge, rom_path: &str, frames: u64, opts: HeadlessOptions) -> Result<(), Box<dyn std::error::Error>> {
    let mut nds = new_console(cart, rom_path);
    nds.cheats = opts.cheats;
    if let Some(path) = opts.load_state {
        nds.load_state(&std::fs::read(path)?)?;
    }
    let (screenshot, wav, script) = (opts.screenshot, opts.wav, opts.script);
    let mut audio = Vec::new();
    for frame in 1..=frames {
        nds.hw.keys = 0;
        nds.hw.touch = None;
        for input in script.iter().filter(|i| (i.frame..i.frame + SCRIPTED_HOLD_FRAMES).contains(&frame)) {
            match input.what {
                Input::Button(bit) => nds.hw.keys |= bit,
                Input::Touch(x, y) => nds.hw.touch = Some((x, y)),
            }
        }
        nds.run_frame();
        let samples = nds.take_audio();
        if wav.is_some() {
            audio.extend(samples);
        }
        if frame % 60 == 0 || frame == frames {
            println!(
                "frame {frame}: ARM9 pc={:#010x} ARM7 pc={:#010x}",
                nds.arm9.regs[15], nds.arm7.regs[15]
            );
        }
    }
    for (name, cpu) in [("ARM9", &nds.arm9), ("ARM7", &nds.arm7)] {
        println!("{name} cpsr={:#010x} regs={:08x?}", cpu.cpsr, cpu.regs);
    }
    if let Some(path) = wav {
        std::fs::write(path, ds_emulator::capture::wav::encode(&audio))?;
        println!("wrote {path} ({:.1} s)", audio.len() as f32 / 2.0 / ds_emulator::capture::wav::SAMPLE_RATE as f32);
    }
    if let Some(path) = screenshot {
        let png = ds_emulator::capture::png::encode(256, 384, nds.framebuffer());
        std::fs::write(path, png)?;
        println!("wrote {path}");
    }
    if let Some(path) = opts.save_state {
        std::fs::write(path, nds.save_state())?;
        println!("wrote {path}");
    }
    if nds.hw.backup.dirty {
        nds.hw.backup.save_to(save_path(rom_path))?;
        println!("wrote {}", save_path(rom_path).display());
    }
    let mut unhandled: Vec<_> = nds.hw.unhandled.iter().collect();
    unhandled.sort_by_key(|&(_, n)| std::cmp::Reverse(*n));
    for (what, count) in unhandled.into_iter().take(30) {
        println!("unhandled: {what} x{count}");
    }
    Ok(())
}


pub enum Input {
    Button(u16),
    Touch(u8, u8),
}

pub struct ScriptedInput {
    pub frame: u64,
    pub what: Input,
}

/// Parse `x,y@frame` (a tap) or `button@frame`.
pub fn parse_script_input(spec: &str, touch: bool) -> Result<ScriptedInput, Box<dyn std::error::Error>> {
    let (what, frame) = spec.split_once('@').ok_or(crate::USAGE)?;
    let frame = frame.parse()?;
    let what = if touch {
        let (x, y) = what.split_once(',').ok_or(crate::USAGE)?;
        Input::Touch(x.parse()?, y.parse()?)
    } else {
        let bit = ["a", "b", "select", "start", "right", "left", "up", "down", "r", "l", "x", "y"]
            .iter()
            .position(|&name| name == what)
            .ok_or_else(|| format!("unknown button {what:?}"))?;
        Input::Button(1 << bit)
    };
    Ok(ScriptedInput { frame, what })
}
