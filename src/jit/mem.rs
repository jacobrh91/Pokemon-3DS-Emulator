//! Executable memory. On Apple silicon a `MAP_JIT` mapping is either writable or
//! executable for the current thread at any moment; `pthread_jit_write_protect_np`
//! flips between the two.

use std::ffi::c_void;

unsafe extern "C" {
    fn sys_icache_invalidate(start: *mut c_void, len: usize);
}

pub struct CodeMemory {
    base: *mut u8,
    size: usize,
    used: usize,
}

// The mapping is only touched from the thread that owns the emulator.
unsafe impl Send for CodeMemory {}

impl CodeMemory {
    pub fn new(size: usize) -> Option<CodeMemory> {
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT,
                -1,
                0,
            )
        };
        (base != libc::MAP_FAILED).then(|| CodeMemory { base: base as *mut u8, size, used: 0 })
    }

    /// Copy `code` in and return its entry address, or `None` when full.
    pub fn install(&mut self, code: &[u32]) -> Option<usize> {
        let bytes = code.len() * 4;
        // Keep entries 16-byte aligned, which the branch predictors like.
        let start = (self.used + 15) & !15;
        if start + bytes > self.size {
            return None;
        }
        unsafe {
            let at = self.base.add(start);
            libc::pthread_jit_write_protect_np(0);
            std::ptr::copy_nonoverlapping(code.as_ptr() as *const u8, at, bytes);
            libc::pthread_jit_write_protect_np(1);
            sys_icache_invalidate(at as *mut c_void, bytes);
            self.used = start + bytes;
            Some(at as usize)
        }
    }

    /// Forget everything compiled so far.
    pub fn reset(&mut self) {
        self.used = 0;
    }
}

impl Drop for CodeMemory {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.base as *mut c_void, self.size);
        }
    }
}
