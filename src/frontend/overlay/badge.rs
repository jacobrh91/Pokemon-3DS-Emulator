//! The speed badge shown in the top-left corner of the picture while fast-forwarding.

use crate::video::gpu::WIDTH;

/// Glyphs for the speed badge: 3 wide, 5 tall, one row per 3 low bits.
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [7, 1, 7, 4, 7],
        '3' => [7, 1, 7, 1, 7],
        '4' => [5, 5, 7, 1, 1],
        '5' => [7, 4, 7, 1, 7],
        '6' => [7, 4, 7, 5, 7],
        '7' => [7, 1, 2, 2, 2],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 7],
        '.' => [0, 0, 0, 0, 2],
        'x' => [0, 5, 2, 5, 0],
        _ => [0; 5],
    }
}

/// The speed as shown on the badge: "3.7x" below 10x, "12x" above.
pub fn format_speed(speed: f64) -> String {
    if speed < 9.95 { format!("{speed:.1}x") } else { format!("{speed:.0}x") }
}

/// Draw `text` on a dark badge in the top-left corner of the picture.
pub fn draw_badge(pixels: &mut [u32], text: &str) {
    const MARGIN: usize = 3;
    const PADDING: usize = 2;
    let width = text.chars().count() * 4 - 1 + PADDING * 2;
    let height = 5 + PADDING * 2;
    for y in 0..height {
        pixels[(MARGIN + y) * WIDTH + MARGIN..(MARGIN + y) * WIDTH + MARGIN + width].fill(0x0020_2428);
    }
    for (i, c) in text.chars().enumerate() {
        for (row, bits) in glyph(c).iter().enumerate() {
            for col in 0..3 {
                if bits >> (2 - col) & 1 != 0 {
                    let (x, y) = (MARGIN + PADDING + i * 4 + col, MARGIN + PADDING + row);
                    pixels[y * WIDTH + x] = 0x00F4_F6F8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::WINDOW_HEIGHT;

    #[test]
    fn speed_is_formatted_like_the_gba_badge() {
        assert_eq!(format_speed(2.04), "2.0x");
        assert_eq!(format_speed(3.72), "3.7x");
        assert_eq!(format_speed(12.4), "12x");
    }

    #[test]
    fn badge_draws_in_the_top_left_only() {
        let mut pixels = vec![0u32; WIDTH * WINDOW_HEIGHT];
        draw_badge(&mut pixels, "2.0x");
        assert!(pixels[3 * WIDTH + 3] != 0, "badge background");
        assert!(pixels[..20 * WIDTH].contains(&0x00F4_F6F8), "text drawn");
        assert!(pixels[20 * WIDTH..].iter().all(|&p| p == 0));
    }
}
