//! The two 2D graphics engines. Each scanline is composed from the engine's
//! registers as they stand at the start of the line, so mid-frame register
//! changes (HBlank effects) show up the way they do on hardware. The 3D layer
//! is not drawn yet; BG0 in 3D mode is transparent.

use crate::system::memory::vram::{Region, VramMap};

pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 192;

const OPAQUE: u16 = 0x8000;

/// Everything one engine needs to draw a line.
pub struct Engine<'a> {
    pub regs: &'a [u8],
    pub palette: &'a [u8],
    pub oam: &'a [u8],
    pub vram: &'a [u8],
    pub map: &'a VramMap,
    pub is_b: bool,
    /// The 3D engine's colour and alpha buffers (engine A only).
    pub gx: Option<(&'a [u16], &'a [u8])>,
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

impl Engine<'_> {
    fn reg(&self, off: usize) -> u16 {
        u16_at(self.regs, off)
    }

    fn dispcnt(&self) -> u32 {
        u16_at(self.regs, 0) as u32 | (u16_at(self.regs, 2) as u32) << 16
    }

    fn bg_region(&self) -> Region {
        if self.is_b { Region::BgB } else { Region::BgA }
    }

    fn obj_region(&self) -> Region {
        if self.is_b { Region::ObjB } else { Region::ObjA }
    }

    fn vram8(&self, region: Region, off: usize) -> u8 {
        self.map.lookup(region, off).map_or(0, |i| self.vram[i])
    }

    /// `N` bytes starting at `off` with one bank lookup. They must not straddle a bank
    /// boundary, which naturally aligned fetches of 2, 4 or 8 bytes never do.
    fn vram_chunk<const N: usize>(&self, region: Region, off: usize) -> [u8; N] {
        match self.map.lookup(region, off) {
            Some(i) => self.vram[i..i + N].try_into().unwrap(),
            None => [0; N],
        }
    }

    fn vram16(&self, region: Region, off: usize) -> u16 {
        self.vram8(region, off) as u16 | (self.vram8(region, off + 1) as u16) << 8
    }

    /// BGR555 from the standard (non-extended) palette.
    fn palette_color(&self, index: usize) -> u16 {
        u16_at(self.palette, (index * 2) & 0x3FF)
    }

    fn bg_ext_color(&self, slot: usize, index: usize) -> u16 {
        let base = if self.is_b { self.map.bg_ext_b[slot] } else { self.map.bg_ext_a[slot] };
        if base == usize::MAX { 0 } else { u16_at(self.vram, base + index * 2) }
    }

    fn obj_ext_color(&self, index: usize) -> u16 {
        let base = if self.is_b { self.map.obj_ext_b } else { self.map.obj_ext_a };
        if base == usize::MAX { 0 } else { u16_at(self.vram, base + index * 2) }
    }
}

struct Layers {
    bg: [[u16; WIDTH]; 4],
    bg_prio: [u8; 4],
    obj: [u16; WIDTH],
    obj_prio: [u8; WIDTH],
    obj_semi: [bool; WIDTH],
    obj_win: [bool; WIDTH],
    /// BG0 carries the 3D image; its per-pixel alpha (31 = opaque).
    bg0_3d: bool,
    bg0_alpha: [u8; WIDTH],
}

/// Render one scanline to 0x00RRGGBB. `affine` holds the current reference
/// points of BG2 and BG3 as (x, y) in 8.8 fixed point.
pub fn render_line(e: &Engine, line: usize, affine: &[[i32; 2]; 2], out: &mut [u32]) {
    let dispcnt = e.dispcnt();
    if dispcnt & 0x80 != 0 {
        out.fill(0x00FF_FFFF);
        return;
    }
    match (dispcnt >> 16) & 3 {
        0 => out.fill(0x00FF_FFFF),
        1 => render_graphics(e, dispcnt, line, affine, out),
        2 if !e.is_b => {
            let block = ((dispcnt >> 18) & 3) as usize * 0x2_0000;
            for (x, px) in out.iter_mut().enumerate() {
                let off = block + (line * WIDTH + x) * 2;
                *px = bgr555_to_rgb(u16_at(e.vram, off));
            }
        }
        _ => out.fill(0),
    }
    apply_master_brightness(e, out);
}

