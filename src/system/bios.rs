//! Simulated BIOS and boot ROM. There is no BIOS image: the exception vectors
//! and system calls are handled natively when a CPU reaches the vector
//! addresses, and `direct_boot` leaves memory the way the firmware would.

use super::Hardware;
use crate::cpu::{Cpu, FLAG_F, FLAG_I};

const ARM9: usize = 0;

struct Vectors {
    swi: u32,
    irq: u32,
    /// Where an IRQ handler returns to; we restore registers and leave there.
    irq_return: u32,
}

/// Distance from a CPU's vector base within which `intercept` may have work to do.
pub const VECTOR_SPAN: u32 = 0x200;

const VECTORS9: Vectors = Vectors { swi: 0xFFFF_0008, irq: 0xFFFF_0018, irq_return: 0xFFFF_0200 };
const VECTORS7: Vectors = Vectors { swi: 0x0000_0008, irq: 0x0000_0018, irq_return: 0x0000_0200 };

fn irq_handler_addr(which: usize, hw: &Hardware) -> u32 {
    if which == ARM9 { (hw.dtcm_reg & 0xFFFF_F000) + 0x3FFC } else { 0x0380_FFFC }
}

fn bios_irq_flags_addr(which: usize, hw: &Hardware) -> u32 {
    irq_handler_addr(which, hw) - 4
}

/// Handle a CPU sitting on a BIOS vector. Returns the cycles the call cost,
/// or None if the CPU isn't on a vector.
pub fn intercept(which: usize, cpu: &mut Cpu, hw: &mut Hardware, wait: &mut Option<u32>) -> Option<u32> {
    let vectors = if which == ARM9 { &VECTORS9 } else { &VECTORS7 };
    let pc = cpu.regs[15];
    if pc > vectors.irq_return || pc & 0xFFFF_0000 != vectors.swi & 0xFFFF_0000 || cpu.in_thumb() {
        return None;
    }
    if pc == vectors.irq {
        // STMFD sp!,{r0-r3,r12,lr} ; LDR pc,[handler]
        let sp = cpu.regs[13].wrapping_sub(24);
        for (i, r) in [0usize, 1, 2, 3, 12, 14].into_iter().enumerate() {
            hw.write(which, sp + i as u32 * 4, 4, cpu.regs[r]);
        }
        cpu.regs[13] = sp;
        cpu.regs[14] = vectors.irq_return;
        let handler = hw.read(which, irq_handler_addr(which, hw), 4);
        if which == ARM9 { cpu.branch_exchange_hle(handler) } else { cpu.branch_hle(handler) }
        Some(10)
    } else if pc == vectors.irq_return {
        // LDMFD sp!,{r0-r3,r12,lr} ; SUBS pc,lr,#4
        let sp = cpu.regs[13];
        for (i, r) in [0usize, 1, 2, 3, 12, 14].into_iter().enumerate() {
            cpu.regs[r] = hw.read(which, sp + i as u32 * 4, 4);
        }
        cpu.regs[13] = sp.wrapping_add(24);
        cpu.exception_return(cpu.regs[14].wrapping_sub(4));
        Some(10)
    } else if pc == vectors.swi {
        Some(swi(which, cpu, hw, wait))
    } else {
        None
    }
}

