//! The 3D engine: a geometry processor (matrix stacks, lighting, polygon
//! assembly, clipping) feeding a software rasterizer.
//!
//! Commands run as soon as they are written, except that SWAP_BUFFERS holds
//! back everything after it until the next VBlank, as the real engine does.
//! The rasterized frame is kept until the next swap and is shown as BG0 of
//! engine A. Not modelled: edge marking, fog, anti-aliasing, wireframe
//! polygons, shadow polygons, specular lighting and the rear-plane bitmap.

use std::collections::VecDeque;

use crate::system::memory::vram::VramMap;

pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 192;

type Mat = [f32; 16];

const IDENTITY: Mat = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];

/// a × b, with matrices stored column-major.
fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut out = [0.0; 16];
    for col in 0..4 {
        for row in 0..4 {
            out[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    out
}

fn transform(m: &Mat, v: [f32; 4]) -> [f32; 4] {
    let mut out = [0.0; 4];
    for (row, o) in out.iter_mut().enumerate() {
        *o = (0..4).map(|k| m[k * 4 + row] * v[k]).sum();
    }
    out
}

fn translation(x: f32, y: f32, z: f32) -> Mat {
    let mut m = IDENTITY;
    m[12] = x;
    m[13] = y;
    m[14] = z;
    m
}

fn scaling(x: f32, y: f32, z: f32) -> Mat {
    let mut m = IDENTITY;
    m[0] = x;
    m[5] = y;
    m[10] = z;
    m
}

/// 1.19.12 fixed point.
fn fx12(v: u32) -> f32 {
    v as i32 as f32 / 4096.0
}

fn sext10(v: u32) -> i32 {
    ((v & 0x3FF) << 22) as i32 >> 22
}

fn sext16(v: u32) -> i32 {
    (v & 0xFFFF) as u16 as i16 as i32
}

fn rgb555(c: u32) -> [f32; 3] {
    [(c & 31) as f32, (c >> 5 & 31) as f32, (c >> 10 & 31) as f32]
}

/// Number of 32-bit parameters a geometry command takes.
fn param_count(cmd: u8) -> usize {
    match cmd {
        0x00 | 0x11 | 0x15 | 0x41 => 0,
        0x10 | 0x12 | 0x13 | 0x14 | 0x20 | 0x21 | 0x22 | 0x24..=0x2B | 0x30..=0x33 | 0x40 | 0x50 | 0x60 | 0x72 => 1,
        0x23 | 0x71 => 2,
        0x1B | 0x1C | 0x70 => 3,
        0x1A => 9,
        0x17 | 0x19 => 12,
        0x16 | 0x18 => 16,
        0x34 => 32,
        _ => 0,
    }
}

#[derive(Clone, Copy, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
struct Vertex {
    clip: [f32; 4],
    color: [f32; 3],
    tex: [f32; 2],
}

#[derive(Clone)]
#[derive(serde::Serialize, serde::Deserialize)]
struct Polygon {
    verts: Vec<Vertex>,
    attr: u32,
    tex_param: u32,
    pltt_base: u32,
    viewport: [u8; 4],
}

#[derive(Clone, Copy, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
struct Light {
    vector: [f32; 3],
    color: [f32; 3],
}

/// What the rasterizer reads out of VRAM.
pub struct TextureMem<'a> {
    pub vram: &'a [u8],
    pub map: &'a VramMap,
}

impl TextureMem<'_> {
    fn image8(&self, addr: usize) -> u8 {
        match self.map.tex_image[(addr >> 17) & 3] {
            usize::MAX => 0,
            base => self.vram[base + (addr & 0x1_FFFF)],
        }
    }

    fn image16(&self, addr: usize) -> u16 {
        self.image8(addr) as u16 | (self.image8(addr + 1) as u16) << 8
    }

    fn palette(&self, addr: usize) -> u16 {
        match self.map.tex_palette.get(addr >> 14) {
            Some(&base) if base != usize::MAX => {
                let o = base + (addr & 0x3FFF);
                u16::from_le_bytes([self.vram[o], self.vram[o + 1]])
            }
            _ => 0,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Gx {
    /// Fast-forward: process geometry but don't rasterize it.
    #[serde(skip)]
    pub skip_draw: bool,
    mode: u8,
    proj: Mat,
    pos: Mat,
    vec: Mat,
    tex: Mat,
    proj_stack: Mat,
    tex_stack: Mat,
    pos_stack: [Mat; 32],
    vec_stack: [Mat; 32],
    stack_ptr: i32,
    pub stack_error: bool,
    clip: Mat,
    clip_dirty: bool,

    color: [f32; 3],
    texcoord: [f32; 2],
    last_vertex: [f32; 3],
    attr_pending: u32,
    attr: u32,
    tex_param: u32,
    pltt_base: u32,
    diffuse: [f32; 3],
    ambient: [f32; 3],
    emission: [f32; 3],
    lights: [Light; 4],
    viewport: [u8; 4],

    prim: Option<u8>,
    verts: Vec<Vertex>,
    strip_polys: usize,
    polygons: Vec<Polygon>,

    packed: [u8; 4],
    packed_pos: usize,
    params: Vec<u32>,
    port_params: Vec<(u8, Vec<u32>)>,

    deferred: VecDeque<(u8, Vec<u32>)>,
    pub swap_pending: bool,
    swap_w_buffer: bool,
    pub box_result: bool,
    /// POS_TEST / VEC_TEST results, read back through GX registers.
    pub test_result: [i32; 4],

    render_polys: Vec<Polygon>,
    w_buffer: bool,
    /// Rendered image: BGR555 colour and alpha (0 = nothing drawn).
    pub color_buf: Vec<u16>,
    pub alpha_buf: Vec<u8>,
    depth: Vec<f32>,
}

impl Gx {
    pub fn new() -> Gx {
        Gx {
            skip_draw: false,
            mode: 0,
            proj: IDENTITY,
            pos: IDENTITY,
            vec: IDENTITY,
            tex: IDENTITY,
            proj_stack: IDENTITY,
            tex_stack: IDENTITY,
            pos_stack: [IDENTITY; 32],
            vec_stack: [IDENTITY; 32],
            stack_ptr: 0,
            stack_error: false,
            clip: IDENTITY,
            clip_dirty: true,
            color: [31.0; 3],
            texcoord: [0.0; 2],
            last_vertex: [0.0; 3],
            attr_pending: 0,
            attr: 0,
            tex_param: 0,
            pltt_base: 0,
            diffuse: [0.0; 3],
            ambient: [0.0; 3],
            emission: [0.0; 3],
            lights: [Light::default(); 4],
            viewport: [0, 0, 255, 191],
            prim: None,
            verts: Vec::new(),
            strip_polys: 0,
            polygons: Vec::new(),
            packed: [0; 4],
            packed_pos: 4,
            params: Vec::new(),
            port_params: Vec::new(),
            deferred: VecDeque::new(),
            swap_pending: false,
            swap_w_buffer: false,
            box_result: true,
            test_result: [0; 4],
            render_polys: Vec::new(),
            w_buffer: false,
            color_buf: vec![0; WIDTH * HEIGHT],
            alpha_buf: vec![0; WIDTH * HEIGHT],
            depth: vec![1.0; WIDTH * HEIGHT],
        }
    }

    /// GXSTAT bits the geometry engine owns: box-test result and stack state.
    pub fn status(&self) -> u32 {
        (self.box_result as u32) << 1
            | ((self.stack_ptr.clamp(0, 31) as u32) << 8)
            | (self.stack_error as u32) << 15
    }

    pub fn clear_stack_error(&mut self) {
        self.stack_error = false;
    }

    // --- command input ------------------------------------------------

    /// A word written to the GXFIFO: a packed run of up to four commands
    /// followed by their parameters.
    pub fn fifo_write(&mut self, word: u32) {
        if self.packed_pos >= 4 {
            self.packed = word.to_le_bytes();
            self.packed_pos = 0;
            self.params.clear();
        } else {
            self.params.push(word);
        }
        self.run_packed();
    }

    fn run_packed(&mut self) {
        while self.packed_pos < 4 {
            let cmd = self.packed[self.packed_pos];
            if self.params.len() < param_count(cmd) {
                return;
            }
            let params = std::mem::take(&mut self.params);
            self.submit(cmd, params);
            self.packed_pos += 1;
        }
    }

    /// A word written straight to a command port (0x04000440 + 4 * command).
    pub fn port_write(&mut self, cmd: u8, word: u32) {
        let slot = match self.port_params.iter().position(|(c, _)| *c == cmd) {
            Some(i) => i,
            None => {
                self.port_params.push((cmd, Vec::new()));
                self.port_params.len() - 1
            }
        };
        self.port_params[slot].1.push(word);
        if self.port_params[slot].1.len() >= param_count(cmd).max(1) {
            let (_, params) = self.port_params.remove(slot);
            self.submit(cmd, params);
        }
    }

    fn submit(&mut self, cmd: u8, params: Vec<u32>) {
        if self.swap_pending {
            self.deferred.push_back((cmd, params));
        } else {
            self.execute(cmd, &params);
        }
    }

    // --- matrices -----------------------------------------------------

    fn clip_matrix(&mut self) -> Mat {
        if self.clip_dirty {
            self.clip = mul(&self.proj, &self.pos);
            self.clip_dirty = false;
        }
        self.clip
    }

    /// CLIPMTX_RESULT: one element of position x projection, in 20.12 fixed point.
    /// The game reads this back to build billboards, so characters depend on it.
    pub fn clip_matrix_result(&mut self, index: usize) -> u32 {
        (self.clip_matrix()[index] * 4096.0).round() as i32 as u32
    }

    /// VECMTX_RESULT: one element of the upper-left 3x3 of the vector matrix.
    pub fn vector_matrix_result(&self, index: usize) -> u32 {
        (self.vec[index / 3 * 4 + index % 3] * 4096.0).round() as i32 as u32
    }

    /// Replace the current matrix (both position and vector in mode 2).
    fn set_current(&mut self, m: Mat) {
        match self.mode {
            0 => self.proj = m,
            1 => self.pos = m,
            2 => {
                self.pos = m;
                self.vec = m;
            }
            _ => self.tex = m,
        }
        self.clip_dirty = true;
    }

    /// Post-multiply the current matrix; `vector_too` applies it to the vector
    /// matrix as well in mode 2.
    fn multiply_current(&mut self, m: &Mat, vector_too: bool) {
        match self.mode {
            0 => self.proj = mul(&self.proj, m),
            1 => self.pos = mul(&self.pos, m),
            2 => {
                self.pos = mul(&self.pos, m);
                if vector_too {
                    self.vec = mul(&self.vec, m);
                }
            }
            _ => self.tex = mul(&self.tex, m),
        }
        self.clip_dirty = true;
    }

    fn read_matrix(p: &[u32], n: usize, rows: usize) -> Mat {
        // `rows` of the input are 3 (4x3 / 3x3) or 4 (4x4) values per column.
        let mut m = IDENTITY;
        for (i, &v) in p.iter().take(n).enumerate() {
            let (col, row) = (i / rows, i % rows);
            m[col * 4 + row] = fx12(v);
        }
        m
    }

    fn matrix_stack_op(&mut self, cmd: u8, param: u32) {
        match (cmd, self.mode) {
            (0x11, 0) => self.proj_stack = self.proj,
            (0x11, 3) => self.tex_stack = self.tex,
            (0x11, _) => {
                if self.stack_ptr > 30 {
                    self.stack_error = true;
                } else {
                    self.pos_stack[self.stack_ptr as usize] = self.pos;
                    self.vec_stack[self.stack_ptr as usize] = self.vec;
                    self.stack_ptr += 1;
                }
            }
            (0x12, 0) => self.proj = self.proj_stack,
            (0x12, 3) => self.tex = self.tex_stack,
            (0x12, _) => {
                let offset = ((param & 0x3F) << 26) as i32 >> 26;
                let ptr = self.stack_ptr - offset;
                if !(0..=30).contains(&ptr) {
                    self.stack_error = true;
                } else {
                    self.stack_ptr = ptr;
                    self.pos = self.pos_stack[ptr as usize];
                    self.vec = self.vec_stack[ptr as usize];
                }
            }
            (0x13, _) => {
                let idx = (param & 0x1F) as usize;
                if idx > 30 {
                    self.stack_error = true;
                } else {
                    self.pos_stack[idx] = self.pos;
                    self.vec_stack[idx] = self.vec;
                }
            }
            (0x14, _) => {
                let idx = (param & 0x1F) as usize;
                if idx > 30 {
                    self.stack_error = true;
                } else {
                    self.pos = self.pos_stack[idx];
                    self.vec = self.vec_stack[idx];
                }
            }
            _ => {}
        }
        self.clip_dirty = true;
    }

    // --- command execution --------------------------------------------

    fn execute(&mut self, cmd: u8, p: &[u32]) {
        let arg = p.first().copied().unwrap_or(0);
        match cmd {
            0x10 => self.mode = (arg & 3) as u8,
            0x11..=0x14 => self.matrix_stack_op(cmd, arg),
            0x15 => self.set_current(IDENTITY),
            0x16 => self.set_current(Self::read_matrix(p, 16, 4)),
            0x17 => self.set_current(Self::read_matrix(p, 12, 3)),
            0x18 => self.multiply_current(&Self::read_matrix(p, 16, 4), true),
            0x19 => self.multiply_current(&Self::read_matrix(p, 12, 3), true),
            0x1A => self.multiply_current(&Self::read_matrix(p, 9, 3), true),
            0x1B => self.multiply_current(&scaling(fx12(p[0]), fx12(p[1]), fx12(p[2])), false),
            0x1C => self.multiply_current(&translation(fx12(p[0]), fx12(p[1]), fx12(p[2])), true),
            0x20 => self.color = rgb555(arg),
            0x21 => self.normal(arg),
            0x22 => self.texcoord = [sext16(arg) as f32 / 16.0, sext16(arg >> 16) as f32 / 16.0],
            0x23 => {
                let v = [sext16(p[0]) as f32, sext16(p[0] >> 16) as f32, sext16(p[1]) as f32];
                self.vertex([v[0] / 4096.0, v[1] / 4096.0, v[2] / 4096.0]);
            }
            0x24 => {
                let v = [sext10(arg), sext10(arg >> 10), sext10(arg >> 20)];
                self.vertex([v[0] as f32 / 64.0, v[1] as f32 / 64.0, v[2] as f32 / 64.0]);
            }
            0x25 => {
                let z = self.last_vertex[2];
                self.vertex([sext16(arg) as f32 / 4096.0, sext16(arg >> 16) as f32 / 4096.0, z]);
            }
            0x26 => {
                let y = self.last_vertex[1];
                self.vertex([sext16(arg) as f32 / 4096.0, y, sext16(arg >> 16) as f32 / 4096.0]);
            }
            0x27 => {
                let x = self.last_vertex[0];
                self.vertex([x, sext16(arg) as f32 / 4096.0, sext16(arg >> 16) as f32 / 4096.0]);
            }
            0x28 => {
                let d = [sext10(arg), sext10(arg >> 10), sext10(arg >> 20)];
                let l = self.last_vertex;
                self.vertex([l[0] + d[0] as f32 / 4096.0, l[1] + d[1] as f32 / 4096.0, l[2] + d[2] as f32 / 4096.0]);
            }
            0x29 => self.attr_pending = arg,
            0x2A => self.tex_param = arg,
            0x2B => self.pltt_base = arg & 0x1FFF,
            0x30 => {
                self.diffuse = rgb555(arg);
                self.ambient = rgb555(arg >> 16);
                if arg & 0x8000 != 0 {
                    self.color = self.diffuse;
                }
            }
            0x31 => self.emission = rgb555(arg >> 16),
            0x32 => {
                let n = [sext10(arg) as f32 / 512.0, sext10(arg >> 10) as f32 / 512.0, sext10(arg >> 20) as f32 / 512.0];
                let v = transform(&self.vec, [n[0], n[1], n[2], 0.0]);
                self.lights[(arg >> 30) as usize].vector = [v[0], v[1], v[2]];
            }
            0x33 => self.lights[(arg >> 30) as usize].color = rgb555(arg),
            0x40 => {
                self.prim = Some((arg & 3) as u8);
                self.attr = self.attr_pending;
                self.verts.clear();
                self.strip_polys = 0;
            }
            0x41 => self.prim = None,
            0x50 => {
                self.swap_pending = true;
                self.swap_w_buffer = arg & 2 != 0;
            }
            0x60 => self.viewport = arg.to_le_bytes(),
            0x70 => self.box_test(p),
            0x71 => {
                let v = [sext16(p[0]) as f32 / 4096.0, sext16(p[0] >> 16) as f32 / 4096.0, sext16(p[1]) as f32 / 4096.0];
                let clip = self.clip_matrix();
                let c = transform(&clip, [v[0], v[1], v[2], 1.0]);
                self.test_result = c.map(|x| (x * 4096.0) as i32);
            }
            0x72 => {
                let n = [sext10(arg) as f32 / 512.0, sext10(arg >> 10) as f32 / 512.0, sext10(arg >> 20) as f32 / 512.0];
                let v = transform(&self.vec, [n[0], n[1], n[2], 0.0]);
                self.test_result = [(v[0] * 4096.0) as i32, (v[1] * 4096.0) as i32, (v[2] * 4096.0) as i32, 0];
            }
            _ => {}
        }
    }

    /// BOX_TEST: is any part of this box possibly on screen? Games use the answer to
    /// skip drawing whole objects. Corners are checked against each frustum plane.
    fn box_test(&mut self, p: &[u32]) {
        let f = |v: u32| sext16(v) as f32 / 4096.0;
        let (x, y, z) = (f(p[0]), f(p[0] >> 16), f(p[1]));
        let (w, h, d) = (f(p[1] >> 16), f(p[2]), f(p[2] >> 16));
        let clip = self.clip_matrix();
        let corners: Vec<[f32; 4]> = (0..8)
            .map(|i| {
                let c = |bit: u32, base: f32, size: f32| if i >> bit & 1 != 0 { base + size } else { base };
                transform(&clip, [c(0, x, w), c(1, y, h), c(2, z, d), 1.0])
            })
            .collect();
        self.box_result = !outside_frustum(corners.iter());
    }

    fn normal(&mut self, arg: u32) {
        if self.attr_pending & 0xF == 0 {
            return;
        }
        let n = [sext10(arg) as f32 / 512.0, sext10(arg >> 10) as f32 / 512.0, sext10(arg >> 20) as f32 / 512.0];
        let n = transform(&self.vec, [n[0], n[1], n[2], 0.0]);
        let mut out = self.emission;
        for (i, light) in self.lights.iter().enumerate() {
            if self.attr_pending >> i & 1 == 0 {
                continue;
            }
            let facing = (-(n[0] * light.vector[0] + n[1] * light.vector[1] + n[2] * light.vector[2])).max(0.0);
            for c in 0..3 {
                out[c] += self.diffuse[c] * light.color[c] / 31.0 * facing + self.ambient[c] * light.color[c] / 31.0;
            }
        }
        self.color = out.map(|c| c.clamp(0.0, 31.0));
    }

    fn vertex(&mut self, v: [f32; 3]) {
        self.last_vertex = v;
        let clip = self.clip_matrix();
        let mut tex = self.texcoord;
        if (self.tex_param >> 30) == 1 {
            let (s, t) = (tex[0] * 16.0, tex[1] * 16.0);
            let m = &self.tex;
            tex = [
                (s * m[0] + t * m[4] + 16.0 * m[8] + 16.0 * m[12]) / 16.0,
                (s * m[1] + t * m[5] + 16.0 * m[9] + 16.0 * m[13]) / 16.0,
            ];
        }
        let vertex = Vertex { clip: transform(&clip, [v[0], v[1], v[2], 1.0]), color: self.color, tex };
        let Some(prim) = self.prim else { return };
        self.verts.push(vertex);
        let n = self.verts.len();
        match prim {
            0 if n == 3 => {
                let poly = self.verts.clone();
                self.emit(poly);
                self.verts.clear();
            }
            1 if n == 4 => {
                let poly = self.verts.clone();
                self.emit(poly);
                self.verts.clear();
            }
            2 if n >= 3 => {
                let (a, b, c) = (self.verts[n - 3], self.verts[n - 2], self.verts[n - 1]);
                // Every other triangle is wound the other way; swap to keep facing consistent.
                let poly = if self.strip_polys % 2 == 0 { vec![a, b, c] } else { vec![b, a, c] };
                self.strip_polys += 1;
                self.emit(poly);
                self.verts.drain(..n - 2);
            }
            3 if n >= 4 && n % 2 == 0 => {
                let poly = vec![self.verts[n - 4], self.verts[n - 3], self.verts[n - 1], self.verts[n - 2]];
                self.emit(poly);
                self.verts.drain(..n - 2);
            }
            _ => {}
        }
    }

    /// Store a finished polygon. As on hardware, polygons that are off-screen or
    /// facing away never reach polygon memory, so they don't use up the 2048 slots.
    fn emit(&mut self, verts: Vec<Vertex>) {
        if self.polygons.len() < 2048 && !is_culled(&verts, self.attr) {
            self.polygons.push(Polygon {
                verts,
                attr: self.attr,
                tex_param: self.tex_param,
                pltt_base: self.pltt_base,
                viewport: self.viewport,
            });
        }
    }

    // --- frame swap and rendering --------------------------------------

    /// At VBlank: draw the frame that was queued by SWAP_BUFFERS, then let
    /// the commands that were held behind it run.
    pub fn vblank(&mut self, regs: &[u8], mem: &TextureMem) {
        if !self.swap_pending {
            return;
        }
        self.swap_pending = false;
        self.render_polys = std::mem::take(&mut self.polygons);
        self.w_buffer = self.swap_w_buffer;
        if !self.skip_draw {
            self.render(regs, mem);
        }
        self.verts.clear();
        self.prim = None;
        while let Some((cmd, params)) = self.deferred.pop_front() {
            self.execute(cmd, &params);
            if self.swap_pending {
                break;
            }
        }
    }

    fn render(&mut self, regs: &[u8], mem: &TextureMem) {
        let word = |off: usize| u32::from_le_bytes(regs[off..off + 4].try_into().unwrap());
        let disp3dcnt = word(0x60);
        let clear = word(0x350);
        let clear_alpha = ((clear >> 16) & 31) as u8;
        let clear_depth = (word(0x354) & 0x7FFF) as f32 / 32767.0;
        self.color_buf.fill((clear & 0x7FFF) as u16);
        self.alpha_buf.fill(clear_alpha);
        self.depth.fill(clear_depth);

        let settings = RenderSettings {
            textures: disp3dcnt & 1 != 0,
            highlight: disp3dcnt & 2 != 0,
            alpha_test: disp3dcnt & 4 != 0,
            alpha_blend: disp3dcnt & 8 != 0,
            alpha_ref: (word(0x340) & 31) as u8,
            w_buffer: self.w_buffer,
            toon: std::array::from_fn(|i| u16::from_le_bytes([regs[0x380 + i * 2], regs[0x381 + i * 2]])),
        };

        let polys = std::mem::take(&mut self.render_polys);
        // Opaque polygons first, then translucent ones in submission order.
        for pass_translucent in [false, true] {
            for poly in &polys {
                if is_translucent(poly) == pass_translucent {
                    self.draw_polygon(poly, &settings, mem);
                }
            }
        }
        self.render_polys = polys;
    }

    fn draw_polygon(&mut self, poly: &Polygon, s: &RenderSettings, mem: &TextureMem) {
        let clipped = clip_polygon(&poly.verts);
        if clipped.len() < 3 {
            return;
        }
        let [x1, y1, x2, y2] = poly.viewport.map(|v| v as f32);
        let (vw, vh) = (x2 - x1 + 1.0, y2 - y1 + 1.0);
        let top = 191.0 - y2;
        let screen: Vec<ScreenVertex> = clipped
            .iter()
            .map(|v| {
                let inv_w = 1.0 / v.clip[3];
                ScreenVertex {
                    x: x1 + (v.clip[0] * inv_w + 1.0) * 0.5 * vw,
                    y: top + (1.0 - v.clip[1] * inv_w) * 0.5 * vh,
                    z: v.clip[2] * inv_w,
                    inv_w,
                    color: v.color,
                    tex: v.tex,
                }
            })
            .collect();

        // Facing from the screen-space winding (y points down).
        let area: f32 = (0..screen.len())
            .map(|i| {
                let (a, b) = (&screen[i], &screen[(i + 1) % screen.len()]);
                a.x * b.y - b.x * a.y
            })
            .sum();
        if area == 0.0 {
            return;
        }
        let front = area < 0.0;
        if !(poly.attr >> if front { 7 } else { 6 } & 1 != 0) {
            return;
        }

        for i in 1..screen.len() - 1 {
            self.raster_triangle(&screen[0], &screen[i], &screen[i + 1], poly, s, mem);
        }
    }

    fn raster_triangle(
        &mut self,
        a: &ScreenVertex,
        b: &ScreenVertex,
        c: &ScreenVertex,
        poly: &Polygon,
        s: &RenderSettings,
        mem: &TextureMem,
    ) {
        let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
        if area.abs() < 1e-6 {
            return;
        }
        let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
        let max_x = a.x.max(b.x).max(c.x).ceil().min(WIDTH as f32 - 1.0).max(0.0) as usize;
        let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
        let max_y = a.y.max(b.y).max(c.y).ceil().min(HEIGHT as f32 - 1.0).max(0.0) as usize;
        let inv_area = 1.0 / area;

        let poly_alpha = ((poly.attr >> 16) & 31) as f32;
        let poly_alpha = if poly_alpha == 0.0 { 31.0 } else { poly_alpha }; // wireframe drawn as solid
        let mode = (poly.attr >> 4) & 3;
        let depth_equal = poly.attr & (1 << 14) != 0;
        let write_depth_translucent = poly.attr & (1 << 11) != 0;
        let tex_format = (poly.tex_param >> 26) & 7;
        let textured = s.textures && tex_format != 0;

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((b.x - px) * (c.y - py) - (c.x - px) * (b.y - py)) * inv_area;
                let w1 = ((c.x - px) * (a.y - py) - (a.x - px) * (c.y - py)) * inv_area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                // Perspective-correct weights.
                let (p0, p1, p2) = (w0 * a.inv_w, w1 * b.inv_w, w2 * c.inv_w);
                let sum = p0 + p1 + p2;
                if sum <= 0.0 {
                    continue;
                }
                let (q0, q1, q2) = (p0 / sum, p1 / sum, p2 / sum);

                let depth = if s.w_buffer {
                    (1.0 / sum) / 4096.0
                } else {
                    (w0 * a.z + w1 * b.z + w2 * c.z) * 0.5 + 0.5
                };
                let idx = y * WIDTH + x;
                let pass = if depth_equal { (depth - self.depth[idx]).abs() < 0.004 } else { depth < self.depth[idx] };
                if !pass {
                    continue;
                }

                let mut rgb = [0.0f32; 3];
                for ch in 0..3 {
                    rgb[ch] = q0 * a.color[ch] + q1 * b.color[ch] + q2 * c.color[ch];
                }
                let mut alpha = poly_alpha;
                if textured {
                    let t = [
                        q0 * a.tex[0] + q1 * b.tex[0] + q2 * c.tex[0],
                        q0 * a.tex[1] + q1 * b.tex[1] + q2 * c.tex[1],
                    ];
                    let Some(texel) = sample_texture(mem, poly.tex_param, poly.pltt_base, t[0], t[1]) else { continue };
                    let ta = texel[3];
                    match mode {
                        1 => {
                            // Decal: texture where opaque, vertex colour where not.
                            let f = ta / 31.0;
                            for ch in 0..3 {
                                rgb[ch] = texel[ch] * f + rgb[ch] * (1.0 - f);
                            }
                        }
                        2 => {
                            let toon = toon_color(s, rgb[0]);
                            for ch in 0..3 {
                                rgb[ch] = if s.highlight {
                                    (texel[ch] * rgb[ch] / 31.0 + toon[ch]).min(31.0)
                                } else {
                                    texel[ch] * toon[ch] / 31.0
                                };
                            }
                            alpha = poly_alpha * ta / 31.0;
                        }
                        _ => {
                            for ch in 0..3 {
                                rgb[ch] = texel[ch] * rgb[ch] / 31.0;
                            }
                            alpha = poly_alpha * ta / 31.0;
                        }
                    }
                } else if mode == 2 {
                    let toon = toon_color(s, rgb[0]);
                    rgb = if s.highlight { [(rgb[0] + toon[0]).min(31.0), (rgb[1] + toon[1]).min(31.0), (rgb[2] + toon[2]).min(31.0)] } else { toon };
                }
                let alpha_i = alpha.round() as u8;
                if alpha_i == 0 || (s.alpha_test && alpha_i <= s.alpha_ref) {
                    continue;
                }

                let src = [rgb[0].round() as u32, rgb[1].round() as u32, rgb[2].round() as u32];
                let dst = self.color_buf[idx];
                let dst_alpha = self.alpha_buf[idx];
                if alpha_i >= 31 || !s.alpha_blend {
                    self.color_buf[idx] = (src[0].min(31) | src[1].min(31) << 5 | src[2].min(31) << 10) as u16;
                    self.alpha_buf[idx] = 31;
                    self.depth[idx] = depth;
                } else {
                    let a = alpha_i as u32;
                    let blended = if dst_alpha == 0 {
                        src
                    } else {
                        let d = [dst as u32 & 31, dst as u32 >> 5 & 31, dst as u32 >> 10 & 31];
                        [0, 1, 2].map(|i| (src[i] * (a + 1) + d[i] * (31 - a)) / 32)
                    };
                    self.color_buf[idx] = (blended[0].min(31) | blended[1].min(31) << 5 | blended[2].min(31) << 10) as u16;
                    self.alpha_buf[idx] = dst_alpha.max(alpha_i);
                    if write_depth_translucent {
                        self.depth[idx] = depth;
                    }
                }
            }
        }
    }
}

impl Default for Gx {
    fn default() -> Self {
        Self::new()
    }
}

struct RenderSettings {
    textures: bool,
    highlight: bool,
    alpha_test: bool,
    alpha_blend: bool,
    alpha_ref: u8,
    w_buffer: bool,
    toon: [u16; 32],
}

struct ScreenVertex {
    x: f32,
    y: f32,
    z: f32,
    inv_w: f32,
    color: [f32; 3],
    tex: [f32; 2],
}

fn toon_color(s: &RenderSettings, red: f32) -> [f32; 3] {
    rgb555(s.toon[(red.round() as usize).min(31)] as u32)
}

fn is_translucent(p: &Polygon) -> bool {
    let alpha = (p.attr >> 16) & 31;
    let format = (p.tex_param >> 26) & 7;
    (alpha != 0 && alpha < 31) || matches!(format, 1 | 6)
}

/// True if every point lies beyond the same frustum plane.
fn outside_frustum<'a>(points: impl Iterator<Item = &'a [f32; 4]> + Clone) -> bool {
    (0..3).any(|axis| {
        points.clone().all(|c| c[axis] > c[3]) || points.clone().all(|c| c[axis] < -c[3])
    })
}

