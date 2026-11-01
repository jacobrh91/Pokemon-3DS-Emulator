use super::asm::*;
use super::mem::CodeMemory;
use super::Jit;
use crate::cpu::{Arch, Cpu};
use crate::system::tests::test_hardware;
use crate::system::{Bus7, Bus9, Hardware, ARM7, ARM9};

#[test]
fn generated_code_runs() {
    let mut mem = CodeMemory::new(1 << 16).expect("MAP_JIT mapping");
    let mut a = Asm::new();
    a.mov_imm(1, 40);
    a.add_imm(0, 1, 2, false);
    a.ret();
    let entry = mem.install(&a.finish()).unwrap();
    let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(entry) };
    assert_eq!(f(), 42);
}

#[test]
fn writing_nzcv_ignores_the_other_bits() {
    let mut mem = CodeMemory::new(1 << 16).unwrap();
    let mut a = Asm::new();
    a.mov_imm(8, 0xFFFF_FFFF);
    a.msr_nzcv(8);
    a.mrs_nzcv(0);
    a.ret();
    let entry = mem.install(&a.finish()).unwrap();
    let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(entry) };
    assert_eq!(f(), 0xF000_0000);
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

const CODE: u32 = 0x0200_1000;
const DATA: u32 = 0x0210_0000;
const DATA_SIZE: u32 = 0x1_0000;

/// A random ARM instruction, weighted towards the kinds the translator handles.
fn random_arm(rng: &mut Rng) -> u32 {
    let cond = if rng.below(3) == 0 { rng.below(14) } else { 14 };
    let (rn, rd, rm) = (rng.below(16), rng.below(15), rng.below(16));
    let word = match rng.below(10) {
        0..=2 => {
            // data processing, rotated immediate
            let (op, s) = (rng.below(16), rng.below(2));
            0b001 << 25 | op << 21 | s << 20 | rn << 16 | rd << 12 | rng.below(16) << 8 | rng.below(256)
        }
        3..=5 => {
            // data processing, register shifted by a constant (and the odd register shift)
            let (op, s) = (rng.below(16), rng.below(2));
            let shift = rng.below(4) << 5 | rng.below(32) << 7 | if rng.below(8) == 0 { 1 << 4 } else { 0 };
            op << 21 | s << 20 | rn << 16 | rd << 12 | shift | rm
        }
        6..=8 => {
            // single load / store
            let flags = rng.below(64) << 20 & 0x3F0_0000;
            let reg_offset = rng.below(3) == 0;
            let offset = if reg_offset {
                1 << 25 | rng.below(32) << 7 | rng.below(4) << 5 | rm
            } else {
                rng.below(0x40) * 4 + rng.below(2) * 0x800
            };
            0b01 << 26 | flags | rn << 16 | rd << 12 | offset
        }
        _ => match rng.below(4) {
            0 => 0x012F_FF10 | rm,
            1 => 0x012F_FF30 | rm,
            _ => 0b101 << 25 | rng.below(2) << 24 | rng.below(64),
        },
    };
    cond << 28 | word
}

/// A random Thumb instruction, from all the formats.
fn random_thumb(rng: &mut Rng) -> u32 {
    let (lo, mid, imm8) = (rng.below(8), rng.below(8), rng.below(256));
    let small = rng.below(32);
    match rng.below(19) {
        0 => rng.below(3) << 11 | small << 6 | mid << 3 | lo,
        1 => 0b00011 << 11 | rng.below(2) << 10 | rng.below(2) << 9 | rng.below(8) << 6 | mid << 3 | lo,
        2 => 0b001 << 13 | rng.below(4) << 11 | lo << 8 | imm8,
        3 => 0b010000 << 10 | rng.below(16) << 6 | mid << 3 | lo,
        4 => 0b010001 << 10 | rng.below(4) << 8 | rng.below(2) << 7 | rng.below(2) << 6 | mid << 3 | lo,
        5 => 0b01001 << 11 | lo << 8 | imm8,
        6 => 0b0101 << 12 | rng.below(8) << 9 | rng.below(8) << 6 | mid << 3 | lo,
        7 => 0b011 << 13 | rng.below(4) << 11 | small << 6 | mid << 3 | lo,
        8 => 0b1000 << 12 | rng.below(2) << 11 | small << 6 | mid << 3 | lo,
        9 => 0b1001 << 12 | rng.below(2) << 11 | lo << 8 | imm8,
        10 => 0b1010 << 12 | rng.below(2) << 11 | lo << 8 | imm8,
        11 => 0b10110000 << 8 | rng.below(2) << 7 | rng.below(128),
        12 => 0b1011 << 12 | [0b010, 0b110][rng.below(2) as usize] << 9 | rng.below(2) << 8 | imm8,
        13 => 0b1100 << 12 | rng.below(2) << 11 | lo << 8 | imm8,
        14 => 0b1101 << 12 | rng.below(14) << 8 | imm8,
        15 => 0b11100 << 11 | rng.below(0x800),
        16 => 0b11110 << 11 | rng.below(0x800),
        17 => 0b11111 << 11 | rng.below(0x800),
        _ => rng.below(0x10000),
    }
}