/// Run a system call; returns its cost in cycles.
fn swi(which: usize, cpu: &mut Cpu, hw: &mut Hardware, wait: &mut Option<u32>) -> u32 {
    let lr = cpu.regs[14];
    let number = match *wait {
        Some(_) => 0x04,
        None if cpu.spsr() & crate::cpu::FLAG_T != 0 => hw.read(which, lr.wrapping_sub(2), 1),
        None => (hw.read(which, lr.wrapping_sub(4), 4) >> 16) & 0xFF,
    };
    let r = cpu.regs;
    let mut cost = 20;
    match number {
        0x03 => cost += r[0].saturating_mul(4),
        0x04 | 0x05 => {
            let flags_addr = bios_irq_flags_addr(which, hw);
            let flags = match *wait {
                Some(flags) => flags,
                None => {
                    let (discard, flags) = if number == 0x05 { (1, 1) } else { (r[0], r[1]) };
                    if discard != 0 {
                        let cur = hw.read(which, flags_addr, 4);
                        hw.write(which, flags_addr, 4, cur & !flags);
                    }
                    hw.irq[which].ime = true;
                    flags
                }
            };
            let cur = hw.read(which, flags_addr, 4);
            if cur & flags != 0 {
                hw.write(which, flags_addr, 4, cur & !flags);
                *wait = None;
            } else {
                *wait = Some(flags);
                cpu.halted = true;
                cpu.cpsr &= !FLAG_I;
                return cost;
            }
        }
        0x06 | 0x07 => cpu.halted = true,
        0x09 => {
            let (n, d) = (r[0] as i32, r[1] as i32);
            if d != 0 {
                let q = n.wrapping_div(d);
                cpu.regs[0] = q as u32;
                cpu.regs[1] = n.wrapping_rem(d) as u32;
                cpu.regs[3] = q.unsigned_abs();
            }
        }
        0x1A if which != ARM9 => cpu.regs[0] = super::peripherals::sound::sine_table(r[0]),
        0x1B if which != ARM9 => cpu.regs[0] = super::peripherals::sound::pitch_table(r[0]),
        0x1C if which != ARM9 => cpu.regs[0] = super::peripherals::sound::volume_table(r[0]),
        0x0B => cpu_set(which, hw, r[0], r[1], r[2], false),
        0x0C => cpu_set(which, hw, r[0], r[1], r[2], true),
        0x0D => cpu.regs[0] = super::io::isqrt(r[0] as u64) as u32,
        0x0E => {
            let (mut crc, mut last) = (r[0] & 0xFFFF, 0);
            for i in 0..r[2] / 2 {
                last = hw.read(which, r[1] + i * 2, 2);
                crc ^= last;
                for _ in 0..16 {
                    crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
                }
            }
            cpu.regs[0] = crc;
            cpu.regs[3] = last;
        }
        0x0F => cpu.regs[0] = 0,
        0x11 | 0x12 => {
            let out = lz77_decompress(which, hw, r[0]);
            write_decompressed(which, hw, r[1], &out, number == 0x12);
        }
        0x14 | 0x15 => {
            let out = rl_decompress(which, hw, r[0]);
            write_decompressed(which, hw, r[1], &out, number == 0x15);
        }
        n => hw.note(format!("ARM{} SWI {n:#04x}", if which == ARM9 { 9 } else { 7 })),
    }
    *wait = None;
    cpu.exception_return(lr);
    cost
}

fn cpu_set(which: usize, hw: &mut Hardware, mut src: u32, mut dst: u32, control: u32, fast: bool) {
    let mut count = control & 0x1F_FFFF;
    let fill = control & (1 << 24) != 0;
    let word = fast || control & (1 << 26) != 0;
    if fast {
        count = (count + 7) & !7;
    }
    let step = if word { 4 } else { 2 };
    let fill_value = if fill { hw.read(which, src, step) } else { 0 };
    for _ in 0..count {
        let v = if fill { fill_value } else { hw.read(which, src, step) };
        hw.write(which, dst, step, v);
        src = src.wrapping_add(step);
        dst = dst.wrapping_add(step);
    }
}

fn lz77_decompress(which: usize, hw: &mut Hardware, mut src: u32) -> Vec<u8> {
    let size = (hw.read(which, src, 4) >> 8) as usize;
    src += 4;
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let flags = hw.read(which, src, 1);
        src += 1;
        for bit in (0..8).rev() {
            if out.len() >= size {
                break;
            }
            if flags & (1 << bit) == 0 {
                out.push(hw.read(which, src, 1) as u8);
                src += 1;
            } else {
                let b1 = hw.read(which, src, 1) as usize;
                let b2 = hw.read(which, src + 1, 1) as usize;
                src += 2;
                let len = (b1 >> 4) + 3;
                let disp = ((b1 & 0xF) << 8 | b2) + 1;
                for _ in 0..len {
                    if out.len() >= size || disp > out.len() {
                        break;
                    }
                    out.push(out[out.len() - disp]);
                }
            }
        }
    }
    out
}

fn rl_decompress(which: usize, hw: &mut Hardware, mut src: u32) -> Vec<u8> {
    let size = (hw.read(which, src, 4) >> 8) as usize;
    src += 4;
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let flag = hw.read(which, src, 1);
        src += 1;
        if flag & 0x80 != 0 {
            let byte = hw.read(which, src, 1) as u8;
            src += 1;
            out.extend(std::iter::repeat(byte).take((flag & 0x7F) as usize + 3));
        } else {
            for _ in 0..(flag & 0x7F) + 1 {
                out.push(hw.read(which, src, 1) as u8);
                src += 1;
            }
        }
    }
    out.truncate(size);
    out
}

/// The VRAM variants of the decompressors must write 16 bits at a time.
fn write_decompressed(which: usize, hw: &mut Hardware, dst: u32, data: &[u8], halfwords: bool) {
    if halfwords {
        for (i, pair) in data.chunks(2).enumerate() {
            let v = pair[0] as u32 | (*pair.get(1).unwrap_or(&0) as u32) << 8;
            hw.write(which, dst + i as u32 * 2, 2, v);
        }
    } else {
        for (i, &b) in data.iter().enumerate() {
            hw.write(which, dst + i as u32, 1, b as u32);
        }
    }
}

