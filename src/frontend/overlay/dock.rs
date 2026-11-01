//! A control dock (power, restart, full screen, volume, fast-forward speed) that slides
//! up when the mouse is near the bottom of the window and away when it is still. Laid
//! out in the same order as the GBA emulator's, minus the cheat and controls buttons.
//!
//! Interaction ([`Dock::update`]) is separate from drawing ([`Dock::draw`]) so it can be
//! tested without a window.

use super::canvas::{Canvas, GLYPH_HEIGHT, Rect, TEXT_SCALE, text_width};

/// How fast the fast-forward key runs the game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpeedSetting {
    X2,
    X4,
    X8,
    /// As fast as the machine allows.
    #[default]
    Max,
}

impl SpeedSetting {
    pub const ALL: [SpeedSetting; 4] = [Self::X2, Self::X4, Self::X8, Self::Max];

    pub fn label(self) -> &'static str {
        match self {
            Self::X2 => "2X",
            Self::X4 => "4X",
            Self::X8 => "8X",
            Self::Max => "MAX",
        }
    }

    /// The target multiple of real time, or `None` for unthrottled.
    pub fn multiple(self) -> Option<u32> {
        match self {
            Self::X2 => Some(2),
            Self::X4 => Some(4),
            Self::X8 => Some(8),
            Self::Max => None,
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.label().eq_ignore_ascii_case(label))
    }
}

/// Height of the strip along the bottom of the window that keeps the dock open for as
/// long as the mouse is anywhere in it.
const HOVER_ZONE: f32 = 90.0;
/// Frames of mouse stillness (about 2.5 s at 60 Hz) before the dock hides.
const HIDE_AFTER_IDLE_FRAMES: u32 = 150;
/// Fraction of the slide animation advanced per frame.
const REVEAL_STEP: f32 = 0.18;
const WHEEL_STEP: f32 = 0.05;

const POWER_COLOR: u32 = 0x00D9_3B3B;
const POWER_SYMBOL_COLOR: u32 = 0x00FF_FFFF;
const DOCK_COLOR: u32 = 0x0020_2428;
const DOCK_ALPHA: f32 = 0.88;
const TRACK_COLOR: u32 = 0x0059_6168;
const FILL_COLOR: u32 = 0x0046_B4FF;
const MUTED_FILL_COLOR: u32 = 0x0090_9498;
const KNOB_COLOR: u32 = 0x00F4_F6F8;
const ICON_COLOR: u32 = 0x00F4_F6F8;
const MUTE_SLASH_COLOR: u32 = 0x00FF_5A5A;

const BUTTON: i32 = 30;
const DOCK_HEIGHT: i32 = 50;
const DOCK_MARGIN: i32 = 12;
const PADDING: i32 = 10;
const ICON_SIZE: i32 = 22;
const TRACK_WIDTH: i32 = 36;
const TRACK_HEIGHT: i32 = 6;
const KNOB_RADIUS: i32 = 7;
const SPEED_WIDTH: i32 = 44;
const SPEED_HEIGHT: i32 = 28;
const SPEED_GAP: i32 = 3;
/// "100%" in the dock's font.
const PERCENT_WIDTH: i32 = 4 * (TEXT_SCALE * 7) - TEXT_SCALE;
const DOCK_WIDTH: i32 = PADDING
    + 3 * BUTTON + 2 * 4
    + 8 + 2 + 8
    + ICON_SIZE + 10 + TRACK_WIDTH + 10 + PERCENT_WIDTH
    + 8 + 2 + 8
    + 4 * SPEED_WIDTH + 3 * SPEED_GAP
    + PADDING;

#[derive(Clone, Copy, Debug, Default)]
pub struct Pointer {
    /// Cursor position in buffer pixels, or `None` when outside the window.
    pub position: Option<(f32, f32)>,
    pub left_down: bool,
    /// Vertical wheel movement since the last frame (positive = up).
    pub scroll: f32,
}

