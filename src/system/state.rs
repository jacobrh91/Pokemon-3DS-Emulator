//! Save states: a snapshot of the whole console (both CPUs, all memory and
//! hardware registers, the save chip) that can be restored later. The game
//! ROM itself is not stored, so a state only loads into the game it came from.

use super::Nds;

const MAGIC: &[u8; 8] = b"DSSTATE3";

/// Identifies the game a state belongs to.
fn game_id(nds: &Nds) -> String {
    let h = &nds.hw.cart.header;
    format!("{}:{}:{:04x}", h.game_code, h.title, h.header_crc)
}

impl Nds {
    pub fn save_state(&mut self) -> Vec<u8> {
        // Timers tick in batches; bring them up to date so the state holds their true values.
        self.hw.flush_timers(0);
        self.hw.flush_timers(1);
        let id = game_id(self);
        let body = bincode::serialize(self).expect("console state serializes");
        let mut out = Vec::with_capacity(MAGIC.len() + 4 + id.len() + body.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(id.len() as u32).to_le_bytes());
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Replace the running console with a saved one. On error nothing changes.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        let rest = bytes.strip_prefix(MAGIC.as_slice()).ok_or("not a save state file")?;
        let (len, rest) = rest.split_first_chunk::<4>().ok_or("save state is truncated")?;
        let len = u32::from_le_bytes(*len) as usize;
        if rest.len() < len {
            return Err("save state is truncated".into());
        }
        let (id, body) = rest.split_at(len);
        if id != game_id(self).as_bytes() {
            return Err(format!("save state is for a different game ({})", String::from_utf8_lossy(id)));
        }
        let mut loaded: Nds = bincode::deserialize(body).map_err(|e| format!("corrupt save state: {e}"))?;
        // Not part of a state: the ROM, and the picture buffer (redrawn every frame).
        loaded.hw.cart = std::mem::take(&mut self.hw.cart);
        loaded.hw.frame = std::mem::take(&mut self.hw.frame);
        loaded.hw.refresh_tcm();
        loaded.hw.refresh_timers_on(0);
        loaded.hw.refresh_timers_on(1);
        loaded.jit = std::mem::take(&mut self.jit);
        loaded.jit.flush(&mut loaded.hw);
        *self = loaded;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::Cartridge;

    fn console() -> Nds {
        let mut rom = vec![0u8; 0x10000];
        rom[0x0C..0x10].copy_from_slice(b"TEST");
        let crc = crate::cartridge::crc16(&rom[..0x15E]);
        rom[0x15E..0x160].copy_from_slice(&crc.to_le_bytes());
        Nds::direct_boot(Cartridge::from_bytes(rom).unwrap())
    }

    #[test]
    fn restores_memory_and_registers() {
        let mut nds = console();
        nds.hw.write(0, 0x0200_1000, 4, 0xCAFE_F00D);
        nds.arm9.regs[3] = 77;
        nds.hw.backup.data[10] = 0x42;
        nds.hw.irq[0].ie = 0x1234;
        let state = nds.save_state();

        nds.hw.write(0, 0x0200_1000, 4, 0);
        nds.arm9.regs[3] = 0;
        nds.hw.backup.data[10] = 0xFF;
        nds.hw.irq[0].ie = 0;
        nds.load_state(&state).unwrap();

        assert_eq!(nds.hw.read(0, 0x0200_1000, 4), 0xCAFE_F00D);
        assert_eq!(nds.arm9.regs[3], 77);
        assert_eq!(nds.hw.backup.data[10], 0x42);
        assert_eq!(nds.hw.irq[0].ie, 0x1234);
    }

    #[test]
    fn keeps_the_loaded_rom() {
        let mut nds = console();
        let state = nds.save_state();
        nds.load_state(&state).unwrap();
        assert_eq!(nds.hw.cart.header.game_code, "TEST");
        assert_eq!(nds.hw.cart.rom.len(), 0x10000);
    }

    #[test]
    fn rejects_garbage_and_other_games() {
        let mut nds = console();
        assert!(nds.load_state(b"hello").is_err());
        let mut state = nds.save_state();
        // Corrupt the game id.
        state[12] ^= 0xFF;
        assert!(nds.load_state(&state).unwrap_err().contains("different game"));
        let truncated = &nds.save_state()[..20];
        assert!(nds.load_state(truncated).is_err());
    }

    /// Resuming from a state must be indistinguishable from never stopping.
    #[test]
    fn resumed_run_matches_uninterrupted_run() {
        let Ok(cart) = Cartridge::load("roms/Pokemon SoulSilver.nds") else { return };
        let mut nds = Nds::direct_boot(cart);
        for _ in 0..400 {
            nds.run_frame();
        }
        let state = nds.save_state();
        for _ in 0..60 {
            nds.run_frame();
        }
        let expected = nds.framebuffer().to_vec();
        let expected_regs = (nds.arm9.regs, nds.arm7.regs);

        nds.load_state(&state).unwrap();
        for _ in 0..60 {
            nds.run_frame();
        }
        assert_eq!(nds.framebuffer(), &expected[..]);
        assert_eq!((nds.arm9.regs, nds.arm7.regs), expected_regs);
    }
}