/// Leave the console as the firmware's boot sequence would after loading the
/// cartridge's ARM9 and ARM7 binaries.
pub fn direct_boot(arm9: &mut Cpu, arm7: &mut Cpu, hw: &mut Hardware) {
    // The boot ROM gives the ARM7 all of shared WRAM; its loader relies on it.
    hw.wramcnt = 3;
    hw.io[0][0x247] = 3;

    let header = hw.cart.header.clone();
    let rom_header = hw.cart.rom[..0x170].to_vec();
    let code9 = hw.cart.arm9_code().to_vec();
    let code7 = hw.cart.arm7_code().to_vec();

    for (i, chunk) in code9.chunks(4).enumerate() {
        let mut w = [0u8; 4];
        w[..chunk.len()].copy_from_slice(chunk);
        hw.write(0, header.arm9.ram_address + i as u32 * 4, 4, u32::from_le_bytes(w));
    }
    for (i, chunk) in code7.chunks(4).enumerate() {
        let mut w = [0u8; 4];
        w[..chunk.len()].copy_from_slice(chunk);
        hw.write(1, header.arm7.ram_address + i as u32 * 4, 4, u32::from_le_bytes(w));
    }

    for (i, chunk) in rom_header.chunks(4).enumerate() {
        hw.write(0, 0x027F_FE00 + i as u32 * 4, 4, u32::from_le_bytes(chunk.try_into().unwrap()));
    }
    let chip_id = hw.card_chip_id();
    let header_crc = header.header_crc as u32;
    let secure_crc = u16::from_le_bytes([hw.cart.rom[0x6C], hw.cart.rom[0x6D]]) as u32;
    for base in [0x027F_F800u32, 0x027F_FC00] {
        hw.write(0, base, 4, chip_id);
        hw.write(0, base + 4, 4, chip_id);
        hw.write(0, base + 8, 2, header_crc);
        hw.write(0, base + 10, 2, secure_crc);
        hw.write(0, base + 12, 2, 0x5835);
    }
    hw.write(0, 0x027F_F850, 2, 0x5835);
    hw.write(0, 0x027F_F880, 2, 0x0007);
    hw.write(0, 0x027F_F884, 2, 0x0006);
    hw.write(0, 0x027F_FC10, 2, 0x5835);
    hw.write(0, 0x027F_FC30, 2, 0xFFFF);
    hw.write(0, 0x027F_FC40, 2, 0x0001);
    for (i, b) in user_settings().iter().enumerate() {
        hw.write(0, 0x027F_FC80 + i as u32, 1, *b as u32);
    }

    arm9.cp15.dtcm = 0x0080_000A;
    arm9.cp15.itcm = 0x0000_000C;
    arm9.cp15.control |= (1 << 16) | (1 << 18);
    hw.dtcm_reg = arm9.cp15.dtcm;
    hw.itcm_reg = arm9.cp15.itcm;
    hw.tcm_enable = (true, true);
    hw.refresh_tcm();

    boot_cpu(arm9, header.arm9.entry, [0x0080_3FC0, 0x0080_3FA0, 0x0080_3EC0]);
    boot_cpu(arm7, header.arm7.entry, [0x0380_FFC0, 0x0380_FF80, 0x0380_FD80]);
}

fn boot_cpu(cpu: &mut Cpu, entry: u32, [sp_svc, sp_irq, sp_sys]: [u32; 3]) {
    cpu.set_cpsr(0x13 | FLAG_I | FLAG_F);
    cpu.regs[13] = sp_svc;
    cpu.set_cpsr(0x12 | FLAG_I | FLAG_F);
    cpu.regs[13] = sp_irq;
    cpu.set_cpsr(0x1F | FLAG_I | FLAG_F);
    cpu.regs[13] = sp_sys;
    cpu.regs[15] = entry;
}

/// The 0x70-byte user settings block the firmware copies to 0x027FFC80.
pub(super) fn user_settings() -> [u8; 0x70] {
    let mut s = [0u8; 0x70];
    s[0] = 5; // version
    s[3] = 1; // birthday: January
    s[4] = 1; // ... 1st
    for (i, c) in "Player".encode_utf16().enumerate() {
        s[6 + i * 2..8 + i * 2].copy_from_slice(&c.to_le_bytes());
    }
    s[0x1A] = 6; // nickname length
    // Touch calibration maps ADC value = 16 * screen coordinate:
    // ADC (0x200,0x200) <-> pixel (32,32), ADC (0xE00,0xA00) <-> pixel (224,160).
    s[0x58..0x5A].copy_from_slice(&0x0200u16.to_le_bytes());
    s[0x5A..0x5C].copy_from_slice(&0x0200u16.to_le_bytes());
    s[0x5C] = 32;
    s[0x5D] = 32;
    s[0x5E..0x60].copy_from_slice(&0x0E00u16.to_le_bytes());
    s[0x60..0x62].copy_from_slice(&0x0A00u16.to_le_bytes());
    s[0x62] = 224;
    s[0x63] = 160;
    s[0x64] = 1; // language: English
    s
}
