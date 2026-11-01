use std::collections::VecDeque;

use crate::system::irq::{IRQ_IPC_RECV_NOT_EMPTY, IRQ_IPC_SEND_EMPTY, IRQ_IPC_SYNC};
use crate::system::Hardware;

const FIFO_DEPTH: usize = 16;

/// Inter-processor communication. Index 0 is the ARM9, 1 the ARM7;
/// `fifo[i]` carries words sent by CPU `i`.
#[derive(Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Ipc {
    sync_out: [u32; 2],
    sync_irq_enable: [bool; 2],
    fifo: [VecDeque<u32>; 2],
    send_empty_irq: [bool; 2],
    recv_not_empty_irq: [bool; 2],
    enabled: [bool; 2],
    error: [bool; 2],
    last_received: [u32; 2],
}

impl Hardware {
    pub fn ipc_sync_read(&self, cpu: usize) -> u32 {
        let ipc = &self.ipc;
        (ipc.sync_out[1 - cpu] & 0xF) | (ipc.sync_out[cpu] & 0xF) << 8 | (ipc.sync_irq_enable[cpu] as u32) << 14
    }

    pub fn ipc_sync_write(&mut self, cpu: usize, value: u32) {
        let other = 1 - cpu;
        self.ipc.sync_out[cpu] = (value >> 8) & 0xF;
        self.ipc.sync_irq_enable[cpu] = value & (1 << 14) != 0;
        if value & (1 << 13) != 0 && self.ipc.sync_irq_enable[other] {
            self.irq[other].raise(IRQ_IPC_SYNC);
        }
    }

    pub fn ipc_fifo_cnt_read(&self, cpu: usize) -> u32 {
        let ipc = &self.ipc;
        let (send, recv) = (&ipc.fifo[cpu], &ipc.fifo[1 - cpu]);
        send.is_empty() as u32
            | ((send.len() == FIFO_DEPTH) as u32) << 1
            | (ipc.send_empty_irq[cpu] as u32) << 2
            | (recv.is_empty() as u32) << 8
            | ((recv.len() == FIFO_DEPTH) as u32) << 9
            | (ipc.recv_not_empty_irq[cpu] as u32) << 10
            | (ipc.error[cpu] as u32) << 14
            | (ipc.enabled[cpu] as u32) << 15
    }

    pub fn ipc_fifo_cnt_write(&mut self, cpu: usize, value: u32) {
        let was_send_irq = self.ipc.send_empty_irq[cpu];
        self.ipc.send_empty_irq[cpu] = value & (1 << 2) != 0;
        if value & (1 << 3) != 0 {
            self.ipc.fifo[cpu].clear();
            if self.ipc.send_empty_irq[cpu] {
                self.irq[cpu].raise(IRQ_IPC_SEND_EMPTY);
            }
        } else if !was_send_irq && self.ipc.send_empty_irq[cpu] && self.ipc.fifo[cpu].is_empty() {
            self.irq[cpu].raise(IRQ_IPC_SEND_EMPTY);
        }
        let was_recv_irq = self.ipc.recv_not_empty_irq[cpu];
        self.ipc.recv_not_empty_irq[cpu] = value & (1 << 10) != 0;
        if !was_recv_irq && self.ipc.recv_not_empty_irq[cpu] && !self.ipc.fifo[1 - cpu].is_empty() {
            self.irq[cpu].raise(IRQ_IPC_RECV_NOT_EMPTY);
        }
        if value & (1 << 14) != 0 {
            self.ipc.error[cpu] = false;
        }
        self.ipc.enabled[cpu] = value & (1 << 15) != 0;
    }

    pub fn ipc_fifo_send(&mut self, cpu: usize, word: u32) {
        if !self.ipc.enabled[cpu] {
            return;
        }
        if self.ipc.fifo[cpu].len() >= FIFO_DEPTH {
            self.ipc.error[cpu] = true;
            return;
        }
        if self.trace_io {
            let line = format!("ARM{} FIFO send {word:08x} (tag {:x} err {} data {:x}) pc={:08x}", 9 - 2 * cpu, word & 0x1F, word >> 5 & 1, word >> 6, self.cur_pc[cpu]);
            self.events.push(line);
        }
        let was_empty = self.ipc.fifo[cpu].is_empty();
        self.ipc.fifo[cpu].push_back(word);
        let other = 1 - cpu;
        if was_empty && self.ipc.recv_not_empty_irq[other] {
            self.irq[other].raise(IRQ_IPC_RECV_NOT_EMPTY);
        }
    }

    pub fn ipc_fifo_recv(&mut self, cpu: usize) -> u32 {
        self.effects = self.effects.wrapping_add(1);
        let other = 1 - cpu;
        if !self.ipc.enabled[cpu] {
            return self.ipc.last_received[cpu];
        }
        match self.ipc.fifo[other].pop_front() {
            Some(word) => {
                self.ipc.last_received[cpu] = word;
                if self.ipc.fifo[other].is_empty() && self.ipc.send_empty_irq[other] {
                    self.irq[other].raise(IRQ_IPC_SEND_EMPTY);
                }
                word
            }
            None => {
                self.ipc.error[cpu] = true;
                self.ipc.last_received[cpu]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::system::tests::test_hardware;

    #[test]
    fn fifo_delivers_in_order_and_signals_irq() {
        let mut hw = test_hardware();
        hw.ipc_fifo_cnt_write(0, 1 << 15);
        hw.ipc_fifo_cnt_write(1, (1 << 15) | (1 << 10));
        hw.ipc_fifo_send(0, 0xAA);
        hw.ipc_fifo_send(0, 0xBB);
        assert_ne!(hw.irq[1].flags & (1 << 18), 0);
        assert_eq!(hw.ipc_fifo_cnt_read(1) & (1 << 8), 0);
        assert_eq!(hw.ipc_fifo_recv(1), 0xAA);
        assert_eq!(hw.ipc_fifo_recv(1), 0xBB);
        assert_ne!(hw.ipc_fifo_cnt_read(1) & (1 << 8), 0);
    }

    #[test]
    fn sync_value_is_visible_to_other_cpu() {
        let mut hw = test_hardware();
        hw.ipc_sync_write(0, 0x0500);
        assert_eq!(hw.ipc_sync_read(1) & 0xF, 5);
    }
}
