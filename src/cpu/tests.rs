use super::alu::{shift, Shift};
use super::*;
use crate::cpu::bus::Bus;

struct Ram(Vec<u8>);

impl Ram {
    fn new() -> Ram {
        Ram(vec![0; 0x1_0000])
    }

    fn load_arm(&mut self, addr: u32, words: &[u32]) {
        for (i, w) in words.iter().enumerate() {
            self.write32(addr + i as u32 * 4, *w);
        }
    }

    fn load_thumb(&mut self, addr: u32, halves: &[u16]) {
        for (i, h) in halves.iter().enumerate() {
            self.write16(addr + i as u32 * 2, *h);
        }
    }
}

impl Bus for Ram {
    fn read8(&mut self, a: u32) -> u8 {
        self.0[a as usize]
    }
    fn read16(&mut self, a: u32) -> u16 {
        u16::from_le_bytes([self.0[a as usize], self.0[a as usize + 1]])
    }
    fn read32(&mut self, a: u32) -> u32 {
        u32::from_le_bytes(self.0[a as usize..a as usize + 4].try_into().unwrap())
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.0[a as usize] = v;
    }
    fn write16(&mut self, a: u32, v: u16) {
        self.0[a as usize..a as usize + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn write32(&mut self, a: u32, v: u32) {
        self.0[a as usize..a as usize + 4].copy_from_slice(&v.to_le_bytes());
    }
}

fn run(arch: Arch, ram: &mut Ram, steps: usize) -> Cpu {
    let mut cpu = Cpu::new(arch);
    cpu.set_cpsr(0x1F);
    for _ in 0..steps {
        cpu.step(ram);
    }
    cpu
}

#[test]
fn mov_and_add_immediate() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE3A0_0005, 0xE280_1007]); // MOV r0,#5 ; ADD r1,r0,#7
    let cpu = run(Arch::V4T, &mut ram, 2);
    assert_eq!(cpu.regs[1], 12);
    assert_eq!(cpu.regs[15], 8);
}

#[test]
fn subtract_sets_negative_and_clears_carry_on_borrow() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE3B0_0000, 0xE250_0001]); // MOVS r0,#0 ; SUBS r0,r0,#1
    let mut cpu = run(Arch::V4T, &mut ram, 1);
    assert_ne!(cpu.cpsr & FLAG_Z, 0);
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[0], 0xFFFF_FFFF);
    assert_ne!(cpu.cpsr & FLAG_N, 0);
    assert_eq!(cpu.cpsr & FLAG_C, 0);
}

#[test]
fn loop_sums_one_to_ten() {
    let mut ram = Ram::new();
    ram.load_arm(
        0,
        &[
            0xE3A0_0000, // MOV r0,#0
            0xE3A0_100A, // MOV r1,#10
            0xE080_0001, // loop: ADD r0,r0,r1
            0xE251_1001, // SUBS r1,r1,#1
            0x1AFF_FFFC, // BNE loop
        ],
    );
    let cpu = run(Arch::V4T, &mut ram, 2 + 10 * 3);
    assert_eq!(cpu.regs[0], 55);
}

#[test]
fn store_then_load_word() {
    let mut ram = Ram::new();
    ram.load_arm(
        0,
        &[
            0xE3A0_0C01, // MOV r0,#0x100
            0xE3A0_1042, // MOV r1,#0x42
            0xE580_1000, // STR r1,[r0]
            0xE590_2000, // LDR r2,[r0]
        ],
    );
    let cpu = run(Arch::V4T, &mut ram, 4);
    assert_eq!(ram.read32(0x100), 0x42);
    assert_eq!(cpu.regs[2], 0x42);
}

#[test]
fn unaligned_word_load_rotates() {
    let mut ram = Ram::new();
    ram.write32(0x100, 0x4433_2211);
    ram.load_arm(0, &[0xE590_1001]); // LDR r1,[r0,#1]
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.regs[0] = 0x100;
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[1], 0x1144_3322);
}

