//! Continuity counter, which numbers the packets of one PID

use core::fmt;

/// Count of the packets of one PID that carry a payload, modulo 16
///
/// `continuity_counter`, ISO/IEC 13818-1 §2.4.3.3
///
/// The counter of a packet with no payload (`adaptation_field_control` `'10'`)
/// does not advance. Two consecutive packets of a PID with the same counter
/// are a duplicate, sent at most twice, not a discontinuity. The crate reads
/// and writes the counter; judging the continuity of a PID is the
/// demultiplexer's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContinuityCounter(pub(super) u8);

impl ContinuityCounter {
    /// Number of distinct counter values, 16
    pub const MODULUS: u8 = 16;

    /// Returns the 4-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns the counter of the next packet of the PID, 15 wrapping to 0
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1).rem_euclid(Self::MODULUS))
    }

    /// Returns the counter of the packet of the PID before, 0 wrapping to 15
    #[cfg(feature = "alloc")]
    pub(super) const fn previous(self) -> Self {
        Self(
            self.0
                .wrapping_add(Self::MODULUS.wrapping_sub(1))
                .rem_euclid(Self::MODULUS),
        )
    }
}

impl TryFrom<u8> for ContinuityCounter {
    type Error = ContinuityCounterOutOfRange;

    /// Takes a 4-bit value
    ///
    /// # Errors
    ///
    /// [`ContinuityCounterOutOfRange`]: `value` is 16 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < Self::MODULUS {
            Ok(Self(value))
        } else {
            Err(ContinuityCounterOutOfRange { value })
        }
    }
}

/// Failure to make a [`ContinuityCounter`] of a value 4 bits cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContinuityCounterOutOfRange {
    value: u8,
}

impl ContinuityCounterOutOfRange {
    /// Returns the value given, 16 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }
}

impl fmt::Display for ContinuityCounterOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "continuity_counter {} does not fit in 4 bits",
            self.value
        )
    }
}

impl core::error::Error for ContinuityCounterOutOfRange {}

#[cfg(test)]
mod tests {
    use super::{ContinuityCounter, ContinuityCounterOutOfRange};

    #[test]
    fn a_continuity_counter_takes_any_4_bit_value_and_wraps_from_15_to_0() {
        let counter = |value| ContinuityCounter::try_from(value).unwrap();

        assert_eq!(
            (
                ContinuityCounter::try_from(16),
                counter(3).next(),
                counter(15).next()
            ),
            (
                Err(ContinuityCounterOutOfRange { value: 16 }),
                counter(4),
                counter(0)
            )
        );
    }
}