/// Element rectangles for a fully revealed dock in a `width`×`height` buffer.
struct Layout {
    dock: Rect,
    power: Rect,
    restart: Rect,
    fullscreen: Rect,
    restart_divider_x: i32,
    icon: Rect,
    track: Rect,
    text_x: i32,
    text_y: i32,
    divider_x: i32,
    speed_buttons: [Rect; 4],
}

impl Layout {
    fn new(width: usize, height: usize) -> Layout {
        let dock = Rect {
            x: (width as i32 - DOCK_WIDTH) / 2,
            y: height as i32 - DOCK_MARGIN - DOCK_HEIGHT,
            width: DOCK_WIDTH,
            height: DOCK_HEIGHT,
        };
        let cy = dock.y + DOCK_HEIGHT / 2;
        let button = |x: i32| Rect { x, y: cy - BUTTON / 2, width: BUTTON, height: BUTTON };
        // Left to right: power, restart, full screen, divider, volume, divider, speed.
        let power = button(dock.x + PADDING);
        let restart = button(power.x + BUTTON + 4);
        let fullscreen = button(restart.x + BUTTON + 4);
        let restart_divider_x = fullscreen.x + BUTTON + 8;
        let icon = Rect { x: restart_divider_x + 10, y: cy - ICON_SIZE / 2, width: ICON_SIZE, height: ICON_SIZE };
        let track = Rect { x: icon.x + ICON_SIZE + 10, y: cy - TRACK_HEIGHT / 2, width: TRACK_WIDTH, height: TRACK_HEIGHT };
        let text_x = track.x + TRACK_WIDTH + 10;
        let divider_x = text_x + PERCENT_WIDTH + 8;
        let buttons_x = divider_x + 10;
        let speed_buttons = std::array::from_fn(|i| Rect {
            x: buttons_x + i as i32 * (SPEED_WIDTH + SPEED_GAP),
            y: cy - SPEED_HEIGHT / 2,
            width: SPEED_WIDTH,
            height: SPEED_HEIGHT,
        });
        Layout {
            dock,
            power,
            restart,
            fullscreen,
            restart_divider_x,
            icon,
            track,
            text_x,
            text_y: cy - GLYPH_HEIGHT * TEXT_SCALE / 2,
            divider_x,
            speed_buttons,
        }
    }
}

pub struct Dock {
    volume: f32,
    muted: bool,
    /// 0.0 hidden .. 1.0 fully shown.
    reveal: f32,
    dragging: bool,
    was_down: bool,
    speed: SpeedSetting,
    last_position: Option<(f32, f32)>,
    idle_frames: u32,
    restart_requested: bool,
    quit_requested: bool,
    fullscreen_requested: bool,
    /// Volume, mute or speed changed since the settings were last read.
    changed: bool,
}

