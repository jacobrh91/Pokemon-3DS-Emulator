//! Drawing the picture: scanline rendering of both 2D engines and the 3D engine's frame.

use super::*;

impl Hardware {
    /// At VBlank the 3D engine swaps buffers and draws the queued frame.
    pub fn gx_vblank(&mut self) {
        let mem = crate::video::gx::TextureMem { vram: &self.vram, map: &self.vram_map };
        self.gx.vblank(&self.io[ARM9][..0x400], &mem);
    }

    /// Draw one visible scanline of both engines into the frame buffer.
    pub fn render_scanline(&mut self, line: usize) {
        if line == 0 {
            for engine in 0..2 {
                for bg in 0..2 {
                    for axis in 0..2 {
                        self.affine_cur[engine][bg][axis] = self.affine_reg(engine, bg, axis);
                    }
                }
            }
        }
        let powcnt = u16::from_le_bytes([self.io[ARM9][0x304], self.io[ARM9][0x305]]);
        let a_on_top = powcnt & 0x8000 != 0;
        for engine in 0..2 {
            if self.skip_render {
                break;
            }
            let base = engine * 0x1000;
            let pal = engine * 0x400;
            let view = gpu::Engine {
                regs: &self.io[ARM9][base..base + 0x70],
                palette: &self.palette[pal..pal + 0x400],
                oam: &self.oam[pal..pal + 0x400],
                vram: &self.vram,
                map: &self.vram_map,
                is_b: engine == 1,
                gx: (engine == 0).then(|| (&self.gx.color_buf[..], &self.gx.alpha_buf[..])),
            };
            let on_top = (engine == 0) == a_on_top;
            let start = (if on_top { 0 } else { gpu::HEIGHT } + line) * gpu::WIDTH;
            let enabled = powcnt & if engine == 0 { 0x0002 } else { 0x0200 } != 0;
            let out = &mut self.frame[start..start + gpu::WIDTH];
            if enabled {
                gpu::render_line(&view, line, &self.affine_cur[engine], out);
            } else {
                out.fill(0x00FF_FFFF);
            }
        }
        for engine in 0..2 {
            for bg in 0..2 {
                let base = engine * 0x1000 + 0x20 + bg * 0x10;
                let pb = i16::from_le_bytes([self.io[ARM9][base + 2], self.io[ARM9][base + 3]]) as i32;
                let pd = i16::from_le_bytes([self.io[ARM9][base + 6], self.io[ARM9][base + 7]]) as i32;
                let cur = &mut self.affine_cur[engine][bg];
                cur[0] = cur[0].wrapping_add(pb);
                cur[1] = cur[1].wrapping_add(pd);
            }
        }
    }

    /// A background reference point register, sign-extended from 28 bits.
    pub(in crate::system) fn affine_reg(&self, engine: usize, bg: usize, axis: usize) -> i32 {
        let off = engine * 0x1000 + 0x28 + bg * 0x10 + axis * 4;
        let v = u32::from_le_bytes(self.io[ARM9][off..off + 4].try_into().unwrap());
        ((v << 4) as i32) >> 4
    }
}
