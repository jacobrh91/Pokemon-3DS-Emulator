use std::fmt;
use std::path::Path;

const HEADER_SIZE: usize = 0x200;
const CRC_RANGE_END: usize = 0x15E;

/// One CPU's binary as described by the cartridge header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CodeBlock {
    pub rom_offset: u32,
    pub entry: u32,
    pub ram_address: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Header {
    pub title: String,
    pub game_code: String,
    pub maker_code: String,
    pub unit_code: u8,
    pub arm9: CodeBlock,
    pub arm7: CodeBlock,
    pub fnt_offset: u32,
    pub fat_offset: u32,
    pub used_rom_size: u32,
    pub header_crc: u16,
    pub header_crc_ok: bool,
}

#[derive(Debug)]
pub enum CartridgeError {
    Io(std::io::Error),
    TooSmall(usize),
    CodeOutOfBounds(&'static str),
}

impl fmt::Display for CartridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "could not read ROM: {e}"),
            Self::TooSmall(n) => write!(f, "ROM is {n} bytes, smaller than the {HEADER_SIZE}-byte header"),
            Self::CodeOutOfBounds(cpu) => write!(f, "{cpu} binary extends past the end of the ROM"),
        }
    }
}

impl std::error::Error for CartridgeError {}

impl From<std::io::Error> for CartridgeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Default)]
pub struct Cartridge {
    pub header: Header,
    pub rom: Vec<u8>,
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn ascii(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// CRC-16/MODBUS, which is what the DS header checksum uses.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

fn code_block(b: &[u8], off: usize) -> CodeBlock {
    CodeBlock {
        rom_offset: u32_at(b, off),
        entry: u32_at(b, off + 4),
        ram_address: u32_at(b, off + 8),
        size: u32_at(b, off + 12),
    }
}

impl Header {
    pub fn parse(rom: &[u8]) -> Result<Header, CartridgeError> {
        if rom.len() < HEADER_SIZE {
            return Err(CartridgeError::TooSmall(rom.len()));
        }
        let header_crc = u16::from_le_bytes([rom[CRC_RANGE_END], rom[CRC_RANGE_END + 1]]);
        Ok(Header {
            title: ascii(&rom[0x000..0x00C]),
            game_code: ascii(&rom[0x00C..0x010]),
            maker_code: ascii(&rom[0x010..0x012]),
            unit_code: rom[0x012],
            arm9: code_block(rom, 0x020),
            arm7: code_block(rom, 0x030),
            fnt_offset: u32_at(rom, 0x040),
            fat_offset: u32_at(rom, 0x048),
            used_rom_size: u32_at(rom, 0x080),
            header_crc,
            header_crc_ok: crc16(&rom[..CRC_RANGE_END]) == header_crc,
        })
    }
}

impl Cartridge {
    pub fn from_bytes(rom: Vec<u8>) -> Result<Cartridge, CartridgeError> {
        let header = Header::parse(&rom)?;
        for (name, block) in [("ARM9", header.arm9), ("ARM7", header.arm7)] {
            let end = block.rom_offset as u64 + block.size as u64;
            if end > rom.len() as u64 {
                return Err(CartridgeError::CodeOutOfBounds(name));
            }
        }
        Ok(Cartridge { header, rom })
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Cartridge, CartridgeError> {
        Self::from_bytes(std::fs::read(path)?)
    }

    pub fn arm9_code(&self) -> &[u8] {
        let b = self.header.arm9;
        &self.rom[b.rom_offset as usize..(b.rom_offset + b.size) as usize]
    }

    pub fn arm7_code(&self) -> &[u8] {
        let b = self.header.arm7;
        &self.rom[b.rom_offset as usize..(b.rom_offset + b.size) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x1000];
        rom[..12].copy_from_slice(b"POKEMON SS\0\0");
        rom[0x0C..0x10].copy_from_slice(b"IPGE");
        rom[0x10..0x12].copy_from_slice(b"01");
        rom[0x20..0x24].copy_from_slice(&0x200u32.to_le_bytes());
        rom[0x24..0x28].copy_from_slice(&0x0200_0800u32.to_le_bytes());
        rom[0x28..0x2C].copy_from_slice(&0x0200_0000u32.to_le_bytes());
        rom[0x2C..0x30].copy_from_slice(&0x100u32.to_le_bytes());
        rom[0x30..0x34].copy_from_slice(&0x400u32.to_le_bytes());
        rom[0x3C..0x40].copy_from_slice(&0x100u32.to_le_bytes());
        let crc = crc16(&rom[..CRC_RANGE_END]);
        rom[CRC_RANGE_END..CRC_RANGE_END + 2].copy_from_slice(&crc.to_le_bytes());
        rom
    }

    #[test]
    fn parses_header_fields() {
        let cart = Cartridge::from_bytes(synthetic_rom()).unwrap();
        assert_eq!(cart.header.title, "POKEMON SS");
        assert_eq!(cart.header.game_code, "IPGE");
        assert_eq!(cart.header.arm9.entry, 0x0200_0800);
        assert_eq!(cart.arm9_code().len(), 0x100);
        assert!(cart.header.header_crc_ok);
    }

    #[test]
    fn rejects_truncated_rom() {
        assert!(matches!(Header::parse(&[0; 16]), Err(CartridgeError::TooSmall(16))));
    }

    #[test]
    fn rejects_code_past_end() {
        let mut rom = synthetic_rom();
        rom[0x2C..0x30].copy_from_slice(&0x10_0000u32.to_le_bytes());
        assert!(matches!(Cartridge::from_bytes(rom), Err(CartridgeError::CodeOutOfBounds("ARM9"))));
    }

    #[test]
    fn crc_matches_known_vector() {
        assert_eq!(crc16(b"123456789"), 0x4B37);
    }
}
