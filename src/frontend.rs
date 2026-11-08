use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use minifb::{Key, KeyRepeat, Scale, ScaleMode, Window, WindowOptions};

mod audio;
mod fast_forward;
mod input;
mod overlay;
mod saves;
mod settings;
mod window_control;

use audio::AudioOutput;
use crate::system::Nds;
use crate::video::gpu::{HEIGHT, WIDTH};
use fast_forward::{run_fast, run_multiple};
use input::{KEY_MAP, pointer_state, touch_from_mouse};
use overlay::badge::{draw_badge, format_speed};
use overlay::cheat_menu::{CheatMenu, MenuKey};
use overlay::dock::Dock;
use saves::{AUTOSAVE_FRAMES, NOTICE_FRAMES, restart, save_if_dirty, state_action, write_flash_log};
use settings::{load_settings, save_settings};

/// The DS stacks its two screens vertically.
pub const WINDOW_HEIGHT: usize = HEIGHT * 2;
/// The window shows the picture at twice its size, so the dock can be drawn sharply.
const SCALE: usize = 2;
const SHOWN_WIDTH: usize = WIDTH * SCALE;
const SHOWN_HEIGHT: usize = WINDOW_HEIGHT * SCALE;

/// The DS's refresh rate: bus clock / (263 lines * 2130 cycles).
const FRAME_RATE: f64 = 33_513_982.0 / 560_190.0;
const SPEED_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// Show the 256x384 picture at twice the size in `out`.
fn upscale(picture: &[u32], out: &mut [u32]) {
    for (y, row) in picture.chunks_exact(WIDTH).enumerate() {
        let (top, bottom) = out[y * 2 * SHOWN_WIDTH..(y * 2 + 2) * SHOWN_WIDTH].split_at_mut(SHOWN_WIDTH);
        for (x, &pixel) in row.iter().enumerate() {
            top[x * 2] = pixel;
            top[x * 2 + 1] = pixel;
        }
        bottom.copy_from_slice(top);
    }
}

/// The keys that drive the cheat menu while it is open.
const MENU_KEYS: [(Key, MenuKey); 7] = [
    (Key::Up, MenuKey::Up),
    (Key::Down, MenuKey::Down),
    (Key::Left, MenuKey::Left),
    (Key::Right, MenuKey::Right),
    (Key::PageUp, MenuKey::PageUp),
    (Key::PageDown, MenuKey::PageDown),
    (Key::Enter, MenuKey::Toggle),
];