fn render_graphics(e: &Engine, dispcnt: u32, line: usize, affine: &[[i32; 2]; 2], out: &mut [u32]) {
    let mut layers = Layers {
        bg: [[0; WIDTH]; 4],
        bg_prio: [0; 4],
        obj: [0; WIDTH],
        obj_prio: [4; WIDTH],
        obj_semi: [false; WIDTH],
        obj_win: [false; WIDTH],
        bg0_3d: false,
        bg0_alpha: [31; WIDTH],
    };
    let mode = dispcnt & 7;
    let mut bg_on = [false; 4];
    for i in 0..4 {
        layers.bg_prio[i] = (e.reg(0x08 + i * 2) & 3) as u8;
        bg_on[i] = dispcnt & (0x100 << i) != 0;
    }
    // Which BGs are text / affine / extended in each mode.
    #[derive(PartialEq, Clone, Copy)]
    enum Kind {
        Text,
        Affine,
        Extended,
        Large,
        None,
    }
    let kinds: [Kind; 4] = match mode {
        0 => [Kind::Text; 4],
        1 => [Kind::Text, Kind::Text, Kind::Text, Kind::Affine],
        2 => [Kind::Text, Kind::Text, Kind::Affine, Kind::Affine],
        3 => [Kind::Text, Kind::Text, Kind::Text, Kind::Extended],
        4 => [Kind::Text, Kind::Text, Kind::Affine, Kind::Extended],
        5 => [Kind::Text, Kind::Text, Kind::Extended, Kind::Extended],
        6 => [Kind::None, Kind::None, Kind::Large, Kind::None],
        _ => [Kind::None; 4],
    };
    for i in 0..4 {
        if !bg_on[i] {
            continue;
        }
        match kinds[i] {
            Kind::Text if i == 0 && dispcnt & 8 != 0 => {
                layers.bg0_3d = true;
                render_3d_layer(e, line, &mut layers);
            }
            Kind::Text => render_text_bg(e, dispcnt, i, line, &mut layers.bg[i]),
            Kind::Affine | Kind::Extended | Kind::Large => {
                render_affine_bg(e, dispcnt, i, kinds[i] != Kind::Affine, affine[i - 2], &mut layers.bg[i])
            }
            Kind::None => {}
        }
    }
    if dispcnt & 0x1000 != 0 {
        render_sprites(e, dispcnt, line, &mut layers);
    }
    compose(e, dispcnt, line, &layers, bg_on, out);
}

/// BG0 as the 3D image, scrolled horizontally by BG0HOFS.
fn render_3d_layer(e: &Engine, line: usize, layers: &mut Layers) {
    let Some((color, alpha)) = e.gx else { return };
    let hofs = (e.reg(0x10) & 0x1FF) as usize;
    for x in 0..WIDTH {
        let sx = (x + hofs) & 0x1FF;
        if sx >= WIDTH {
            continue;
        }
        let a = alpha[line * WIDTH + sx];
        if a != 0 {
            layers.bg[0][x] = OPAQUE | color[line * WIDTH + sx];
            layers.bg0_alpha[x] = a;
        }
    }
}

fn bgr555_to_rgb(c: u16) -> u32 {
    let (r, g, b) = ((c & 31) as u32, (c >> 5 & 31) as u32, (c >> 10 & 31) as u32);
    let scale = |v: u32| (v << 3) | (v >> 2);
    scale(r) << 16 | scale(g) << 8 | scale(b)
}

fn render_text_bg(e: &Engine, dispcnt: u32, i: usize, line: usize, out: &mut [u16; WIDTH]) {
    let cnt = e.reg(0x08 + i * 2) as usize;
    let (char_off, screen_off) = if e.is_b {
        (0, 0)
    } else {
        (((dispcnt >> 24) & 7) as usize * 0x1_0000, ((dispcnt >> 27) & 7) as usize * 0x1_0000)
    };
    let char_base = char_off + ((cnt >> 2) & 0xF) * 0x4000;
    let screen_base = screen_off + ((cnt >> 8) & 0x1F) * 0x800;
    let color256 = cnt & 0x80 != 0;
    let size = cnt >> 14;
    let hofs = (e.reg(0x10 + i * 4) & 0x1FF) as usize;
    let vofs = (e.reg(0x12 + i * 4) & 0x1FF) as usize;
    let width_mask = if size & 1 != 0 { 511 } else { 255 };
    let height_mask = if size & 2 != 0 { 511 } else { 255 };
    let ext_pal = dispcnt & (1 << 30) != 0;
    let ext_slot = if i < 2 && cnt & 0x2000 != 0 { i + 2 } else { i };
    let region = e.bg_region();

    let y = (line + vofs) & height_mask;
    let mut x = 0;
    while x < WIDTH {
        // One tile's worth of pixels at a time (the first and last may be partial).
        let sx = (x + hofs) & width_mask;
        let first = sx & 7;
        let run = (8 - first).min(WIDTH - x);
        let block = match size {
            1 => sx >> 8,
            2 => y >> 8,
            3 => (sx >> 8) + (y >> 8) * 2,
            _ => 0,
        };
        let entry_addr = screen_base + block * 0x800 + ((y & 0xFF) >> 3) * 64 + ((sx & 0xFF) >> 3) * 2;
        let entry = u16::from_le_bytes(e.vram_chunk(region, entry_addr)) as usize;
        let tile = entry & 0x3FF;
        let mut py = y & 7;
        if entry & 0x800 != 0 {
            py = 7 - py;
        }
        let mut tile_row = [0u8; 8];
        if color256 {
            tile_row = e.vram_chunk(region, char_base + tile * 64 + py * 8);
        } else {
            let packed: [u8; 4] = e.vram_chunk(region, char_base + tile * 32 + py * 4);
            for (k, b) in packed.into_iter().enumerate() {
                tile_row[k * 2] = b & 0xF;
                tile_row[k * 2 + 1] = b >> 4;
            }
        }
        if u64::from_le_bytes(tile_row) != 0 {
            let pal = entry >> 12;
            let flip = entry & 0x400 != 0;
            for (k, px) in out[x..x + run].iter_mut().enumerate() {
                let pxl = if flip { 7 - (first + k) } else { first + k };
                let idx = tile_row[pxl] as usize;
                if idx == 0 {
                    continue;
                }
                *px = OPAQUE
                    | if color256 {
                        if ext_pal { e.bg_ext_color(ext_slot, pal * 256 + idx) } else { e.palette_color(idx) }
                    } else {
                        e.palette_color(pal * 16 + idx)
                    };
            }
        }
        x += run;
    }
}

