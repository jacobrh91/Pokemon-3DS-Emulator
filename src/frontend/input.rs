//! Reading the keyboard and mouse: the button layout, the pointer, and touch-screen mapping.

use minifb::{Key, MouseButton, MouseMode, Window};

use super::overlay::dock::Pointer;
use super::{SHOWN_HEIGHT, SHOWN_WIDTH, WINDOW_HEIGHT, window_control};
use crate::video::gpu::{HEIGHT, WIDTH};

/// Keyboard layout, as bits of `Hardware::keys` (A B Select Start Right Left Up Down R L X Y).
/// Matches the GBA emulator's defaults; the DS's own buttons take their letters, so Start moves to Enter.
pub(super) const KEY_MAP: [(Key, u16); 12] = [
    (Key::Space, 1 << 0),
    (Key::C, 1 << 1),
    (Key::V, 1 << 2),
    (Key::Enter, 1 << 3),
    (Key::D, 1 << 4),
    (Key::A, 1 << 5),
    (Key::W, 1 << 6),
    (Key::S, 1 << 7),
    (Key::E, 1 << 8),
    (Key::Q, 1 << 9),
    (Key::X, 1 << 10),
    (Key::Z, 1 << 11),
];

/// Map a mouse position in window space to a pixel on the bottom screen.
pub fn touch_from_mouse(mouse: (f32, f32), window: (usize, usize)) -> Option<(u8, u8)> {
    let x = mouse.0 / window.0 as f32 * WIDTH as f32;
    let y = mouse.1 / window.1 as f32 * WINDOW_HEIGHT as f32 - HEIGHT as f32;
    ((0.0..WIDTH as f32).contains(&x) && (0.0..HEIGHT as f32).contains(&y)).then(|| (x as u8, y as u8))
}

pub(super) fn pointer_state(window: &Window) -> Pointer {
    let buffer = (SHOWN_WIDTH, SHOWN_HEIGHT);
    let position = match window_control::pointer_in_picture_view(window) {
        Some((pointer, view)) => window_control::pointer_in_buffer(Some(pointer), view, buffer),
        None => window_control::pointer_in_buffer(
            window.get_mouse_pos(MouseMode::Discard),
            window_control::content_size(window),
            buffer,
        ),
    };
    Pointer {
        position,
        left_down: window.get_mouse_down(MouseButton::Left)
            || (position.is_some() && window_control::left_button_down().unwrap_or(false)),
        scroll: window.get_scroll_wheel().map_or(0.0, |(_, y)| y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_on_top_screen_is_not_a_touch() {
        assert_eq!(touch_from_mouse((100.0, 100.0), (512, 768)), None);
    }

    #[test]
    fn mouse_maps_to_bottom_screen_pixels() {
        // Window is 2x scale: the bottom screen starts at y = 384.
        assert_eq!(touch_from_mouse((200.0, 384.0 + 100.0), (512, 768)), Some((100, 50)));
        assert_eq!(touch_from_mouse((0.0, 384.0), (512, 768)), Some((0, 0)));
    }

    #[test]
    fn mouse_outside_window_is_not_a_touch() {
        assert_eq!(touch_from_mouse((600.0, 500.0), (512, 768)), None);
    }
}
