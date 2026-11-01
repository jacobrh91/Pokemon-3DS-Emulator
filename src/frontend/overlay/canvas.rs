//! Small software drawing helpers for the dock: rectangles, circles, alpha
//! blending and a 5x7 bitmap font. Same shapes as the GBA emulator's dock.

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x as f32 && x < (self.x + self.width) as f32 && y >= self.y as f32 && y < (self.y + self.height) as f32
    }

    pub fn inflated(&self, by: i32) -> Rect {
        Rect { x: self.x - by, y: self.y - by, width: self.width + by * 2, height: self.height + by * 2 }
    }
}

pub struct Canvas<'a> {
    buffer: &'a mut [u32],
    width: usize,
    height: usize,
    /// Extra downward shift used for the slide animation.
    offset_y: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(buffer: &'a mut [u32], width: usize, height: usize, offset_y: i32) -> Self {
        Self { buffer, width, height, offset_y }
    }
}

// Kept out of line, as in the GBA emulator, where inlining the width into the caller
// made rustc 1.92's arm64 backend miscompile the pixel addressing.
impl Canvas<'_> {
    #[inline(never)]
    pub fn blend(&mut self, x: i32, y: i32, color: u32, alpha: f32) {
        let y = y + self.offset_y;
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        let pixel = &mut self.buffer[y as usize * self.width + x as usize];
        if alpha >= 1.0 {
            *pixel = color;
            return;
        }
        let mix = |shift: u32| {
            let dst = ((*pixel >> shift) & 0xFF) as f32;
            let src = ((color >> shift) & 0xFF) as f32;
            (dst + (src - dst) * alpha).round() as u32
        };
        *pixel = (mix(16) << 16) | (mix(8) << 8) | mix(0);
    }

    #[inline(never)]
    pub fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32) {
        for row in y..y + height {
            for column in x..x + width {
                self.blend(column, row, color, 1.0);
            }
        }
    }

    #[inline(never)]
    pub fn rounded_rect(&mut self, rect: Rect, radius: i32, color: u32, alpha: f32) {
        let radius = radius.min(rect.width / 2).min(rect.height / 2);
        for row in 0..rect.height {
            for column in 0..rect.width {
                // Distance outside the nearest corner circle, if any.
                let dx = (radius - 1 - column).max(column - (rect.width - radius)).max(0);
                let dy = (radius - 1 - row).max(row - (rect.height - radius)).max(0);
                if dx * dx + dy * dy <= radius * radius {
                    self.blend(rect.x + column, rect.y + row, color, alpha);
                }
            }
        }
    }

    /// Fills the triangle `a`, `b`, `c` (any winding).
    pub fn triangle(&mut self, a: (f32, f32), b: (f32, f32), c: (f32, f32), color: u32) {
        let min_x = a.0.min(b.0).min(c.0).floor() as i32;
        let max_x = a.0.max(b.0).max(c.0).ceil() as i32;
        let min_y = a.1.min(b.1).min(c.1).floor() as i32;
        let max_y = a.1.max(b.1).max(c.1).ceil() as i32;
        let edge = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let point = (x as f32 + 0.5, y as f32 + 0.5);
                let (w0, w1, w2) = (edge(b, c, point), edge(c, a, point), edge(a, b, point));
                let inside = (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
                if inside {
                    self.blend(x, y, color, 1.0);
                }
            }
        }
    }

    /// A clockwise circular arrow: an arc covering most of a circle whose arrowhead
    /// points back toward the start of the arc, as in a "restart" icon.
    pub fn circular_arrow(&mut self, cx: i32, cy: i32, radius: i32, color: u32) {
        // Angles in degrees, 0 = right, increasing clockwise on screen.
        const START: f32 = 25.0;
        const END: f32 = 318.0;
        let r = radius as f32;
        let mut angle = START;
        while angle <= END {
            let (sin, cos) = angle.to_radians().sin_cos();
            self.rect((cx as f32 + r * cos).round() as i32 - 1, (cy as f32 + r * sin).round() as i32 - 1, 3, 3, color);
            angle += 2.0;
        }
        let (sin, cos) = END.to_radians().sin_cos();
        let end = (cx as f32 + r * cos, cy as f32 + r * sin);
        let tangent = (-sin, cos);
        let normal = (cos, sin);
        let (length, half_width) = (r * 0.85, r * 0.62);
        self.triangle(
            (end.0 + tangent.0 * length, end.1 + tangent.1 * length),
            (end.0 + normal.0 * half_width, end.1 + normal.1 * half_width),
            (end.0 - normal.0 * half_width, end.1 - normal.1 * half_width),
            color,
        );
    }

    /// Four corner brackets around a square, the usual "full screen" glyph.
    pub fn corner_brackets(&mut self, cx: i32, cy: i32, half: i32, arm: i32, color: u32) {
        for (sx, sy) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
            let (corner_x, corner_y) = (cx + sx * half, cy + sy * half);
            let x0 = if sx < 0 { corner_x } else { corner_x - arm + 1 };
            let y0 = if sy < 0 { corner_y } else { corner_y - 2 };
            self.rect(x0, y0, arm, 3, color);
            let x1 = if sx < 0 { corner_x } else { corner_x - 2 };
            let y1 = if sy < 0 { corner_y } else { corner_y - arm + 1 };
            self.rect(x1, y1, 3, arm, color);
        }
    }

    /// The standard power symbol: a ring open at the top with a bar through the opening.
    pub fn power_symbol(&mut self, cx: i32, cy: i32, radius: i32, color: u32) {
        const GAP: f32 = 40.0;
        let r = radius as f32;
        let mut angle = 270.0 + GAP;
        while angle <= 270.0 + 360.0 - GAP {
            let (sin, cos) = angle.to_radians().sin_cos();
            self.rect((cx as f32 + r * cos).round() as i32 - 1, (cy as f32 + r * sin).round() as i32 - 1, 3, 3, color);
            angle += 2.0;
        }
        self.rect(cx - 1, cy - radius - 3, 3, radius + 3, color);
    }

    pub fn circle(&mut self, cx: i32, cy: i32, radius: i32, color: u32) {
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy <= radius * radius {
                    self.blend(cx + dx, cy + dy, color, 1.0);
                }
            }
        }
    }

    /// Right-facing arc segment used for the speaker's sound waves.
    pub fn arc(&mut self, cx: i32, cy: i32, radius: i32, color: u32) {
        for dy in -radius..=radius {
            for dx in 1..=radius {
                let distance = ((dx * dx + dy * dy) as f32).sqrt();
                // Keep a 55 degree wedge around the horizontal axis.
                if (distance - radius as f32).abs() < 1.2 && dy.abs() as f32 <= distance * 0.82 {
                    self.rect(cx + dx, cy + dy, 2, 2, color);
                }
            }
        }
    }

    /// Draws `text` in the built-in bold 5x7 font at [`TEXT_SCALE`].
    pub fn text(&mut self, text: &str, x: i32, y: i32, color: u32) {
        let mut cursor = x;
        for character in text.chars() {
            if let Some(glyph) = glyph(character) {
                for (row, bits) in glyph.iter().enumerate() {
                    for column in 0..GLYPH_WIDTH {
                        if bits & (1 << (GLYPH_WIDTH - 1 - column)) != 0 {
                            // Each lit cell is two units wide, which bolds the vertical strokes.
                            self.rect(
                                cursor + column * TEXT_SCALE,
                                y + row as i32 * TEXT_SCALE,
                                2 * TEXT_SCALE,
                                TEXT_SCALE,
                                color,
                            );
                        }
                    }
                }
            }
            cursor += GLYPH_ADVANCE;
        }
    }
}

pub const GLYPH_WIDTH: i32 = 5;
pub const GLYPH_HEIGHT: i32 = 7;
/// Screen pixels per grid cell.
pub const TEXT_SCALE: i32 = 2;
/// Distance between glyph origins: the design is one cell wider once bolded, plus a space.
pub const GLYPH_ADVANCE: i32 = (GLYPH_WIDTH + 2) * TEXT_SCALE;

/// Width in pixels of `text` as drawn by [`Canvas::text`] (no trailing space).
pub fn text_width(text: &str) -> i32 {
    text.chars().count() as i32 * GLYPH_ADVANCE - TEXT_SCALE
}

/// 5x7 glyphs for the digits and the few letters and symbols the dock uses.
fn glyph(character: char) -> Option<[u8; 7]> {
    Some(match character {
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        '%' => [0b11000, 0b11001, 0b00010, 0b00100, 0b01000, 0b10011, 0b00011],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        _ => return None,
    })
}
