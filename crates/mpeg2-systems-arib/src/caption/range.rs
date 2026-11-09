//! The errors of value types of widths `mpeg2-systems` has no shared error
//! for

use core::fmt;

/// Failure to make the value of a 6-bit field of a value 6 bits cannot carry
///
/// The value types of 6-bit fields, such as `data_group_id`, share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SixBitFieldOutOfRange {
    value: u8,
}

impl SixBitFieldOutOfRange {
    /// Returns the value given, 64 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }

    /// Returns `value` if 6 bits carry it
    ///
    /// Use it in the `TryFrom<u8>` of the value type of a 6-bit field, in
    /// this crate or another, so the conversion returns this error.
    ///
    /// # Errors
    ///
    /// [`SixBitFieldOutOfRange`]: `value` is 64 or more.
    ///
    /// # Examples
    ///
    /// ```
    /// use mpeg2_systems_arib::caption::SixBitFieldOutOfRange;
    ///
    /// assert_eq!(SixBitFieldOutOfRange::check(63), Ok(63));
    /// // 6 bits cannot carry 64
    /// let error = SixBitFieldOutOfRange::check(64).unwrap_err();
    /// assert_eq!(error.value(), 64);
    /// ```
    pub const fn check(value: u8) -> Result<u8, Self> {
        if value < 64 {
            Ok(value)
        } else {
            Err(Self { value })
        }
    }
}

impl fmt::Display for SixBitFieldOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "value {} does not fit in 6 bits", self.value)
    }
}

impl core::error::Error for SixBitFieldOutOfRange {}

/// Failure to make the value of a 3-bit field of a value 3 bits cannot carry
///
/// The value types of 3-bit fields, such as `language_tag`, share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThreeBitFieldOutOfRange {
    value: u8,
}

impl ThreeBitFieldOutOfRange {
    /// Returns the value given, 8 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }

    /// Returns `value` if 3 bits carry it
    ///
    /// Use it in the `TryFrom<u8>` of the value type of a 3-bit field, in
    /// this crate or another, so the conversion returns this error.
    ///
    /// # Errors
    ///
    /// [`ThreeBitFieldOutOfRange`]: `value` is 8 or more.
    ///
    /// # Examples
    ///
    /// ```
    /// use mpeg2_systems_arib::caption::ThreeBitFieldOutOfRange;
    ///
    /// assert_eq!(ThreeBitFieldOutOfRange::check(7), Ok(7));
    /// // 3 bits cannot carry 8
    /// let error = ThreeBitFieldOutOfRange::check(8).unwrap_err();
    /// assert_eq!(error.value(), 8);
    /// ```
    pub const fn check(value: u8) -> Result<u8, Self> {
        if value < 8 {
            Ok(value)
        } else {
            Err(Self { value })
        }
    }
}

impl fmt::Display for ThreeBitFieldOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "value {} does not fit in 3 bits", self.value)
    }
}

impl core::error::Error for ThreeBitFieldOutOfRange {}

#[cfg(test)]
mod tests {
    use super::{SixBitFieldOutOfRange, ThreeBitFieldOutOfRange};

    #[test]
    fn six_bits_carry_values_below_64() {
        assert_eq!(
            [63, 64].map(SixBitFieldOutOfRange::check),
            [Ok(63), Err(SixBitFieldOutOfRange { value: 64 })]
        );
    }

    #[test]
    fn three_bits_carry_values_below_8() {
        assert_eq!(
            [7, 8].map(ThreeBitFieldOutOfRange::check),
            [Ok(7), Err(ThreeBitFieldOutOfRange { value: 8 })]
        );
    }
}
