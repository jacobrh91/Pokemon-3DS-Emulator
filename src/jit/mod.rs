//! A JIT for ARM-state code running from main RAM, on Apple silicon.
//!
//! Straight-line runs of instructions become native blocks. Simple instructions
//! (data processing, single loads and stores, branches) are translated; anything
//! else is handed to the interpreter one instruction at a time from inside the
//! block, so every instruction behaves exactly as it does when interpreted.
//!
//! A block runs only when the cycle budget covers all of it, and it leaves early
//! after anything that could change what the CPU does next: an access to IO
//! registers, a write to code, a branch taken by the interpreter, a mode change.
//! That keeps cycle counts and interrupt timing identical to interpretation.

pub mod asm;
pub mod mem;
mod translate;

use translate::MAX_BLOCK;

use crate::cpu::Cpu;
use crate::system::Hardware;
use asm::Asm;
use mem::CodeMemory;
use std::collections::HashMap;

const TABLE_BITS: usize = 15;
const CODE_SIZE: usize = 32 << 20;

type BlockFn = unsafe extern "C" fn(*mut Cpu, *mut Hardware, u32) -> u32;

/// A compiled block: `len` instructions starting at `pc`, entered at `entry`. Blocks are
/// named by their address, with bit 0 set for Thumb code. A `len` of zero marks code
/// that can't be compiled.
#[derive(Clone, Copy)]
pub struct Block {
    pc: u32,
    pub len: u32,
    entry: usize,
    /// Where other blocks jump to run this one without going through the dispatcher.
    chain: usize,
}

const EMPTY: Block = Block { pc: u32::MAX, len: 0, entry: 0, chain: 0 };

/// A jump in compiled code that is aimed at another block once that exists.
#[derive(Clone, Copy)]
struct Site {
    /// Address of the branch instruction.
    at: usize,
    /// Where it goes when its target is gone: the stub that returns to the dispatcher.
    stub: usize,
}

struct Cache {
    /// Direct-mapped front for the map.
    table: Vec<Block>,
    map: HashMap<u32, Block>,
    /// Blocks cut short to fit what is left of a time slice, by (address, length).
    short: HashMap<(u32, u32), Block>,
}

impl Cache {
    fn new() -> Cache {
        Cache { table: vec![EMPTY; 1 << TABLE_BITS], map: HashMap::new(), short: HashMap::new() }
    }

    fn slot(pc: u32) -> usize {
        (pc >> 1) as usize & ((1 << TABLE_BITS) - 1)
    }

    fn remove(&mut self, pc: u32) {
        self.map.remove(&pc);
        for len in 1..MAX_BLOCK {
            self.short.remove(&(pc, len));
        }
        let slot = &mut self.table[Cache::slot(pc)];
        if slot.pc == pc {
            *slot = EMPTY;
        }
    }
}

pub struct Jit {
    pub enabled: bool,
    code: Option<CodeMemory>,
    caches: [Cache; 2],
    /// For each page of main RAM holding compiled code, the blocks made from it.
    pages: HashMap<u32, Vec<(usize, u32)>>,
    /// For each block (by CPU and address) the jumps that lead to it.
    incoming: HashMap<(usize, u32), Vec<Site>>,
    /// Addresses (ARM9) where the emulator has to take control before the instruction runs:
    /// no block starts or continues through them.
    pub hooks: Vec<u32>,
}

impl Default for Jit {
    fn default() -> Jit {
        Jit { enabled: true, code: None, caches: [Cache::new(), Cache::new()], pages: HashMap::new(), incoming: HashMap::new(), hooks: Vec::new() }
    }
}

impl Jit {
    /// Drop all compiled code.
    pub fn flush(&mut self, hw: &mut Hardware) {
        for cache in &mut self.caches {
            cache.map.clear();
            cache.short.clear();
            cache.table.fill(EMPTY);
        }
        self.pages.clear();
        self.incoming.clear();
        hw.code_map.fill(0);
        hw.code_hits.clear();
        hw.code_flush = false;
        if let Some(code) = &mut self.code {
            code.reset();
        }
    }

    /// Replace the hooked addresses, dropping the compiled code if they changed.
    pub fn set_hooks(&mut self, hooks: Vec<u32>, hw: &mut Hardware) {
        if hooks != self.hooks {
            self.hooks = hooks;
            self.flush(hw);
        }
    }

    /// Discard blocks whose code was written to (or everything, if memory moved).
    #[cold]
    pub fn process(&mut self, hw: &mut Hardware) {
        if hw.code_flush {
            return self.flush(hw);
        }
        for page in std::mem::take(&mut hw.code_hits) {
            for (cpu, pc) in self.pages.remove(&page).unwrap_or_default() {
                self.caches[cpu].remove(pc);
                // Whatever jumped into it goes back through the dispatcher.
                if let (Some(sites), Some(code)) = (self.incoming.get(&(cpu, pc)), &mut self.code) {
                    let patches: Vec<_> = sites.iter().map(|s| (s.at, Asm::branch_word(s.at, s.stub))).collect();
                    code.patch(&patches);
                }
            }
        }
    }