fn random_state(rng: &mut Rng, cpu: &mut Cpu) {
    for r in 0..15 {
        cpu.regs[r] = match rng.below(4) {
            0 => rng.next(),
            1 => rng.below(40),
            2 => 0u32.wrapping_sub(rng.below(40)),
            _ => DATA + (rng.below(DATA_SIZE - 0x2000) & !3) + 0x1000,
        };
    }
    cpu.cpsr = 0x1F | (rng.below(16) << 28);
}

fn machine<const C: usize>(seed: u64, bytes: &[u8]) -> (Hardware, Cpu) {
    let mut hw = test_hardware();
    let mut fill = Rng(seed | 1);
    for i in (0..DATA_SIZE).step_by(4) {
        hw.write(C, DATA + i, 4, fill.next());
    }
    for (i, &b) in bytes.iter().enumerate() {
        hw.write(C, CODE + i as u32, 1, b as u32);
    }
    hw.code_hits.clear();
    (hw, Cpu::new(if C == ARM9 { Arch::V5TE } else { Arch::V4T }))
}

fn step<const C: usize>(cpu: &mut Cpu, hw: &mut Hardware) {
    if C == ARM9 {
        cpu.step(&mut Bus9(hw));
    } else {
        cpu.step(&mut Bus7(hw));
    }
}

/// Run random sequences both ways and compare everything observable.
fn compare_blocks<const C: usize>(thumb: bool, cases: u32) {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1 ^ thumb as u64 ^ (C as u64) << 8);
    let mut jit = Jit::default();
    for case in 0..cases {
        let len = 1 + rng.below(8);
        let mut bytes = Vec::new();
        let mut words = Vec::new();
        for _ in 0..len {
            if thumb {
                let h = random_thumb(&mut rng);
                bytes.extend_from_slice(&(h as u16).to_le_bytes());
                words.push(h);
            } else {
                let w = random_arm(&mut rng);
                bytes.extend_from_slice(&w.to_le_bytes());
                words.push(w);
            }
        }
        let seed = rng.next() as u64;
        let (mut hw_j, mut cpu_j) = machine::<C>(seed, &bytes);
        let (mut hw_i, mut cpu_i) = machine::<C>(seed, &bytes);
        random_state(&mut rng, &mut cpu_j);
        if thumb {
            cpu_j.cpsr |= 1 << 5;
        }
        cpu_i.regs = cpu_j.regs;
        cpu_i.cpsr = cpu_j.cpsr;
        cpu_j.regs[15] = CODE;
        cpu_i.regs[15] = CODE;
        let before = (cpu_j.regs, cpu_j.cpsr);

        jit.flush(&mut hw_j);
        let block = jit.lookup::<C>(&mut hw_j, CODE | thumb as u32).expect("block");
        let ran = jit.run(block, &mut cpu_j, &mut hw_j);
        for _ in 0..ran {
            step::<C>(&mut cpu_i, &mut hw_i);
        }
        let describe = || {
            let list: Vec<String> = words.iter().map(|w| format!("{w:x}")).collect();
            format!(
                "case {case}: {} [{}] ran {ran}, start regs {:x?} cpsr {:08x}",
                if thumb { "thumb" } else { "arm" },
                list.join(" "),
                before.0,
                before.1
            )
        };
        assert_eq!(cpu_j.regs, cpu_i.regs, "registers differ, {}", describe());
        assert_eq!(cpu_j.cpsr, cpu_i.cpsr, "cpsr differs, {}", describe());
        assert!(hw_j.main_ram == hw_i.main_ram, "memory differs, {}", describe());
    }
}

#[test]
fn translated_arm_blocks_match_the_interpreter() {
    compare_blocks::<ARM9>(false, 8000);
    compare_blocks::<ARM7>(false, 2000);
}

#[test]
fn translated_thumb_blocks_match_the_interpreter() {
    compare_blocks::<ARM9>(true, 8000);
    compare_blocks::<ARM7>(true, 2000);
}

/// The whole game, with the JIT and without, must agree to the last cycle.
#[test]
fn compiling_changes_nothing_on_the_real_game() {
    use crate::cartridge::Cartridge;
    use crate::system::Nds;
    let make = |compiled: bool| {
        let mut nds = Nds::direct_boot(Cartridge::load("roms/Pokemon SoulSilver.nds").ok()?);
        nds.jit.enabled = compiled;
        Some(nds)
    };
    let (Some(mut fast), Some(mut plain)) = (make(true), make(false)) else { return };
    for frame in 0..700 {
        let keys = if frame % 90 < 20 { 1 << 3 } else { 0 };
        fast.hw.keys = keys;
        plain.hw.keys = keys;
        fast.run_frame();
        plain.run_frame();
        assert_eq!(fast.hw.cycles, plain.hw.cycles, "cycle counts differ at frame {frame}");
        assert_eq!(fast.arm9.regs, plain.arm9.regs, "ARM9 differs at frame {frame}");
        assert_eq!(fast.arm7.regs, plain.arm7.regs, "ARM7 differs at frame {frame}");
        assert_eq!(fast.arm9.cpsr, plain.arm9.cpsr, "ARM9 cpsr differs at frame {frame}");
        assert_eq!(fast.framebuffer(), plain.framebuffer(), "picture differs at frame {frame}");
        assert_eq!(fast.take_audio(), plain.take_audio(), "audio differs at frame {frame}");
    }
}