fn render_affine_bg(e: &Engine, dispcnt: u32, i: usize, extended: bool, origin: [i32; 2], out: &mut [u16; WIDTH]) {
    let cnt = e.reg(0x08 + i * 2) as usize;
    let params = if i == 2 { 0x20 } else { 0x30 };
    let pa = e.reg(params) as i16 as i32;
    let pc = e.reg(params + 4) as i16 as i32;
    let size = cnt >> 14;
    let wrap = cnt & 0x2000 != 0;
    let (char_off, screen_off) = if e.is_b {
        (0, 0)
    } else {
        (((dispcnt >> 24) & 7) as usize * 0x1_0000, ((dispcnt >> 27) & 7) as usize * 0x1_0000)
    };
    let char_base = char_off + ((cnt >> 2) & 0xF) * 0x4000;
    let map_base = screen_off + ((cnt >> 8) & 0x1F) * 0x800;
    let region = e.bg_region();
    let ext_pal = dispcnt & (1 << 30) != 0;

    // Extended BGs are bitmaps when BGCNT bit 7 is set.
    let bitmap = extended && cnt & 0x80 != 0;
    let direct = bitmap && cnt & 0x04 != 0;
    let (w, h) = if bitmap {
        match size {
            0 => (128, 128),
            1 => (256, 256),
            2 => (512, 256),
            _ => (512, 512),
        }
    } else {
        (128 << size, 128 << size)
    };
    let bitmap_base = ((cnt >> 8) & 0x1F) * 0x4000;

    let (mut fx, mut fy) = (origin[0], origin[1]);
    for px in out.iter_mut() {
        let (mut tx, mut ty) = (fx >> 8, fy >> 8);
        fx = fx.wrapping_add(pa);
        fy = fy.wrapping_add(pc);
        if tx < 0 || ty < 0 || tx >= w as i32 || ty >= h as i32 {
            if !wrap {
                continue;
            }
            tx = tx.rem_euclid(w as i32);
            ty = ty.rem_euclid(h as i32);
        }
        let (tx, ty) = (tx as usize, ty as usize);
        if bitmap {
            if direct {
                let c = e.vram16(region, bitmap_base + (ty * w + tx) * 2);
                if c & OPAQUE != 0 {
                    *px = c;
                }
            } else {
                let idx = e.vram8(region, bitmap_base + ty * w + tx) as usize;
                if idx != 0 {
                    *px = OPAQUE | e.palette_color(idx);
                }
            }
            continue;
        }
        let tiles_per_row = w / 8;
        let (tile, mut px_in, mut py_in, pal) = if extended {
            let entry = e.vram16(region, map_base + ((ty >> 3) * tiles_per_row + (tx >> 3)) * 2) as usize;
            let (mut ix, mut iy) = (tx & 7, ty & 7);
            if entry & 0x400 != 0 {
                ix = 7 - ix;
            }
            if entry & 0x800 != 0 {
                iy = 7 - iy;
            }
            (entry & 0x3FF, ix, iy, entry >> 12)
        } else {
            let t = e.vram8(region, map_base + (ty >> 3) * tiles_per_row + (tx >> 3)) as usize;
            (t, tx & 7, ty & 7, 0)
        };
        px_in &= 7;
        py_in &= 7;
        let idx = e.vram8(region, char_base + tile * 64 + py_in * 8 + px_in) as usize;
        if idx != 0 {
            *px = OPAQUE
                | if ext_pal && extended { e.bg_ext_color(i, pal * 256 + idx) } else { e.palette_color(idx) };
        }
    }
}

