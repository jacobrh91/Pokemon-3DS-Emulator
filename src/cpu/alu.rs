#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shift {
    Lsl,
    Lsr,
    Asr,
    Ror,
}

impl Shift {
    pub fn from_bits(bits: u32) -> Shift {
        match bits & 3 {
            0 => Shift::Lsl,
            1 => Shift::Lsr,
            2 => Shift::Asr,
            _ => Shift::Ror,
        }
    }
}

/// Returns (result, carry, signed overflow).
pub fn add_with_carry(a: u32, b: u32, carry: bool) -> (u32, bool, bool) {
    let sum = a as u64 + b as u64 + carry as u64;
    let result = sum as u32;
    let overflow = (!(a ^ b) & (a ^ result)) >> 31 != 0;
    (result, sum >> 32 != 0, overflow)
}

/// Barrel shifter. `immediate` selects the encoding where an amount of 0 has
/// special meaning (LSR/ASR #32, RRX); register-specified amounts of 0 leave
/// the value and carry alone.
pub fn shift(kind: Shift, value: u32, amount: u32, carry: bool, immediate: bool) -> (u32, bool) {
    match kind {
        Shift::Lsl => match amount {
            0 => (value, carry),
            1..=31 => (value << amount, value >> (32 - amount) & 1 != 0),
            32 => (0, value & 1 != 0),
            _ => (0, false),
        },
        Shift::Lsr => {
            let amount = if immediate && amount == 0 { 32 } else { amount };
            match amount {
                0 => (value, carry),
                1..=31 => (value >> amount, value >> (amount - 1) & 1 != 0),
                32 => (0, value >> 31 != 0),
                _ => (0, false),
            }
        }
        Shift::Asr => {
            let amount = if immediate && amount == 0 { 32 } else { amount };
            match amount {
                0 => (value, carry),
                1..=31 => (((value as i32) >> amount) as u32, value >> (amount - 1) & 1 != 0),
                _ => (((value as i32) >> 31) as u32, value >> 31 != 0),
            }
        }
        Shift::Ror => {
            if immediate && amount == 0 {
                return ((value >> 1) | ((carry as u32) << 31), value & 1 != 0);
            }
            if amount == 0 {
                return (value, carry);
            }
            let rot = amount & 31;
            if rot == 0 {
                (value, value >> 31 != 0)
            } else {
                let result = value.rotate_right(rot);
                (result, result >> 31 != 0)
            }
        }
    }
}