/// Set by the Ctrl+C handler so the main loop can exit and save instead of dying mid-write.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_interrupt(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Run the emulator in a window. `0` writes a save state to `state_path`, `1` restores it.
pub fn run(
    nds: &mut Nds,
    title: &str,
    save_path: Option<&Path>,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut window = Window::new(
        title,
        SHOWN_WIDTH,
        SHOWN_HEIGHT,
        WindowOptions {
            scale: Scale::X1,
            resize: true,
            scale_mode: ScaleMode::AspectRatioStretch,
            ..WindowOptions::default()
        },
    )?;
    // Pacing is done below, against the DS's real refresh rate.
    window.set_target_fps(0);
    window_control::lock_aspect_ratio(&window, SHOWN_WIDTH, SHOWN_HEIGHT, SHOWN_WIDTH / 2, SHOWN_HEIGHT / 2);
    window_control::enable_fullscreen(&window);
    let settings_path = state_path.with_extension("settings");
    let mut dock = Dock::new(1.0);
    let mut menu = CheatMenu::new(&nds.cheats);
    load_settings(&settings_path, &mut dock, &mut menu, &mut nds.cheats);

    // The game still runs, silently, if there is no usable audio device.
    let audio = match AudioOutput::new() {
        Ok(audio) => Some(audio),
        Err(error) => {
            eprintln!("audio disabled: {error}");
            None
        }
    };

    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, on_interrupt as libc::sighandler_t);
    }
    let mut notice_until = 0;
    let frame_time = Duration::from_secs_f64(1.0 / FRAME_RATE);
    let mut next_frame = Instant::now();
    let mut last_present = Instant::now();
    let mut next_autosave = nds.frames + AUTOSAVE_FRAMES;
    let (mut was_fast, mut meter_start, mut meter_frames) = (false, Instant::now(), 0);
    let mut speed_label: Option<String> = None;
    let mut badged = vec![0u32; WIDTH * WINDOW_HEIGHT];
    let mut shown = vec![0u32; SHOWN_WIDTH * SHOWN_HEIGHT];
    while window.is_open() && !INTERRUPTED.load(Ordering::SeqCst) {
        if window.is_key_pressed(Key::Tab, KeyRepeat::No) {
            menu.open = !menu.open;
        }
        if menu.open {
            let mut changed = false;
            for (key, action) in MENU_KEYS {
                if window.is_key_pressed(key, KeyRepeat::Yes) {
                    changed |= menu.press(action, &mut nds.cheats);
                }
            }
            if changed {
                save_settings(&settings_path, &dock, &menu, &nds.cheats);
            }
        }
        if window.is_key_pressed(Key::Escape, KeyRepeat::No) {
            if menu.open {
                menu.open = false;
            } else if window_control::is_fullscreen(&window) {
                window_control::toggle_fullscreen(&window);
            } else {
                break;
            }
        }
        let pointer = pointer_state(&window);
        dock.update(pointer, SHOWN_WIDTH, SHOWN_HEIGHT);
        if dock.take_quit_request() {
            break;
        }
        if dock.take_fullscreen_request() {
            window_control::toggle_fullscreen(&window);
        }
        if dock.take_restart_request() {
            restart(nds, save_path);
            if let Some(audio) = &audio {
                audio.clear();
            }
            next_frame = Instant::now();
        }
        if dock.take_changed() {
            save_settings(&settings_path, &dock, &menu, &nds.cheats);
        }

        for (key, save) in [(Key::Key0, true), (Key::Key1, false)] {
            if window.is_key_pressed(key, KeyRepeat::No) {
                let message = state_action(nds, state_path, save);
                if let (false, Some(audio)) = (save, &audio) {
                    audio.clear();
                }
                eprintln!("{message}");
                window.set_title(&format!("{title} - {message}"));
                notice_until = nds.frames + NOTICE_FRAMES;
            }
        }
        if notice_until != 0 && nds.frames >= notice_until {
            window.set_title(title);
            notice_until = 0;
        }

        // The menu has the keyboard while it is open (Enter is the game's Start button too).
        nds.hw.keys = if menu.open {
            0
        } else {
            KEY_MAP.iter().filter(|(key, _)| window.is_key_down(*key)).fold(0, |keys, (_, bit)| keys | bit)
        };
        // A click on the dock is the dock's, not a touch on the screen under it.
        nds.hw.touch = pointer.position.filter(|_| pointer.left_down).and_then(|(x, y)| {
            if menu.open || dock.covers(x, y, SHOWN_WIDTH, SHOWN_HEIGHT) {
                None
            } else {
                touch_from_mouse((x, y), (SHOWN_WIDTH, SHOWN_HEIGHT))
            }
        });

        let fast = window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift);
        if fast {
            if !was_fast {
                meter_start = Instant::now();
                meter_frames = 0;
                speed_label = None;
            }
            meter_frames += match dock.speed().multiple() {
                None => run_fast(nds, last_present),
                Some(n) => run_multiple(nds, n),
            };
            if meter_start.elapsed() >= SPEED_SAMPLE_INTERVAL {
                let speed = meter_frames as f64 / meter_start.elapsed().as_secs_f64() / FRAME_RATE;
                speed_label = Some(format_speed(speed));
                meter_start = Instant::now();
                meter_frames = 0;
            }
        } else {
            nds.run_frame();
            let mut samples = nds.take_audio();
            let gain = dock.gain();
            if gain < 1.0 {
                for sample in &mut samples {
                    *sample = (*sample as f32 * gain) as i16;
                }
            }
            if let Some(audio) = &audio {
                audio.push(&samples);
            }
        }
        was_fast = fast;

        let picture: &[u32] = match (&speed_label, fast) {
            (Some(label), true) => {
                badged.copy_from_slice(nds.framebuffer());
                draw_badge(&mut badged, label);
                &badged
            }
            _ => nds.framebuffer(),
        };
        upscale(picture, &mut shown);
        dock.draw(&mut shown, SHOWN_WIDTH, SHOWN_HEIGHT);
        menu.draw(&mut shown, SHOWN_WIDTH, SHOWN_HEIGHT, &nds.cheats);
        window.update_with_buffer(&shown, SHOWN_WIDTH, SHOWN_HEIGHT)?;
        last_present = Instant::now();

        if fast && dock.speed().multiple().is_none() {
            next_frame = last_present;
        } else {
            next_frame += frame_time;
            let now = Instant::now();
            if next_frame > now {
                std::thread::sleep(next_frame - now);
            } else if now - next_frame > frame_time * 6 {
                next_frame = now; // fell far behind: don't race to catch up
            }
        }

        if nds.frames >= next_autosave {
            next_autosave = nds.frames + AUTOSAVE_FRAMES;
            save_if_dirty(nds, save_path);
            write_flash_log(nds);
        }
    }
    save_if_dirty(nds, save_path);
    write_flash_log(nds);
    save_settings(&settings_path, &dock, &menu, &nds.cheats);
    Ok(())
}