const SPRITE_SIZES: [[(usize, usize); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)],
    [(16, 8), (32, 8), (32, 16), (64, 32)],
    [(8, 16), (8, 32), (16, 32), (32, 64)],
];

fn render_sprites(e: &Engine, dispcnt: u32, line: usize, layers: &mut Layers) {
    let region = e.obj_region();
    let one_d = dispcnt & 0x10 != 0;
    let boundary_shift = ((dispcnt >> 20) & 3) as usize;
    let ext_pal = dispcnt & (1 << 31) != 0;

    for n in 0..128 {
        let a0 = u16_at(e.oam, n * 8) as usize;
        let a1 = u16_at(e.oam, n * 8 + 2) as usize;
        let a2 = u16_at(e.oam, n * 8 + 4) as usize;
        let affine = a0 & 0x100 != 0;
        if !affine && a0 & 0x200 != 0 {
            continue;
        }
        let mode = (a0 >> 10) & 3;
        if mode == 3 {
            continue; // bitmap sprites are not supported
        }
        let shape = (a0 >> 14) & 3;
        if shape == 3 {
            continue;
        }
        let (w, h) = SPRITE_SIZES[shape][(a1 >> 14) & 3];
        let (bw, bh) = if affine && a0 & 0x200 != 0 { (w * 2, h * 2) } else { (w, h) };
        let y_rel = line.wrapping_sub(a0 & 0xFF) & 0xFF;
        if y_rel >= bh {
            continue;
        }
        let x0 = ((a1 & 0x1FF) as i32) << 23 >> 23;
        let color256 = a0 & 0x2000 != 0;
        let tile = a2 & 0x3FF;
        let prio = ((a2 >> 10) & 3) as u8;
        let pal = a2 >> 12;

        let (pa, pb, pc, pd) = if affine {
            let g = ((a1 >> 9) & 31) * 32;
            let p = |k: usize| u16_at(e.oam, g + 6 + k * 8) as i16 as i32;
            (p(0), p(1), p(2), p(3))
        } else {
            (0, 0, 0, 0)
        };

        let mut row_key = (usize::MAX, 0);
        let mut row = [0u8; 8];
        for ix in 0..bw {
            let sx = x0 + ix as i32;
            if !(0..WIDTH as i32).contains(&sx) {
                continue;
            }
            let (tx, ty) = if affine {
                let dx = ix as i32 - (bw / 2) as i32;
                let dy = y_rel as i32 - (bh / 2) as i32;
                let tx = ((pa * dx + pb * dy) >> 8) + (w / 2) as i32;
                let ty = ((pc * dx + pd * dy) >> 8) + (h / 2) as i32;
                if tx < 0 || ty < 0 || tx >= w as i32 || ty >= h as i32 {
                    continue;
                }
                (tx as usize, ty as usize)
            } else {
                let tx = if a1 & 0x1000 != 0 { w - 1 - ix } else { ix };
                let ty = if a1 & 0x2000 != 0 { h - 1 - y_rel } else { y_rel };
                (tx, ty)
            };
            let (tile_x, tile_y) = (tx >> 3, ty >> 3);
            let bytes_per_tile = if color256 { 64 } else { 32 };
            let base = if one_d {
                (tile << (5 + boundary_shift)) + (tile_y * (w / 8) + tile_x) * bytes_per_tile
            } else {
                (tile + tile_y * 32 + tile_x * if color256 { 2 } else { 1 }) * 32
            };
            let (px, py) = (tx & 7, ty & 7);
            // The row of the tile this pixel is in; neighbouring pixels share it.
            if row_key != (base, py) {
                row_key = (base, py);
                if color256 {
                    row = e.vram_chunk(region, base + py * 8);
                } else {
                    let packed: [u8; 4] = e.vram_chunk(region, base + py * 4);
                    for (k, b) in packed.into_iter().enumerate() {
                        row[k * 2] = b & 0xF;
                        row[k * 2 + 1] = b >> 4;
                    }
                }
            }
            let idx = row[px] as usize;
            if idx == 0 {
                continue;
            }
            let x = sx as usize;
            if mode == 2 {
                layers.obj_win[x] = true;
                continue;
            }
            if layers.obj[x] & OPAQUE != 0 && layers.obj_prio[x] <= prio {
                continue;
            }
            let color = if color256 {
                if ext_pal { e.obj_ext_color(pal * 256 + idx) } else { e.palette_color(256 + idx) }
            } else {
                e.palette_color(256 + pal * 16 + idx)
            };
            layers.obj[x] = OPAQUE | color;
            layers.obj_prio[x] = prio;
            layers.obj_semi[x] = mode == 1;
        }
    }
}

fn in_window_range(coord: usize, start: usize, end: usize) -> bool {
    let end = if end == 0 { 256 } else { end };
    if start <= end { coord >= start && coord < end } else { coord >= start || coord < end }
}

