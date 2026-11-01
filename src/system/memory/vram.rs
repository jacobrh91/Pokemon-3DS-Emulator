//! VRAM bank mapping. Nine banks (A-I) are placed into the CPU-visible regions
//! by VRAMCNT; each region is a table of 16 KiB granules pointing into the flat
//! bank buffer.

pub const VRAM_SIZE: usize = 0xA4000;
const GRANULE: usize = 0x4000;
const UNMAPPED: usize = usize::MAX;

/// (offset into the flat buffer, size) of banks A..I.
const BANKS: [(usize, usize); 9] = [
    (0x00000, 0x20000),
    (0x20000, 0x20000),
    (0x40000, 0x20000),
    (0x60000, 0x20000),
    (0x80000, 0x10000),
    (0x90000, 0x04000),
    (0x94000, 0x04000),
    (0x98000, 0x08000),
    (0xA0000, 0x04000),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Region {
    BgA,
    ObjA,
    BgB,
    ObjB,
    Arm7,
}

#[derive(Clone)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct VramMap {
    pub bg_a: [usize; 32],
    pub obj_a: [usize; 16],
    pub bg_b: [usize; 8],
    pub obj_b: [usize; 8],
    pub arm7: [usize; 16],
    /// Extended palettes: four 8 KiB background slots per engine, one 8 KiB
    /// sprite palette per engine (offsets into the flat buffer).
    pub bg_ext_a: [usize; 4],
    pub bg_ext_b: [usize; 4],
    pub obj_ext_a: usize,
    pub obj_ext_b: usize,
    /// 3D textures: four 128 KiB image slots and six 16 KiB palette slots.
    pub tex_image: [usize; 4],
    pub tex_palette: [usize; 6],
}

impl VramMap {
    pub fn new() -> VramMap {
        VramMap {
            bg_a: [UNMAPPED; 32],
            obj_a: [UNMAPPED; 16],
            bg_b: [UNMAPPED; 8],
            obj_b: [UNMAPPED; 8],
            arm7: [UNMAPPED; 16],
            bg_ext_a: [UNMAPPED; 4],
            bg_ext_b: [UNMAPPED; 4],
            obj_ext_a: UNMAPPED,
            obj_ext_b: UNMAPPED,
            tex_image: [UNMAPPED; 4],
            tex_palette: [UNMAPPED; 6],
        }
    }

    fn table(&mut self, region: Region) -> &mut [usize] {
        match region {
            Region::BgA => &mut self.bg_a,
            Region::ObjA => &mut self.obj_a,
            Region::BgB => &mut self.bg_b,
            Region::ObjB => &mut self.obj_b,
            Region::Arm7 => &mut self.arm7,
        }
    }

    fn place(&mut self, region: Region, bank: usize, region_offset: usize) {
        let (base, size) = BANKS[bank];
        let table = self.table(region);
        let len = table.len();
        for g in 0..size / GRANULE {
            let slot = (region_offset / GRANULE + g) % len;
            if table[slot] == UNMAPPED {
                table[slot] = base + g * GRANULE;
            }
        }
    }

