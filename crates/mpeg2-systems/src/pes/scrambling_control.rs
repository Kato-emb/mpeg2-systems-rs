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
pub struct PesScramblingControl(pub(super) u8);

impl PesScramblingControl {
    /// `'00'`, the payload is not scrambled
    pub const NOT_SCRAMBLED: Self = Self(0b00);

    /// Returns the 2-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
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