fn compose(e: &Engine, dispcnt: u32, line: usize, l: &Layers, bg_on: [bool; 4], out: &mut [u32]) {
    let win_on = dispcnt >> 13 & 7;
    let win_in = e.reg(0x48);
    let win_out = e.reg(0x4A);
    let win_rect = |n: usize| {
        let h = e.reg(0x40 + n * 2);
        let v = e.reg(0x44 + n * 2);
        ((h >> 8) as usize, (h & 0xFF) as usize, (v >> 8) as usize, (v & 0xFF) as usize)
    };
    let (w0, w1) = (win_rect(0), win_rect(1));
    let win0_line = win_on & 1 != 0 && in_window_range(line, w0.2, w0.3);
    let win1_line = win_on & 2 != 0 && in_window_range(line, w1.2, w1.3);

    let bldcnt = e.reg(0x50) as usize;
    let effect = (bldcnt >> 6) & 3;
    let alpha = e.reg(0x52);
    let (eva, evb) = ((alpha & 0x1F).min(16) as u32, ((alpha >> 8) & 0x1F).min(16) as u32);
    let evy = (e.reg(0x54) & 0x1F).min(16) as u32;
    let backdrop = e.palette_color(0);

    // The enabled backgrounds, front to back. A sprite goes in front of the backgrounds
    // of its own priority, so it slots in ahead of the first one it doesn't lose to.
    let mut order = [0usize; 4];
    let mut count_bg = 0;
    for prio in 0..4u8 {
        for b in 0..4 {
            if bg_on[b] && l.bg_prio[b] == prio {
                order[count_bg] = b;
                count_bg += 1;
            }
        }
    }
    let order = &order[..count_bg];

    // The colour of the pixel at `x`, in full: windows, the two topmost layers, and any blending.
    let pixel = |x: usize| -> u16 {
        // Which layers (bits 0-4) and effects (bit 5) are enabled at this pixel.
        let mask = if win_on == 0 {
            0x3F
        } else if win0_line && in_window_range(x, w0.0, w0.1) {
            win_in & 0x3F
        } else if win1_line && in_window_range(x, w1.0, w1.1) {
            (win_in >> 8) & 0x3F
        } else if win_on & 4 != 0 && l.obj_win[x] {
            (win_out >> 8) & 0x3F
        } else {
            win_out & 0x3F
        } as usize;

        // Collect the two topmost visible pixels (layer 4 = sprite, 5 = backdrop).
        let mut found: [(u16, usize); 2] = [(backdrop, 5), (backdrop, 5)];
        let mut count = 0;
        let mut obj = (mask & 0x10 != 0 && l.obj[x] & OPAQUE != 0).then(|| (l.obj_prio[x], l.obj[x] & 0x7FFF));
        for &b in order {
            if mask & (1 << b) == 0 || l.bg[b][x] & OPAQUE == 0 {
                continue;
            }
            if let Some((_, color)) = obj.filter(|&(prio, _)| prio <= l.bg_prio[b]) {
                found[count] = (color, 4);
                count += 1;
                obj = None;
                if count == 2 {
                    break;
                }
            }
            found[count] = (l.bg[b][x] & 0x7FFF, b);
            count += 1;
            if count == 2 {
                break;
            }
        }
        if let (Some((_, color)), true) = (obj, count < 2) {
            found[count] = (color, 4);
        }
        let (top, top_layer) = found[0];
        let (below, below_layer) = found[1];
        let first_target = bldcnt >> top_layer & 1 != 0;
        let second_target = bldcnt >> 8 >> below_layer & 1 != 0;
        let effects_on = mask & 0x20 != 0;

        let semi = top_layer == 4 && l.obj_semi[x];
        if top_layer == 0 && l.bg0_3d && l.bg0_alpha[x] < 31 {
            // Translucent 3D pixels blend with whatever is behind them.
            blend_3d(top, below, l.bg0_alpha[x] as u32)
        } else if semi && second_target && effects_on {
            blend_alpha(top, below, eva, evb)
        } else if effects_on && first_target {
            match effect {
                1 if second_target => blend_alpha(top, below, eva, evb),
                2 => fade(top, evy, true),
                3 => fade(top, evy, false),
                _ => top,
            }
        } else {
            top
        }
    };

    if win_on != 0 || effect != 0 {
        for x in 0..WIDTH {
            out[x] = bgr555_to_rgb(pixel(x));
        }
        return;
    }
    // No windows and no brightness or blend effect: the top pixel is the answer, unless it is
    // a translucent sprite or 3D pixel, which blend regardless.
    for x in 0..WIDTH {
        let obj = l.obj[x];
        let obj_prio = if obj & OPAQUE != 0 { l.obj_prio[x] } else { u8::MAX };
        let mut top = None;
        for &b in order {
            if obj_prio <= l.bg_prio[b] {
                break;
            }
            let c = l.bg[b][x];
            if c & OPAQUE != 0 {
                top = Some((c, b));
                break;
            }
        }
        let (color, layer) = top.unwrap_or(if obj_prio != u8::MAX { (obj, 4) } else { (backdrop, 5) });
        let translucent = match layer {
            4 => l.obj_semi[x],
            0 => l.bg0_3d && l.bg0_alpha[x] < 31,
            _ => false,
        };
        out[x] = bgr555_to_rgb(if translucent { pixel(x) } else { color & 0x7FFF });
    }
}

