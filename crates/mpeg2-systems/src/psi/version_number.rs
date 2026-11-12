//! The version of a table

use core::fmt;

/// Version of a table, which changes whenever its content does
///
/// `version_number`, ISO/IEC 13818-1 §2.4.4.6, §2.4.4.12
///
/// The version is incremented by 1 modulo 32 at each change. The crate reads
/// and writes it; telling a new version from a repeated one is the
/// demultiplexer's.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::psi::{VersionNumber, VersionNumberOutOfRange};
///
/// // The last version wraps to the first
/// let last = VersionNumber::try_from(31)?;
/// assert_eq!(last.next().get(), 0);
///
/// // A value of more than 5 bits is refused
/// assert_eq!(
///     VersionNumber::try_from(32).map_err(VersionNumberOutOfRange::value),
///     Err(32)
/// );
/// # Ok::<(), VersionNumberOutOfRange>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VersionNumber(pub(crate) u8);

impl VersionNumber {
    /// Number of distinct versions, 32
    pub const MODULUS: u8 = 32;

    /// Returns the 5-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns the version that follows, 31 wrapping to 0
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1).rem_euclid(Self::MODULUS))
    }
}

impl TryFrom<u8> for VersionNumber {
    type Error = VersionNumberOutOfRange;

    /// Takes a 5-bit value
    ///
    /// # Errors
    ///
    /// [`VersionNumberOutOfRange`]: `value` is 32 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < Self::MODULUS {
            Ok(Self(value))
        } else {
            Err(VersionNumberOutOfRange { value })
        }
    }
}

/// Failure to make a [`VersionNumber`] of a value 5 bits cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VersionNumberOutOfRange {
    value: u8,
}

impl VersionNumberOutOfRange {
    /// Returns the value given, 32 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }
}

impl fmt::Display for VersionNumberOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "version_number {} does not fit in 5 bits",
            self.value
        )
    }
}

impl core::error::Error for VersionNumberOutOfRange {}

#[cfg(test)]
mod tests {
    use super::{VersionNumber, VersionNumberOutOfRange};

    #[test]
    fn a_version_number_takes_any_5_bit_value_and_wraps_from_31_to_0() {
        let version = |value| VersionNumber::try_from(value).unwrap();

        assert_eq!(
            (
                VersionNumber::try_from(32),
                version(4).next(),
                version(31).next()
            ),
            (
                Err(VersionNumberOutOfRange { value: 32 }),
                version(5),
                version(0)
            )
        );
    }
}
