//! The functions compiled code calls for everything it doesn't do inline: memory
//! accesses (which tell the block when to leave) and running one instruction in the interpreter.

use crate::cpu::Cpu;
use crate::system::{ARM9, Bus7, Bus9, Hardware};

fn io_flag(hw: &Hardware, before: u64) -> u64 {
    (hw.io_ops != before) as u64
}

pub(super) extern "C" fn load32<const C: usize>(hw: *mut Hardware, addr: u32) -> u64 {
    let hw = unsafe { &mut *hw };
    let io = hw.io_ops;
    let value = hw.read_c::<C, 4>(addr & !3).rotate_right((addr & 3) * 8);
    value as u64 | io_flag(hw, io) << 32
}

pub(super) extern "C" fn load16<const C: usize>(hw: *mut Hardware, addr: u32) -> u64 {
    let hw = unsafe { &mut *hw };
    let io = hw.io_ops;
    let value = hw.read_c::<C, 2>(addr & !1);
    value as u64 | io_flag(hw, io) << 32
}

pub(super) extern "C" fn load16_signed<const C: usize>(hw: *mut Hardware, addr: u32) -> u64 {
    let hw = unsafe { &mut *hw };
    let io = hw.io_ops;
    let value = hw.read_c::<C, 2>(addr & !1) as u16 as i16 as u32;
    value as u64 | io_flag(hw, io) << 32
}

pub(super) extern "C" fn load8<const C: usize>(hw: *mut Hardware, addr: u32) -> u64 {
    let hw = unsafe { &mut *hw };
    let io = hw.io_ops;
    let value = hw.read_c::<C, 1>(addr);
    value as u64 | io_flag(hw, io) << 32
}

pub(super) extern "C" fn load8_signed<const C: usize>(hw: *mut Hardware, addr: u32) -> u64 {
    let hw = unsafe { &mut *hw };
    let io = hw.io_ops;
    let value = hw.read_c::<C, 1>(addr) as u8 as i8 as u32;
    value as u64 | io_flag(hw, io) << 32
}

fn wrote(hw: &Hardware, io: u64) -> u32 {
    (hw.io_ops != io || !hw.code_hits.is_empty()) as u32
}

pub(super) extern "C" fn store32<const C: usize>(hw: *mut Hardware, addr: u32, value: u32) -> u32 {
    let hw = unsafe { &mut *hw };
    let io = hw.io_ops;
    hw.write_c::<C, 4>(addr & !3, value);
    wrote(hw, io)
}

pub(super) extern "C" fn store16<const C: usize>(hw: *mut Hardware, addr: u32, value: u32) -> u32 {
    let hw = unsafe { &mut *hw };
    let io = hw.io_ops;
    hw.write_c::<C, 2>(addr & !1, value & 0xFFFF);
    wrote(hw, io)
}

pub(super) extern "C" fn store8<const C: usize>(hw: *mut Hardware, addr: u32, value: u32) -> u32 {
    let hw = unsafe { &mut *hw };
    let io = hw.io_ops;
    hw.write_c::<C, 1>(addr, value & 0xFF);
    wrote(hw, io)
}

/// Run one instruction in the interpreter. Returns nonzero if the block has to end
/// here because the CPU may now do something other than carry on to the next one.
pub(super) extern "C" fn interpret<const C: usize, const THUMB: bool>(
    cpu: *mut Cpu,
    hw: *mut Hardware,
    instr: u32,
    pc: u32,
) -> u32 {
    let (cpu, hw) = unsafe { (&mut *cpu, &mut *hw) };
    let io = hw.io_ops;
    let mode = cpu.cpsr & 0xFF;
    match (C == ARM9, THUMB) {
        (true, false) => cpu.execute_arm(&mut Bus9(hw), instr, pc),
        (true, true) => cpu.execute_thumb(&mut Bus9(hw), instr, pc),
        (false, false) => cpu.execute_arm(&mut Bus7(hw), instr, pc),
        (false, true) => cpu.execute_thumb(&mut Bus7(hw), instr, pc),
    }
    if C == ARM9 && cpu.cp15_dirty {
        cpu.cp15_dirty = false;
        hw.sync_tcm(cpu);
    }
    let next = pc.wrapping_add(if THUMB { 2 } else { 4 });
    (cpu.regs[15] != next
        || cpu.cpsr & 0xFF != mode
        || cpu.halted
        || hw.io_ops != io
        || hw.code_flush
        || !hw.code_hits.is_empty()) as u32
}