#[test]
fn push_and_pop_with_stm_ldm() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE92D_0003, 0xE8BD_000C]); // STMDB sp!,{r0,r1} ; LDMIA sp!,{r2,r3}
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.regs[13] = 0x1000;
    cpu.regs[0] = 0xAAAA;
    cpu.regs[1] = 0xBBBB;
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[13], 0x0FF8);
    cpu.step(&mut ram);
    assert_eq!((cpu.regs[2], cpu.regs[3], cpu.regs[13]), (0xAAAA, 0xBBBB, 0x1000));
}

#[test]
fn multiply() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE002_0190]); // MUL r2,r0,r1
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.regs[0] = 6;
    cpu.regs[1] = 7;
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[2], 42);
}

#[test]
fn bx_switches_to_thumb_and_runs() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE12F_FF10]); // BX r0
    ram.load_thumb(0x200, &[0x2005, 0x3003, 0x0081]); // MOV r0,#5 ; ADD r0,#3 ; LSL r1,r0,#2
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.regs[0] = 0x201;
    cpu.step(&mut ram);
    assert!(cpu.in_thumb());
    assert_eq!(cpu.regs[15], 0x200);
    for _ in 0..3 {
        cpu.step(&mut ram);
    }
    assert_eq!((cpu.regs[0], cpu.regs[1]), (8, 32));
}

#[test]
fn thumb_bl_returns_via_lr() {
    let mut ram = Ram::new();
    // BL +0x10 from 0x100 (prefix hi=0, suffix lo=6 -> target 0x100+4+12), at target: BX lr
    ram.load_thumb(0x100, &[0xF000, 0xF806]);
    ram.load_thumb(0x110, &[0x4770]); // BX lr
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.cpsr |= FLAG_T;
    cpu.regs[15] = 0x100;
    cpu.step(&mut ram);
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[15], 0x110);
    assert_eq!(cpu.regs[14], 0x105);
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[15], 0x104);
    assert!(cpu.in_thumb());
}

#[test]
fn thumb_push_pop_roundtrip() {
    let mut ram = Ram::new();
    ram.load_thumb(0, &[0xB403, 0xBC0C]); // PUSH {r0,r1} ; POP {r2,r3}
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.cpsr |= FLAG_T;
    cpu.regs[13] = 0x1000;
    cpu.regs[0] = 1;
    cpu.regs[1] = 2;
    cpu.step(&mut ram);
    cpu.step(&mut ram);
    assert_eq!((cpu.regs[2], cpu.regs[3], cpu.regs[13]), (1, 2, 0x1000));
}

#[test]
fn irq_enters_handler_and_banks_registers() {
    let mut ram = Ram::new();
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.set_cpsr(0x1F);
    cpu.regs[13] = 0x5000;
    cpu.regs[15] = 0x400;
    cpu.irq_line = true;
    cpu.step(&mut ram);
    assert_eq!(cpu.cpsr & 0x1F, 0x12);
    assert_ne!(cpu.cpsr & FLAG_I, 0);
    assert_eq!(cpu.regs[15], 0x18);
    assert_eq!(cpu.regs[14], 0x404);
    assert_eq!(cpu.spsr() & 0x1F, 0x1F);
    assert_ne!(cpu.regs[13], 0x5000);
}

#[test]
fn irq_masked_when_i_flag_set() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE3A0_0001]);
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.irq_line = true; // reset state has I set
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[0], 1);
}

#[test]
fn swi_enters_supervisor() {
    let mut ram = Ram::new();
    ram.load_arm(0x100, &[0xEF00_0000]); // SWI 0
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.set_cpsr(0x1F);
    cpu.regs[15] = 0x100;
    cpu.step(&mut ram);
    assert_eq!(cpu.cpsr & 0x1F, 0x13);
    assert_eq!((cpu.regs[15], cpu.regs[14]), (8, 0x104));
}

#[test]
fn clz_exists_only_on_armv5() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE16F_1F10]); // CLZ r1,r0
    let mut cpu = Cpu::new(Arch::V5TE);
    cpu.regs[0] = 1;
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[1], 31);

    let mut cpu = Cpu::new(Arch::V4T);
    cpu.step(&mut ram);
    assert_eq!(cpu.cpsr & 0x1F, 0x1B);
}