/// True if a polygon can't appear: entirely off-screen, or showing a face its
/// attributes hide (bit 6 = back faces visible, bit 7 = front faces visible).
fn is_culled(verts: &[Vertex], attr: u32) -> bool {
    if outside_frustum(verts.iter().map(|v| &v.clip)) {
        return true;
    }
    if verts.iter().any(|v| v.clip[3] <= 0.0) {
        return false; // crosses the camera plane: facing is decided after clipping
    }
    let ndc = |v: &Vertex| (v.clip[0] / v.clip[3], v.clip[1] / v.clip[3]);
    let area: f32 = (0..verts.len())
        .map(|i| {
            let (a, b) = (ndc(&verts[i]), ndc(&verts[(i + 1) % verts.len()]));
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    let front = area > 0.0;
    area == 0.0 || attr >> if front { 7 } else { 6 } & 1 == 0
}

/// Clip against the near and far planes (-w <= z <= w) in homogeneous space.
fn clip_polygon(input: &[Vertex]) -> Vec<Vertex> {
    let lerp = |a: &Vertex, b: &Vertex, t: f32| {
        let mix = |x: f32, y: f32| x + (y - x) * t;
        Vertex {
            clip: [0, 1, 2, 3].map(|i| mix(a.clip[i], b.clip[i])),
            color: [0, 1, 2].map(|i| mix(a.color[i], b.color[i])),
            tex: [0, 1].map(|i| mix(a.tex[i], b.tex[i])),
        }
    };
    let mut poly = input.to_vec();
    for plane in 0..2 {
        // Signed distance: positive inside.
        let dist = |v: &Vertex| if plane == 0 { v.clip[2] + v.clip[3] } else { v.clip[3] - v.clip[2] };
        let mut out = Vec::with_capacity(poly.len() + 2);
        for i in 0..poly.len() {
            let (a, b) = (&poly[i], &poly[(i + 1) % poly.len()]);
            let (da, db) = (dist(a), dist(b));
            if da >= 0.0 {
                out.push(*a);
            }
            if (da >= 0.0) != (db >= 0.0) {
                out.push(lerp(a, b, da / (da - db)));
            }
        }
        poly = out;
        if poly.len() < 3 {
            return Vec::new();
        }
    }
    poly.retain(|v| v.clip[3] > 1e-6);
    poly
}

/// Fetch one texel as (r, g, b, a), each 0..31, or None if it should be skipped.
fn sample_texture(mem: &TextureMem, tex_param: u32, pltt_base: u32, s: f32, t: f32) -> Option<[f32; 4]> {
    let w = 8usize << ((tex_param >> 20) & 7);
    let h = 8usize << ((tex_param >> 23) & 7);
    let wrap = |i: i32, size: usize, repeat: bool, flip: bool| -> usize {
        let size = size as i32;
        let v = if repeat {
            if flip {
                let m = i.rem_euclid(2 * size);
                if m >= size { 2 * size - 1 - m } else { m }
            } else {
                i.rem_euclid(size)
            }
        } else {
            i.clamp(0, size - 1)
        };
        v as usize
    };
    let x = wrap(s.floor() as i32, w, tex_param & (1 << 16) != 0, tex_param & (1 << 18) != 0);
    let y = wrap(t.floor() as i32, h, tex_param & (1 << 17) != 0, tex_param & (1 << 19) != 0);
    let base = ((tex_param & 0xFFFF) as usize) * 8;
    let idx = y * w + x;
    let color0_clear = tex_param & (1 << 29) != 0;
    let pal = |addr: usize| rgb555(mem.palette(addr) as u32);
    let opaque = |c: [f32; 3]| Some([c[0], c[1], c[2], 31.0]);

    match (tex_param >> 26) & 7 {
        1 => {
            let b = mem.image8(base + idx);
            let a = (b >> 5) as f32;
            let c = pal(pltt_base as usize * 16 + (b & 31) as usize * 2);
            Some([c[0], c[1], c[2], (a * 4.0 + (a / 2.0).floor()).min(31.0)])
        }
        2 => {
            let v = (mem.image8(base + idx / 4) >> ((idx & 3) * 2)) & 3;
            if v == 0 && color0_clear {
                return None;
            }
            opaque(pal(pltt_base as usize * 8 + v as usize * 2))
        }
        3 => {
            let b = mem.image8(base + idx / 2);
            let v = if idx & 1 != 0 { b >> 4 } else { b & 15 };
            if v == 0 && color0_clear {
                return None;
            }
            opaque(pal(pltt_base as usize * 16 + v as usize * 2))
        }
        4 => {
            let v = mem.image8(base + idx);
            if v == 0 && color0_clear {
                return None;
            }
            opaque(pal(pltt_base as usize * 16 + v as usize * 2))
        }
        5 => {
            let block = base + ((y / 4) * (w / 4) + x / 4) * 4;
            let v = (mem.image8(block + (y & 3)) >> ((x & 3) * 2)) & 3;
            let entry_addr = 0x2_0000 + ((block & 0x1_FFFF) >> 1) + if block >= 0x4_0000 { 0x1_0000 } else { 0 };
            let entry = mem.image16(entry_addr) as usize;
            let colors = pltt_base as usize * 16 + (entry & 0x3FFF) * 4;
            let c = |i: usize| pal(colors + i * 2);
            let mix = |a: [f32; 3], b: [f32; 3], wa: f32, wb: f32| [0, 1, 2].map(|i| ((a[i] * wa + b[i] * wb) / 8.0).floor());
            match (entry >> 14, v) {
                (0, 3) | (1, 3) => None,
                (0, v) => opaque(c(v as usize)),
                (1, 2) => opaque([0, 1, 2].map(|i| ((c(0)[i] + c(1)[i]) / 2.0).floor())),
                (1, v) => opaque(c(v as usize)),
                (2, v) => opaque(c(v as usize)),
                (_, 2) => opaque(mix(c(0), c(1), 5.0, 3.0)),
                (_, 3) => opaque(mix(c(0), c(1), 3.0, 5.0)),
                (_, v) => opaque(c(v as usize)),
            }
        }
        6 => {
            let b = mem.image8(base + idx);
            let c = pal(pltt_base as usize * 16 + (b & 7) as usize * 2);
            Some([c[0], c[1], c[2], (b >> 3) as f32])
        }
        7 => {
            let v = mem.image16(base + idx * 2);
            let c = rgb555(v as u32);
            Some([c[0], c[1], c[2], if v & 0x8000 != 0 { 31.0 } else { 0.0 }])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(regs: &mut [u8], off: usize, v: u32) {
        regs[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    struct Rig {
        gx: Gx,
        regs: Vec<u8>,
        vram: Vec<u8>,
        map: VramMap,
    }

    impl Rig {
        fn new() -> Rig {
            let mut regs = vec![0u8; 0x1100];
            word(&mut regs, 0x60, 0x0001 | 0x0008); // textures + alpha blending
            word(&mut regs, 0x354, 0x7FFF); // clear depth: farthest
            Rig { gx: Gx::new(), regs, vram: vec![0; 0xA4000], map: VramMap::new() }
        }

        fn draw(&mut self) {
            self.gx.port_write(0x50, 0);
            let mem = TextureMem { vram: &self.vram, map: &self.map };
            self.gx.vblank(&self.regs, &mem);
        }

        fn pixel(&self, x: usize, y: usize) -> (u16, u8) {
            (self.gx.color_buf[y * WIDTH + x], self.gx.alpha_buf[y * WIDTH + x])
        }

        /// A triangle covering the left-top region, both faces visible.
        fn triangle(&mut self, color: u32) {
            self.gx.port_write(0x29, 0xC0 | (31 << 16)); // front+back, opaque
            self.gx.port_write(0x40, 0);
            self.gx.port_write(0x20, color);
            for (x, y) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0)] {
                let p = |v: f32| ((v * 4096.0) as i32 as u32) & 0xFFFF;
                self.gx.port_write(0x23, p(x) | p(y) << 16);
                self.gx.port_write(0x23, 0);
            }
            self.gx.port_write(0x41, 0);
        }
    }

    #[test]
    fn matrix_multiplication_applies_translation() {
        let m = mul(&translation(1.0, 2.0, 3.0), &scaling(2.0, 2.0, 2.0));
        let v = transform(&m, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(&v[..3], &[3.0, 4.0, 5.0]);
    }

    #[test]
    fn packed_fifo_runs_commands_with_their_parameters() {
        let mut gx = Gx::new();
        // MTX_MODE (1 param) and MTX_IDENTITY (0 params) packed together, then NOP padding.
        gx.fifo_write(0x0000_1510);
        gx.fifo_write(2); // mode 2
        assert_eq!(gx.mode, 2);
        // A word with only zero-parameter commands needs no parameter words.
        gx.fifo_write(0x0000_0015);
        gx.fifo_write(0x0000_0010);
        gx.fifo_write(1);
        assert_eq!(gx.mode, 1);
    }

    #[test]
    fn triangle_fills_pixels_with_vertex_colour() {
        let mut rig = Rig::new();
        rig.triangle(0x001F); // red
        rig.draw();
        assert_eq!(rig.pixel(10, 10), (0x001F, 31));
        // The far corner of the screen is outside the triangle.
        assert_eq!(rig.pixel(250, 180).1, 0);
    }

    #[test]
    fn commands_after_swap_wait_for_vblank() {
        let mut rig = Rig::new();
        rig.triangle(0x001F);
        rig.gx.port_write(0x50, 0);
        rig.gx.port_write(0x10, 2);
        assert_ne!(rig.gx.mode, 2, "mode change must be held until the swap happens");
        let mem = TextureMem { vram: &rig.vram, map: &rig.map };
        rig.gx.vblank(&rig.regs, &mem);
        assert_eq!(rig.gx.mode, 2);
    }

    #[test]
    fn nearer_polygon_wins_depth_test() {
        let mut rig = Rig::new();
        rig.gx.port_write(0x29, 0xC0 | (31 << 16));
        for (color, z) in [(0x001Fu32, 0.5f32), (0x03E0, -0.5)] {
            rig.gx.port_write(0x40, 0);
            rig.gx.port_write(0x20, color);
            for (x, y) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0)] {
                let p = |v: f32| ((v * 4096.0) as i32 as u32) & 0xFFFF;
                rig.gx.port_write(0x23, p(x) | p(y) << 16);
                rig.gx.port_write(0x23, p(z));
            }
        }
        rig.draw();
        assert_eq!(rig.pixel(10, 10).0, 0x03E0); // green is nearer
    }

    #[test]
    fn translucent_polygon_blends_with_background() {
        let mut rig = Rig::new();
        rig.gx.port_write(0x29, 0xC0 | (16 << 16));
        rig.gx.port_write(0x40, 0);
        rig.gx.port_write(0x20, 0x001F);
        for (x, y) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0)] {
            let p = |v: f32| ((v * 4096.0) as i32 as u32) & 0xFFFF;
            rig.gx.port_write(0x23, p(x) | p(y) << 16);
            rig.gx.port_write(0x23, 0);
        }
        word(&mut rig.regs, 0x350, 0x7C00 | (31 << 16)); // opaque blue clear colour
        rig.draw();
        let (c, a) = rig.pixel(10, 10);
        assert_eq!(a, 31);
        assert!(c & 31 > 8 && c >> 10 > 8, "expected a red/blue mix, got {c:#06x}");
    }

    #[test]
    fn near_plane_clipping_keeps_polygon_in_front_of_camera() {
        let v = |z: f32, w: f32| Vertex { clip: [0.0, 0.0, z, w], ..Vertex::default() };
        let out = clip_polygon(&[v(0.0, 1.0), v(0.0, 1.0), v(-3.0, 1.0)]);
        assert!(out.iter().all(|v| v.clip[2] + v.clip[3] >= -1e-4));
        assert!(clip_polygon(&[v(-3.0, 1.0), v(-3.0, 1.0), v(-3.0, 1.0)]).is_empty());
    }

    #[test]
    fn sixteen_colour_texture_is_sampled() {
        let mut rig = Rig::new();
        // Bank A as texture slot 0, bank E as texture palettes.
        let mut cnt = [0u8; 9];
        cnt[0] = 0x83;
        cnt[4] = 0x83;
        rig.map = VramMap::rebuild(&cnt);
        // 8x8 texture, all texels palette index 3 (two per byte).
        for i in 0..32 {
            rig.vram[i] = 0x33;
        }
        let pal_base = 0x80000; // bank E
        rig.vram[pal_base + 6..pal_base + 8].copy_from_slice(&0x7C00u16.to_le_bytes()); // index 3 = blue
        let tex_param = 3 << 26; // 16 colours, 8x8, offset 0
        rig.gx.port_write(0x2A, tex_param);
        rig.gx.port_write(0x29, 0xC0 | (31 << 16));
        rig.gx.port_write(0x40, 0);
        rig.gx.port_write(0x20, 0x7FFF);
        for (x, y) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0)] {
            let p = |v: f32| ((v * 4096.0) as i32 as u32) & 0xFFFF;
            rig.gx.port_write(0x22, 0);
            rig.gx.port_write(0x23, p(x) | p(y) << 16);
            rig.gx.port_write(0x23, 0);
        }
        rig.draw();
        assert_eq!(rig.pixel(10, 10).0, 0x7C00);
    }

    #[test]
    fn clip_matrix_reads_back_position_times_projection() {
        let mut gx = Gx::new();
        gx.port_write(0x10, 0); // projection
        gx.port_write(0x1B, 2 << 12);
        gx.port_write(0x1B, 1 << 12);
        gx.port_write(0x1B, 1 << 12);
        gx.port_write(0x10, 1); // position
        gx.port_write(0x1C, 3 << 12);
        gx.port_write(0x1C, 0);
        gx.port_write(0x1C, 0);
        assert_eq!(gx.clip_matrix_result(0), 2 << 12);
        // Translation goes through the projection's scale.
        assert_eq!(gx.clip_matrix_result(12), 6 << 12);
    }

    #[test]
    fn vector_matrix_reads_back_as_three_by_three() {
        let mut gx = Gx::new();
        gx.port_write(0x10, 2);
        for v in 1..=9u32 {
            gx.port_write(0x1A, v << 12);
        }
        assert_eq!(gx.vector_matrix_result(0), 1 << 12);
        assert_eq!(gx.vector_matrix_result(3), 4 << 12);
        assert_eq!(gx.vector_matrix_result(8), 9 << 12);
    }

    fn clip_vertex(x: f32, y: f32) -> Vertex {
        Vertex { clip: [x, y, 0.0, 1.0], ..Vertex::default() }
    }

    #[test]
    fn offscreen_polygons_do_not_use_polygon_memory() {
        let off = [clip_vertex(2.0, 0.0), clip_vertex(3.0, 0.0), clip_vertex(2.0, 1.0)];
        assert!(is_culled(&off, 0xC0));
        let on = [clip_vertex(0.0, 0.0), clip_vertex(0.5, 0.0), clip_vertex(0.0, 0.5)];
        assert!(!is_culled(&on, 0xC0));
    }

    #[test]
    fn facing_rules_match_the_rasterizer() {
        // Counter-clockwise in clip space, which the rasterizer treats as front-facing.
        let ccw = [clip_vertex(0.0, 0.0), clip_vertex(0.5, 0.0), clip_vertex(0.0, 0.5)];
        assert!(!is_culled(&ccw, 0x80), "front visible");
        assert!(is_culled(&ccw, 0x40), "only back visible");
        let cw = [ccw[0], ccw[2], ccw[1]];
        assert!(is_culled(&cw, 0x80));
        assert!(!is_culled(&cw, 0x40));
    }

    #[test]
    fn box_test_reports_visibility() {
        let mut gx = Gx::new();
        let unit = 1 << 12;
        // A unit box around the origin is on screen with identity matrices.
        gx.port_write(0x70, 0);
        gx.port_write(0x70, unit);
        gx.port_write(0x70, unit | unit << 16);
        assert!(gx.box_result);
        // The same box far to the right is not.
        gx.port_write(0x70, 8 * unit);
        gx.port_write(0x70, unit);
        gx.port_write(0x70, unit | unit << 16);
        assert!(!gx.box_result);
    }

    #[test]
    fn matrix_stack_push_pop_restores_position() {
        let mut gx = Gx::new();
        gx.port_write(0x10, 1);
        gx.port_write(0x11, 0);
        gx.port_write(0x1C, ((2.0f32 * 4096.0) as u32) & 0xFFFF_FFFF);
        gx.port_write(0x1C, 0);
        gx.port_write(0x1C, 0);
        assert_eq!(gx.pos[12], 2.0);
        gx.port_write(0x12, 1);
        assert_eq!(gx.pos[12], 0.0);
    }
}