    /// The block starting at `pc` (bit 0 set for Thumb), compiling it if needed.
    #[inline(always)]
    pub fn lookup<const CPU: usize>(&mut self, hw: &mut Hardware, pc: u32) -> Option<Block> {
        let block = self.caches[CPU].table[Cache::slot(pc)];
        if block.pc == pc {
            return (block.len != 0).then_some(block);
        }
        self.lookup_slow::<CPU>(hw, pc)
    }

    #[inline(never)]
    fn lookup_slow<const CPU: usize>(&mut self, hw: &mut Hardware, pc: u32) -> Option<Block> {
        if (pc & 1 == 0 && pc & 3 != 0) || hw.code_page(CPU, pc & !1).is_none() {
            return None;
        }
        let block = match self.caches[CPU].map.get(&pc) {
            Some(&block) => block,
            None => self.compile::<CPU>(hw, pc, MAX_BLOCK),
        };
        self.caches[CPU].table[Cache::slot(pc)] = block;
        (block.len != 0).then_some(block)
    }

    /// A block of at most `max_len` instructions, remembered under its address (when full
    /// length) or under its address and length.
    fn compile<const CPU: usize>(&mut self, hw: &mut Hardware, pc: u32, max_len: u32) -> Block {
        let none = Block { pc, len: 0, entry: 0, chain: 0 };
        let remember = |cache: &mut Cache, block: Block| {
            if max_len == MAX_BLOCK {
                cache.map.insert(pc, block);
            } else {
                cache.short.insert((pc, max_len), block);
            }
        };
        let Some(compiled) = translate::compile::<CPU>(hw, &self.hooks, pc, max_len) else {
            remember(&mut self.caches[CPU], none);
            return none;
        };
        if self.code.is_none() {
            self.code = CodeMemory::new(CODE_SIZE);
        }
        let Some(code) = &mut self.code else { return none };
        let entry = match code.install(&compiled.code) {
            Some(entry) => entry,
            None => {
                // Out of space: start over.
                self.flush(hw);
                match self.code.as_mut().and_then(|c| c.install(&compiled.code)) {
                    Some(entry) => entry,
                    None => return none,
                }
            }
        };
        let block = Block { pc, len: compiled.len, entry, chain: entry + 4 * compiled.chain };
        remember(&mut self.caches[CPU], block);
        self.link::<CPU>(&compiled, block, max_len == MAX_BLOCK);
        for page in compiled.pages {
            hw.code_map[page as usize] = 1;
            self.pages.entry(page).or_default().push((CPU, pc));
        }
        block
    }

    /// Aim the jumps out of a new block at the blocks they lead to, and the jumps into it
    /// (if it can be jumped to) at it.
    fn link<const CPU: usize>(&mut self, compiled: &translate::Compiled, block: Block, whole: bool) {
        let Some(code) = &mut self.code else { return };
        let stub = block.entry + 4 * compiled.bail;
        let mut patches = Vec::new();
        for link in &compiled.links {
            let site = Site { at: block.entry + 4 * link.site, stub };
            let target = self.caches[CPU].map.get(&link.key).filter(|b| b.len != 0);
            if let Some(target) = target {
                patches.push((site.at, Asm::branch_word(site.at, target.chain)));
            }
            self.incoming.entry((CPU, link.key)).or_default().push(site);
        }
        if whole {
            for site in self.incoming.get(&(CPU, block.pc)).into_iter().flatten() {
                patches.push((site.at, Asm::branch_word(site.at, block.chain)));
            }
        }
        if !patches.is_empty() {
            code.patch(&patches);
        }
    }

    /// A block starting at `pc` that is at most `len` instructions long, for when that is
    /// all that is left of the time the CPU may run.
    pub fn lookup_short<const CPU: usize>(&mut self, hw: &mut Hardware, pc: u32, len: u32) -> Option<Block> {
        let block = match self.caches[CPU].short.get(&(pc, len)) {
            Some(&block) => block,
            None => self.compile::<CPU>(hw, pc, len),
        };
        (block.len != 0).then_some(block)
    }

    /// Run a block, and any blocks it leads on to, for at most `allowed` instructions in
    /// all; returns the number executed (their cycle cost).
    #[inline(always)]
    pub fn run(&self, block: Block, cpu: &mut Cpu, hw: &mut Hardware, allowed: u32) -> u32 {
        let entry: BlockFn = unsafe { std::mem::transmute(block.entry) };
        hw.fast = crate::system::FastPtrs { main_ram: hw.main_ram.as_mut_ptr() as usize, code_map: hw.code_map.as_mut_ptr() as usize };
        unsafe { entry(cpu, hw, allowed) }
    }
}

#[cfg(test)]
mod tests;
