//! Running the game faster than real time while Shift is held.

use std::time::{Duration, Instant};

use crate::system::Nds;

/// How often fast-forward shows a picture and refreshes its speed reading.
const FAST_PRESENT_INTERVAL: Duration = Duration::from_millis(16);

/// Frames drawn before each fast-forward picture. The game swaps its 3D scene every
/// second frame, so this guarantees the picture has a fresh one.
const FAST_DRAWN_FRAMES: u32 = 3;

/// Run flat out until it's time to show a picture, drawing only the last few frames.
/// Sped-up audio is dropped, as pitching it up isn't worth listening to. Returns frames run.
pub(super) fn run_fast(nds: &mut Nds, last_present: Instant) -> u32 {
    let mut frames = 0;
    nds.hw.skip_render = true;
    nds.hw.gx.skip_draw = true;
    while last_present.elapsed() < FAST_PRESENT_INTERVAL {
        nds.run_frame();
        frames += 1;
    }
    nds.hw.gx.skip_draw = false;
    for i in 0..FAST_DRAWN_FRAMES {
        nds.hw.skip_render = i + 1 != FAST_DRAWN_FRAMES;
        nds.run_frame();
        frames += 1;
    }
    nds.hw.skip_render = false;
    nds.take_audio();
    frames
}

/// Run `n` frames for each picture shown, for the 2X/4X/8X settings. Frames before the
/// last few are not drawn. Sped-up audio is dropped. Returns frames run.
pub(super) fn run_multiple(nds: &mut Nds, n: u32) -> u32 {
    for i in 0..n {
        let drawn = i + FAST_DRAWN_FRAMES >= n;
        nds.hw.skip_render = !drawn || i + 1 != n;
        nds.hw.gx.skip_draw = !drawn;
        nds.run_frame();
    }
    nds.hw.skip_render = false;
    nds.hw.gx.skip_draw = false;
    nds.take_audio();
    n
}