impl Dock {
    pub fn new(initial_volume: f32) -> Dock {
        Dock {
            volume: initial_volume.clamp(0.0, 1.0),
            muted: false,
            reveal: 0.0,
            dragging: false,
            was_down: false,
            speed: SpeedSetting::default(),
            last_position: None,
            idle_frames: HIDE_AFTER_IDLE_FRAMES,
            restart_requested: false,
            quit_requested: false,
            fullscreen_requested: false,
            changed: false,
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    pub fn speed(&self) -> SpeedSetting {
        self.speed
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    pub fn set_speed(&mut self, speed: SpeedSetting) {
        self.speed = speed;
    }

    /// True once after volume, mute or speed was changed with the dock.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Linear gain to apply to the audio. The slider is perceptual, so the gain follows
    /// a squared curve.
    pub fn gain(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume * self.volume }
    }

    /// Whether the dock is showing over the point, so a click there is the dock's.
    pub fn covers(&self, x: f32, y: f32, width: usize, height: usize) -> bool {
        self.reveal >= 0.5 && Layout::new(width, height).dock.contains(x, y)
    }

    /// True once per click of the full-screen button.
    pub fn take_fullscreen_request(&mut self) -> bool {
        std::mem::take(&mut self.fullscreen_requested)
    }

    /// True once per click of the power button (the app should exit).
    pub fn take_quit_request(&mut self) -> bool {
        std::mem::take(&mut self.quit_requested)
    }

    /// True once per click of the restart button.
    pub fn take_restart_request(&mut self) -> bool {
        std::mem::take(&mut self.restart_requested)
    }

    /// Advances the slide animation and processes pointer input for one frame.
    pub fn update(&mut self, pointer: Pointer, width: usize, height: usize) {
        let layout = Layout::new(width, height);

        // Staying near the bottom keeps the dock; stillness hides it again.
        if pointer.position.is_some() && pointer.position != self.last_position {
            self.idle_frames = 0;
        } else {
            self.idle_frames = self.idle_frames.saturating_add(1);
        }
        self.last_position = pointer.position;
        let in_hover_zone = pointer.position.is_some_and(|(_, y)| y >= height as f32 - HOVER_ZONE);
        let active = in_hover_zone && self.idle_frames < HIDE_AFTER_IDLE_FRAMES;
        let target = if active || self.dragging { 1.0 } else { 0.0 };
        let delta: f32 = target - self.reveal;
        self.reveal = (self.reveal + delta.clamp(-REVEAL_STEP, REVEAL_STEP)).clamp(0.0, 1.0);

        let pressed = pointer.left_down && !self.was_down;
        self.was_down = pointer.left_down;
        if !pointer.left_down {
            self.dragging = false;
        }

        // Ignore input while the dock is still sliding mostly out of view.
        if self.reveal < 0.5 {
            return;
        }
        if let Some((x, y)) = pointer.position {
            if pressed {
                if layout.power.contains(x, y) {
                    self.quit_requested = true;
                } else if layout.restart.contains(x, y) {
                    self.restart_requested = true;
                } else if layout.fullscreen.contains(x, y) {
                    self.fullscreen_requested = true;
                } else if layout.icon.inflated(6).contains(x, y) {
                    self.muted = !self.muted;
                    self.changed = true;
                } else if layout.track.inflated(KNOB_RADIUS).contains(x, y) {
                    self.dragging = true;
                    self.muted = false;
                } else if let Some(index) = layout.speed_buttons.iter().position(|b| b.contains(x, y)) {
                    self.speed = SpeedSetting::ALL[index];
                    self.changed = true;
                }
            }
            if pointer.scroll != 0.0 && layout.dock.contains(x, y) {
                self.volume = (self.volume + pointer.scroll.signum() * WHEEL_STEP).clamp(0.0, 1.0);
                self.muted = false;
                self.changed = true;
            }
        }
        if self.dragging
            && let Some((x, _)) = pointer.position
        {
            self.volume = ((x - layout.track.x as f32) / layout.track.width as f32).clamp(0.0, 1.0);
            self.changed = true;
        }
    }

    /// Composites the dock over `buffer` (`0x00RRGGBB` pixels, row-major).
    pub fn draw(&self, buffer: &mut [u32], width: usize, height: usize) {
        if self.reveal <= 0.0 {
            return;
        }
        let layout = Layout::new(width, height);
        let eased = 1.0 - (1.0 - self.reveal).powi(3);
        let slide = ((1.0 - eased) * (DOCK_HEIGHT + DOCK_MARGIN) as f32).round() as i32;
        let mut canvas = Canvas::new(buffer, width, height, slide);

        canvas.rounded_rect(layout.dock, 12, DOCK_COLOR, DOCK_ALPHA);
        let divider = |canvas: &mut Canvas, x: i32| canvas.rect(x, layout.dock.y + 12, 2, DOCK_HEIGHT - 24, TRACK_COLOR);

        // Power (red), restart (circular arrow) and full screen (corner brackets).
        canvas.rounded_rect(layout.power, 8, POWER_COLOR, 1.0);
        canvas.power_symbol(
            layout.power.x + layout.power.width / 2,
            layout.power.y + layout.power.height / 2 + 2,
            7,
            POWER_SYMBOL_COLOR,
        );
        canvas.rounded_rect(layout.restart, 8, TRACK_COLOR, 0.55);
        canvas.circular_arrow(
            layout.restart.x + layout.restart.width / 2,
            layout.restart.y + layout.restart.height / 2,
            8,
            ICON_COLOR,
        );
        canvas.rounded_rect(layout.fullscreen, 8, TRACK_COLOR, 0.55);
        canvas.corner_brackets(
            layout.fullscreen.x + layout.fullscreen.width / 2,
            layout.fullscreen.y + layout.fullscreen.height / 2,
            7,
            6,
            ICON_COLOR,
        );
        divider(&mut canvas, layout.restart_divider_x);

        // Speaker icon.
        let (ix, iy) = (layout.icon.x, layout.icon.y);
        canvas.rect(ix + 1, iy + 7, 6, 8, ICON_COLOR);
        for column in 0..8 {
            let half = 4 + column;
            canvas.rect(ix + 7 + column, iy + 11 - half, 1, half * 2, ICON_COLOR);
        }
        if self.muted || self.volume <= 0.0 {
            for step in 0..11 {
                canvas.rect(ix + 12 + step, iy + 6 + step, 2, 2, MUTE_SLASH_COLOR);
                canvas.rect(ix + 12 + step, iy + 16 - step, 2, 2, MUTE_SLASH_COLOR);
            }
        } else {
            let waves = if self.volume > 0.66 { 3 } else if self.volume > 0.33 { 2 } else { 1 };
            for wave in 0..waves {
                canvas.arc(ix + 10, iy + 11, 6 + wave * 4, ICON_COLOR);
            }
        }

        // Slider.
        let track = layout.track;
        let filled = (track.width as f32 * self.volume).round() as i32;
        let fill_color = if self.muted { MUTED_FILL_COLOR } else { FILL_COLOR };
        canvas.rounded_rect(track, TRACK_HEIGHT / 2, TRACK_COLOR, 1.0);
        if filled > 0 {
            canvas.rounded_rect(Rect { width: filled.max(TRACK_HEIGHT), ..track }, TRACK_HEIGHT / 2, fill_color, 1.0);
        }
        canvas.circle(track.x + filled, track.y + TRACK_HEIGHT / 2, KNOB_RADIUS, KNOB_COLOR);
        let percent = if self.muted { 0 } else { (self.volume * 100.0).round() as u32 };
        canvas.text(&format!("{percent}%"), layout.text_x, layout.text_y, ICON_COLOR);

        // Fast-forward speed selector.
        divider(&mut canvas, layout.divider_x);
        for (index, setting) in SpeedSetting::ALL.into_iter().enumerate() {
            let button = layout.speed_buttons[index];
            let selected = setting == self.speed;
            let (fill, text) = if selected { (FILL_COLOR, DOCK_COLOR) } else { (TRACK_COLOR, ICON_COLOR) };
            canvas.rounded_rect(button, 8, fill, if selected { 1.0 } else { 0.55 });
            let label = setting.label();
            canvas.text(
                label,
                button.x + (button.width - text_width(label)) / 2,
                button.y + (button.height - GLYPH_HEIGHT * TEXT_SCALE) / 2,
                text,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 512;
    const H: usize = 768;

    fn at(x: f32, y: f32, down: bool) -> Pointer {
        Pointer { position: Some((x, y)), left_down: down, scroll: 0.0 }
    }

    /// Reveal the dock by hovering over its bottom strip.
    fn shown() -> Dock {
        let mut dock = Dock::new(0.5);
        for _ in 0..10 {
            dock.update(at(250.0, 740.0 + (dock.reveal * 3.0), false), W, H);
        }
        assert!(dock.reveal > 0.99);
        dock
    }

    fn click(dock: &mut Dock, rect: Rect) {
        let (x, y) = ((rect.x + rect.width / 2) as f32, (rect.y + rect.height / 2) as f32);
        dock.update(at(x, y, false), W, H);
        dock.update(at(x, y, true), W, H);
        dock.update(at(x, y + 1.0, false), W, H);
    }

    #[test]
    fn dock_fits_the_window() {
        assert!(DOCK_WIDTH <= W as i32 - 20, "dock is {DOCK_WIDTH} wide");
        let layout = Layout::new(W, H);
        assert!(layout.speed_buttons[3].x + SPEED_WIDTH <= layout.dock.x + DOCK_WIDTH);
    }

    #[test]
    fn dock_hides_when_the_mouse_is_away_and_shows_near_the_bottom() {
        let mut dock = Dock::new(1.0);
        for i in 0..10 {
            dock.update(at(100.0, 100.0 + i as f32, false), W, H);
        }
        assert_eq!(dock.reveal, 0.0, "the top of the window doesn't show it");
        let dock = shown();
        assert!(dock.covers(250.0, 740.0, W, H));
    }

    #[test]
    fn speed_buttons_select_the_speed() {
        let mut dock = shown();
        let layout = Layout::new(W, H);
        for (i, setting) in SpeedSetting::ALL.into_iter().enumerate() {
            click(&mut dock, layout.speed_buttons[i]);
            assert_eq!(dock.speed(), setting);
        }
        assert!(dock.take_changed());
    }

    #[test]
    fn power_restart_and_fullscreen_make_requests() {
        let mut dock = shown();
        let layout = Layout::new(W, H);
        click(&mut dock, layout.power);
        click(&mut dock, layout.restart);
        click(&mut dock, layout.fullscreen);
        assert!(dock.take_quit_request() && dock.take_restart_request() && dock.take_fullscreen_request());
        assert!(!dock.take_quit_request(), "a request is reported once");
    }

    #[test]
    fn volume_slider_icon_and_wheel() {
        let mut dock = shown();
        let layout = Layout::new(W, H);
        let y = (layout.track.y + TRACK_HEIGHT / 2) as f32;
        dock.update(at(layout.track.x as f32 + 1.0, y, false), W, H);
        dock.update(at(layout.track.x as f32 + 1.0, y, true), W, H);
        dock.update(at((layout.track.x + TRACK_WIDTH) as f32, y, true), W, H);
        assert!(dock.volume() > 0.99, "dragging to the right end is full volume");
        dock.update(at(0.0, y, false), W, H);

        click(&mut dock, layout.icon);
        assert!(dock.is_muted() && dock.gain() == 0.0);
        let mut wheel = at(layout.track.x as f32, y, false);
        wheel.scroll = -1.0;
        dock.update(wheel, W, H);
        assert!(!dock.is_muted() && dock.volume() < 1.0);
    }

    #[test]
    fn gain_follows_a_squared_curve() {
        let mut dock = Dock::new(0.5);
        assert_eq!(dock.gain(), 0.25);
        dock.set_muted(true);
        assert_eq!(dock.gain(), 0.0);
    }

    #[test]
    fn drawing_a_revealed_dock_changes_pixels_inside_it_only() {
        let dock = shown();
        let mut buffer = vec![0u32; W * H];
        dock.draw(&mut buffer, W, H);
        let layout = Layout::new(W, H);
        for (i, &pixel) in buffer.iter().enumerate() {
            let (x, y) = ((i % W) as f32, (i / W) as f32);
            if pixel != 0 {
                assert!(layout.dock.inflated(1).contains(x, y), "stray pixel at {x},{y}");
            }
        }
        assert!(buffer.iter().any(|&p| p != 0));
    }
}