#[test]
fn cp15_wait_for_interrupt_halts_until_irq() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xEE07_0F90]); // MCR p15,0,r0,c7,c0,4
    let mut cpu = Cpu::new(Arch::V5TE);
    cpu.step(&mut ram);
    assert!(cpu.halted);
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[15], 4);
    cpu.irq_line = true;
    cpu.step(&mut ram);
    assert!(!cpu.halted);
}

#[test]
fn qadd_saturates_and_sets_q() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE101_2050]); // QADD r2,r0,r1
    let mut cpu = Cpu::new(Arch::V5TE);
    cpu.regs[0] = i32::MAX as u32;
    cpu.regs[1] = 1;
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[2], i32::MAX as u32);
    assert_ne!(cpu.cpsr & FLAG_Q, 0);
}

#[test]
fn barrel_shifter_edge_cases() {
    assert_eq!(shift(Shift::Lsr, 0x8000_0000, 0, false, true), (0, true));
    assert_eq!(shift(Shift::Asr, 0x8000_0000, 0, false, true), (0xFFFF_FFFF, true));
    assert_eq!(shift(Shift::Ror, 1, 0, true, true), (0x8000_0000, true));
    assert_eq!(shift(Shift::Lsl, 1, 32, false, false), (0, true));
    assert_eq!(shift(Shift::Lsl, 1, 33, true, false), (0, false));
    assert_eq!(shift(Shift::Ror, 0x8000_0000, 32, false, false), (0x8000_0000, true));
    assert_eq!(shift(Shift::Lsr, 5, 0, true, false), (5, true));
}

/// The lookup table must agree with the textbook definition of every condition code.
#[test]
fn condition_table_matches_the_definitions() {
    for cond in 0..16u32 {
        for flags in 0..16u32 {
            let mut cpu = Cpu::new(Arch::V4T);
            cpu.cpsr = flags << 28;
            let (n, z, c, v) = (flags & 8 != 0, flags & 4 != 0, flags & 2 != 0, flags & 1 != 0);
            let expected = match cond {
                0 => z,
                1 => !z,
                2 => c,
                3 => !c,
                4 => n,
                5 => !n,
                6 => v,
                7 => !v,
                8 => c && !z,
                9 => !c || z,
                10 => n == v,
                11 => n != v,
                12 => !z && n == v,
                13 => z || n != v,
                _ => true,
            };
            assert_eq!(cpu.check_cond(cond), expected, "condition {cond:#x} with flags {flags:#06b}");
        }
    }
}

#[test]
fn adds_sets_overflow_and_negative_but_not_carry() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE091_2000]); // ADDS r2,r1,r0
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.regs[1] = 0x7FFF_FFFF;
    cpu.regs[0] = 1;
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[2], 0x8000_0000);
    assert_eq!(cpu.cpsr & 0xF000_0000, FLAG_N | FLAG_V);
}

#[test]
fn logical_operations_keep_overflow_and_take_the_shifter_carry() {
    let mut ram = Ram::new();
    // MOVS r1,r0,LSL #1 : carry out is bit 31 of r0.
    ram.load_arm(0, &[0xE1B0_1080]);
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.cpsr |= FLAG_V;
    cpu.regs[0] = 0x8000_0000;
    cpu.step(&mut ram);
    assert_eq!(cpu.regs[1], 0);
    assert_eq!(cpu.cpsr & 0xF000_0000, FLAG_Z | FLAG_C | FLAG_V, "V survives a logical op");
}

#[test]
fn compare_equal_sets_zero_and_carry() {
    let mut ram = Ram::new();
    ram.load_arm(0, &[0xE150_0001]); // CMP r0,r1
    let mut cpu = Cpu::new(Arch::V4T);
    cpu.regs[0] = 5;
    cpu.regs[1] = 5;
    cpu.step(&mut ram);
    assert_eq!(cpu.cpsr & 0xF000_0000, FLAG_Z | FLAG_C);
}
