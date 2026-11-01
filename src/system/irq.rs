pub const IRQ_VBLANK: u32 = 1 << 0;
pub const IRQ_HBLANK: u32 = 1 << 1;
pub const IRQ_VCOUNT: u32 = 1 << 2;
pub const IRQ_TIMER0: u32 = 1 << 3;
pub const IRQ_DMA0: u32 = 1 << 8;
pub const IRQ_IPC_SYNC: u32 = 1 << 16;
pub const IRQ_IPC_SEND_EMPTY: u32 = 1 << 17;
pub const IRQ_IPC_RECV_NOT_EMPTY: u32 = 1 << 18;
pub const IRQ_CARD_DONE: u32 = 1 << 19;
pub const IRQ_SPI: u32 = 1 << 23;

#[derive(Clone, Copy, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Irq {
    pub ime: bool,
    pub ie: u32,
    pub flags: u32,
}

impl Irq {
    pub fn raise(&mut self, bits: u32) {
        self.flags |= bits;
    }

    /// Level seen by the CPU's IRQ input.
    pub fn line(&self) -> bool {
        self.ime && self.ie & self.flags != 0
    }

    /// Level that ends HALT (ignores IME).
    pub fn wake(&self) -> bool {
        self.ie & self.flags != 0
    }
}
