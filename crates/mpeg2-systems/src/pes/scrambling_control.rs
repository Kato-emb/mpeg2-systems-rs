//! PES scrambling control, which says how the payload of a PES packet is
//! scrambled

use crate::field::TwoBitFieldOutOfRange;

/// How the payload of a PES packet is scrambled
///
/// `PES_scrambling_control`, ISO/IEC 13818-1 §2.4.3.7, Table 2-23
///
/// Only `'00'` has a meaning the specification fixes; the other values are
/// user-defined. The PES header is never scrambled. The crate does not
/// descramble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PesScramblingControl(u8);

impl PesScramblingControl {
    /// `'00'`, the payload is not scrambled
    pub const NOT_SCRAMBLED: Self = Self(0b00);

    /// Returns the 2-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Takes the 2 bits of a `PES_scrambling_control` field, keeping only
    /// those bits
    pub(super) const fn from_bits_truncate(value: u8) -> Self {
        Self(value & 0b11)
    }
}

impl TryFrom<u8> for PesScramblingControl {
    type Error = TwoBitFieldOutOfRange;

    /// Takes a 2-bit value
    ///
    /// # Errors
    ///
    /// [`TwoBitFieldOutOfRange`]: `value` is 4 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        TwoBitFieldOutOfRange::check(value).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::PesScramblingControl;

    #[test]
    fn a_pes_scrambling_control_keeps_only_the_low_2_bits_of_a_wider_value() {
        assert_eq!(
            [
                PesScramblingControl::from_bits_truncate(0b11),
                PesScramblingControl::from_bits_truncate(0b1110)
            ],
            [PesScramblingControl(0b11), PesScramblingControl(0b10)]
        );
    }
}