fn blend_3d(top: u16, below: u16, alpha: u32) -> u16 {
    let ch = |shift: u32| {
        let (x, y) = ((top as u32 >> shift) & 31, (below as u32 >> shift) & 31);
        (x * (alpha + 1) + y * (31 - alpha)) / 32
    };
    (ch(0) | ch(5) << 5 | ch(10) << 10) as u16
}

fn blend_alpha(a: u16, b: u16, eva: u32, evb: u32) -> u16 {
    let ch = |shift: u32| {
        let (x, y) = ((a as u32 >> shift) & 31, (b as u32 >> shift) & 31);
        ((x * eva + y * evb) / 16).min(31)
    };
    (ch(0) | ch(5) << 5 | ch(10) << 10) as u16
}

fn fade(c: u16, evy: u32, up: bool) -> u16 {
    let ch = |shift: u32| {
        let v = (c as u32 >> shift) & 31;
        if up { v + (31 - v) * evy / 16 } else { v - v * evy / 16 }
    };
    (ch(0) | ch(5) << 5 | ch(10) << 10) as u16
}

fn apply_master_brightness(e: &Engine, out: &mut [u32]) {
    let v = e.reg(0x6C);
    let factor = (v & 0x1F).min(16) as u32;
    let mode = v >> 14;
    if factor == 0 || !(mode == 1 || mode == 2) {
        return;
    }
    for px in out.iter_mut() {
        let ch = |shift: u32| {
            let c = (*px >> shift) & 0xFF;
            if mode == 1 { c + (255 - c) * factor / 16 } else { c - c * factor / 16 }
        };
        *px = ch(16) << 16 | ch(8) << 8 | ch(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        regs: Vec<u8>,
        palette: Vec<u8>,
        oam: Vec<u8>,
        vram: Vec<u8>,
        map: VramMap,
    }

    impl Fixture {
        fn new() -> Fixture {
            // Bank A as BG-A and bank B as OBJ-A, both at offset 0.
            let mut cnt = [0u8; 9];
            cnt[0] = 0x81;
            cnt[1] = 0x82;
            Fixture {
                regs: vec![0; 0x70],
                palette: vec![0; 0x400],
                oam: vec![0; 0x400],
                vram: vec![0; 0xA4000],
                map: VramMap::rebuild(&cnt),
            }
        }

        fn engine(&self) -> Engine<'_> {
            Engine { regs: &self.regs, palette: &self.palette, oam: &self.oam, vram: &self.vram, map: &self.map, is_b: false, gx: None }
        }

        fn set16(buf: &mut [u8], off: usize, v: u16) {
            buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
        }

        fn render(&self, line: usize) -> Vec<u32> {
            let mut out = vec![0; WIDTH];
            render_line(&self.engine(), line, &[[0; 2]; 2], &mut out);
            out
        }
    }

    #[test]
    fn forced_blank_is_white() {
        let mut f = Fixture::new();
        Fixture::set16(&mut f.regs, 0, 0x80);
        assert!(f.render(0).iter().all(|&p| p == 0xFFFFFF));
    }

    #[test]
    fn backdrop_color_fills_empty_screen() {
        let mut f = Fixture::new();
        Fixture::set16(&mut f.regs, 0, 0x0000);
        Fixture::set16(&mut f.regs, 2, 1); // display mode 1
        Fixture::set16(&mut f.palette, 0, 0x001F); // red backdrop
        assert!(f.render(5).iter().all(|&p| p == 0xFF0000));
    }

    #[test]
    fn text_bg_draws_a_tile() {
        let mut f = Fixture::new();
        Fixture::set16(&mut f.regs, 0, 0x0100); // BG0 on, mode 0
        Fixture::set16(&mut f.regs, 2, 1);
        Fixture::set16(&mut f.regs, 0x08, 0x0100); // screen base block 1, char base 0
        Fixture::set16(&mut f.palette, 2, 0x03E0); // color 1 = green
        // Tile 1: first pixel row all color 1.
        f.vram[32..36].copy_from_slice(&[0x11; 4]);
        // Map entry (0,0) -> tile 1.
        Fixture::set16(&mut f.vram, 0x800, 1);
        let line = f.render(0);
        assert_eq!(line[0], 0x00FF00);
        assert_eq!(line[7], 0x00FF00);
        assert_eq!(line[8], 0); // tile 0 is transparent -> black backdrop
    }

    #[test]
    fn horizontal_flip_mirrors_tile() {
        let mut f = Fixture::new();
        Fixture::set16(&mut f.regs, 0, 0x0100);
        Fixture::set16(&mut f.regs, 2, 1);
        Fixture::set16(&mut f.regs, 0x08, 0x0100);
        Fixture::set16(&mut f.palette, 2, 0x001F);
        f.vram[32] = 0x01; // only the leftmost pixel of row 0 is set
        Fixture::set16(&mut f.vram, 0x800, 1 | 0x400);
        let line = f.render(0);
        assert_eq!(line[7], 0xFF0000);
        assert_eq!(line[0], 0);
    }

    #[test]
    fn sprite_appears_over_background() {
        let mut f = Fixture::new();
        Fixture::set16(&mut f.regs, 0, 0x1000 | 0x10); // OBJ on, 1D mapping
        Fixture::set16(&mut f.regs, 2, 1);
        Fixture::set16(&mut f.palette, 0x202, 0x7C00); // OBJ color 1 = blue
        f.vram[0x20000..0x20004].copy_from_slice(&[0x11; 4]); // bank B (OBJ) tile 0 row 0
        // The OBJ region maps bank B at 0, so write through the map offset instead.
        let off = f.map.lookup(Region::ObjA, 0).unwrap();
        f.vram[off..off + 4].copy_from_slice(&[0x11; 4]);
        Fixture::set16(&mut f.oam, 0, 10); // y = 10, 16 colors, square 8x8
        Fixture::set16(&mut f.oam, 2, 20); // x = 20
        Fixture::set16(&mut f.oam, 4, 0); // tile 0
        let line = f.render(10);
        assert_eq!(line[20], 0x0000FF);
        assert_eq!(line[28], 0);
        assert_eq!(f.render(11)[20], 0);
    }

    #[test]
    fn alpha_blend_mixes_top_two_layers() {
        assert_eq!(blend_alpha(0x001F, 0x7C00, 8, 8), 0x3C0F);
        assert_eq!(fade(0x0000, 16, true), 0x7FFF);
        assert_eq!(fade(0x7FFF, 16, false), 0);
    }

    #[test]
    fn master_brightness_fades_to_white() {
        let mut f = Fixture::new();
        Fixture::set16(&mut f.regs, 2, 1);
        Fixture::set16(&mut f.regs, 0x6C, 0x4000 | 16);
        assert!(f.render(0).iter().all(|&p| p == 0xFFFFFF));
    }

    #[test]
    fn window_restricts_layers() {
        let mut f = Fixture::new();
        Fixture::set16(&mut f.regs, 0, 0x0100 | 0x2000); // BG0 + window 0
        Fixture::set16(&mut f.regs, 2, 1);
        Fixture::set16(&mut f.regs, 0x08, 0x0100);
        Fixture::set16(&mut f.palette, 2, 0x03E0);
        f.vram[32..36].copy_from_slice(&[0x11; 4]);
        for i in 0..32 {
            Fixture::set16(&mut f.vram, 0x800 + i * 2, 1);
        }
        Fixture::set16(&mut f.regs, 0x40, 8 << 8 | 16); // window 0 x: 8..16
        Fixture::set16(&mut f.regs, 0x44, 192); // y: 0..192
        Fixture::set16(&mut f.regs, 0x48, 0x01); // BG0 visible inside
        Fixture::set16(&mut f.regs, 0x4A, 0x00); // nothing outside
        let line = f.render(0);
        assert_eq!(line[4], 0);
        assert_eq!(line[10], 0x00FF00);
        assert_eq!(line[20], 0);
    }

    /// The original per-pixel layer search, kept to check the faster one against.
    fn compose_reference(e: &Engine, dispcnt: u32, line: usize, l: &Layers, bg_on: [bool; 4], out: &mut [u32]) {
        let win_on = dispcnt >> 13 & 7;
        let win_in = e.reg(0x48);
        let win_out = e.reg(0x4A);
        let win_rect = |n: usize| {
            let h = e.reg(0x40 + n * 2);
            let v = e.reg(0x44 + n * 2);
            ((h >> 8) as usize, (h & 0xFF) as usize, (v >> 8) as usize, (v & 0xFF) as usize)
        };
        let (w0, w1) = (win_rect(0), win_rect(1));
        let win0_line = win_on & 1 != 0 && in_window_range(line, w0.2, w0.3);
        let win1_line = win_on & 2 != 0 && in_window_range(line, w1.2, w1.3);

        let bldcnt = e.reg(0x50) as usize;
        let effect = (bldcnt >> 6) & 3;
        let alpha = e.reg(0x52);
        let (eva, evb) = ((alpha & 0x1F).min(16) as u32, ((alpha >> 8) & 0x1F).min(16) as u32);
        let evy = (e.reg(0x54) & 0x1F).min(16) as u32;
        let backdrop = e.palette_color(0);

        for x in 0..WIDTH {
            // Which layers (bits 0-4) and effects (bit 5) are enabled at this pixel.
            let mask = if win_on == 0 {
                0x3F
            } else if win0_line && in_window_range(x, w0.0, w0.1) {
                win_in & 0x3F
            } else if win1_line && in_window_range(x, w1.0, w1.1) {
                (win_in >> 8) & 0x3F
            } else if win_on & 4 != 0 && l.obj_win[x] {
                (win_out >> 8) & 0x3F
            } else {
                win_out & 0x3F
            } as usize;

            // Collect the two topmost visible pixels (layer 4 = sprite, 5 = backdrop).
            let mut found: [(u16, usize); 2] = [(backdrop, 5), (backdrop, 5)];
            let mut count = 0;
            'search: for prio in 0..4u8 {
                if mask & 0x10 != 0 && l.obj[x] & OPAQUE != 0 && l.obj_prio[x] == prio {
                    found[count] = (l.obj[x] & 0x7FFF, 4);
                    count += 1;
                    if count == 2 {
                        break 'search;
                    }
                }
                for b in 0..4 {
                    if bg_on[b] && mask & (1 << b) != 0 && l.bg_prio[b] == prio && l.bg[b][x] & OPAQUE != 0 {
                        found[count] = (l.bg[b][x] & 0x7FFF, b);
                        count += 1;
                        if count == 2 {
                            break 'search;
                        }
                    }
                }
            }
            let (top, top_layer) = found[0];
            let (below, below_layer) = found[1];
            let first_target = bldcnt >> top_layer & 1 != 0;
            let second_target = bldcnt >> 8 >> below_layer & 1 != 0;
            let effects_on = mask & 0x20 != 0;

            let semi = top_layer == 4 && l.obj_semi[x];
            let color = if top_layer == 0 && l.bg0_3d && l.bg0_alpha[x] < 31 {
                // Translucent 3D pixels blend with whatever is behind them.
                blend_3d(top, below, l.bg0_alpha[x] as u32)
            } else if semi && second_target && effects_on {
                blend_alpha(top, below, eva, evb)
            } else if effects_on && first_target {
                match effect {
                    1 if second_target => blend_alpha(top, below, eva, evb),
                    2 => fade(top, evy, true),
                    3 => fade(top, evy, false),
                    _ => top,
                }
            } else {
                top
            };
            out[x] = bgr555_to_rgb(color);
        }
    }

    #[test]
    fn composition_matches_the_plain_layer_search() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as u32
        };
        for case in 0..3000 {
            let mut fixture = Fixture::new();
            for b in fixture.regs.iter_mut().chain(fixture.palette.iter_mut()) {
                *b = next() as u8;
            }
            // Mostly the simple cases: no windows, no effects.
            let mut dispcnt = next() & 0xE000_FF08;
            if case % 3 != 0 {
                dispcnt &= !0xE000;
            }
            if case % 4 != 0 {
                fixture.regs[0x50] &= 0x3F;
                fixture.regs[0x51] &= 0x3F;
            }
            let mut layers = Layers {
                bg: [[0; WIDTH]; 4],
                bg_prio: [0; 4],
                obj: [0; WIDTH],
                obj_prio: [4; WIDTH],
                obj_semi: [false; WIDTH],
                obj_win: [false; WIDTH],
                bg0_3d: case % 7 == 0,
                bg0_alpha: [31; WIDTH],
            };
            let bg_on = [next() % 4 != 0, next() % 4 != 0, next() % 4 != 0, next() % 4 != 0];
            for b in 0..4 {
                layers.bg_prio[b] = (next() % 4) as u8;
                for x in 0..WIDTH {
                    layers.bg[b][x] = if next() % 3 == 0 { 0 } else { OPAQUE | (next() & 0x7FFF) as u16 };
                }
            }
            for x in 0..WIDTH {
                if next() % 3 == 0 {
                    layers.obj[x] = OPAQUE | (next() & 0x7FFF) as u16;
                    layers.obj_prio[x] = (next() % 4) as u8;
                    layers.obj_semi[x] = case % 5 == 0 && next() % 4 == 0;
                }
                layers.obj_win[x] = next() % 4 == 0;
                layers.bg0_alpha[x] = (next() % 32) as u8;
            }
            let line = (next() % 192) as usize;
            let engine = fixture.engine();
            let (mut fast, mut slow) = ([0u32; WIDTH], [0u32; WIDTH]);
            compose(&engine, dispcnt, line, &layers, bg_on, &mut fast);
            compose_reference(&engine, dispcnt, line, &layers, bg_on, &mut slow);
            assert!(fast == slow, "case {case}: pictures differ");
        }
    }
}