    /// Rebuild every table from the nine VRAMCNT bytes (A..I).
    pub fn rebuild(cnt: &[u8; 9]) -> VramMap {
        let mut map = VramMap::new();
        for (bank, &c) in cnt.iter().enumerate() {
            if c & 0x80 == 0 {
                continue;
            }
            let mst = (c & 7) as usize;
            let ofs = ((c >> 3) & 3) as usize;
            match (bank, mst) {
                (0..=3, 3) => map.tex_image[ofs] = BANKS[bank].0,
                (4, 3) => {
                    for slot in 0..4 {
                        map.tex_palette[slot] = BANKS[bank].0 + slot * 0x4000;
                    }
                }
                (5 | 6, 3) => map.tex_palette[[0, 1, 4, 5][ofs]] = BANKS[bank].0,
                (0 | 1, 1) => map.place(Region::BgA, bank, ofs * 0x20000),
                (0 | 1, 2) => map.place(Region::ObjA, bank, (ofs & 1) * 0x20000),
                (2, 1) | (3, 1) => map.place(Region::BgA, bank, ofs * 0x20000),
                (2, 2) | (3, 2) => map.place(Region::Arm7, bank, (ofs & 1) * 0x20000),
                (2, 4) => map.place(Region::BgB, bank, 0),
                (3, 4) => map.place(Region::ObjB, bank, 0),
                (4, 1) => map.place(Region::BgA, bank, 0),
                (4, 2) => map.place(Region::ObjA, bank, 0),
                (4, 4) => map.place_ext_bg(bank, true, 0, 4),
                (5 | 6, 4) => map.place_ext_bg(bank, true, (ofs & 1) * 2, 2),
                (5 | 6, 5) => map.obj_ext_a = BANKS[bank].0,
                (7, 2) => map.place_ext_bg(bank, false, 0, 4),
                (8, 3) => map.obj_ext_b = BANKS[bank].0,
                (5 | 6, 1) => map.place(Region::BgA, bank, (ofs & 1) * 0x4000 + (ofs & 2) * 0x10000),
                (5 | 6, 2) => map.place(Region::ObjA, bank, (ofs & 1) * 0x4000 + (ofs & 2) * 0x10000),
                (7, 1) => map.place(Region::BgB, bank, 0),
                (8, 1) => map.place(Region::BgB, bank, 0x8000),
                (8, 2) => map.place(Region::ObjB, bank, 0),
                _ => {}
            }
        }
        map
    }

    fn place_ext_bg(&mut self, bank: usize, engine_a: bool, first_slot: usize, slots: usize) {
        let base = BANKS[bank].0;
        let table = if engine_a { &mut self.bg_ext_a } else { &mut self.bg_ext_b };
        for s in 0..slots {
            table[first_slot + s] = base + s * 0x2000;
        }
    }

    /// Offset into the flat buffer for `offset` within `region`, if mapped.
    pub fn lookup(&self, region: Region, offset: usize) -> Option<usize> {
        let table = match region {
            Region::BgA => &self.bg_a[..],
            Region::ObjA => &self.obj_a[..],
            Region::BgB => &self.bg_b[..],
            Region::ObjB => &self.obj_b[..],
            Region::Arm7 => &self.arm7[..],
        };
        let granule = table[(offset / GRANULE) % table.len()];
        (granule != UNMAPPED).then(|| granule + offset % GRANULE)
    }
}

impl Default for VramMap {
    fn default() -> Self {
        Self::new()
    }
}

/// LCDC view (VRAMCNT MST 0): banks sit back to back from 0x06800000.
pub fn lcdc_offset(addr: u32) -> Option<usize> {
    let off = (addr & 0x007F_FFFF) as usize;
    (off < VRAM_SIZE).then_some(off)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bank_a_maps_to_bg_a() {
        let mut cnt = [0u8; 9];
        cnt[0] = 0x81; // A enabled, MST 1 (BG-A), offset 0
        let map = VramMap::rebuild(&cnt);
        assert_eq!(map.lookup(Region::BgA, 0x1234), Some(0x1234));
        assert_eq!(map.lookup(Region::BgA, 0x20000), None);
    }

    #[test]
    fn bank_offset_shifts_region_position() {
        let mut cnt = [0u8; 9];
        cnt[1] = 0x81 | (1 << 3); // B enabled, BG-A, offset 1 -> 0x20000
        let map = VramMap::rebuild(&cnt);
        assert_eq!(map.lookup(Region::BgA, 0x20000), Some(0x20000));
        assert_eq!(map.lookup(Region::BgA, 0), None);
    }

    #[test]
    fn texture_banks_fill_image_and_palette_slots() {
        let mut cnt = [0u8; 9];
        cnt[2] = 0x83 | (1 << 3); // C -> texture slot 1
        cnt[4] = 0x83; // E -> texture palette slots 0-3
        cnt[5] = 0x83 | (3 << 3); // F -> palette slot 5
        let map = VramMap::rebuild(&cnt);
        assert_eq!(map.tex_image[1], 0x40000);
        assert_eq!(map.tex_palette[2], 0x80000 + 2 * 0x4000);
        assert_eq!(map.tex_palette[5], 0x90000);
    }

    #[test]
    fn disabled_bank_is_unmapped() {
        let map = VramMap::rebuild(&[0x01; 9]);
        assert_eq!(map.lookup(Region::BgA, 0), None);
    }
}
