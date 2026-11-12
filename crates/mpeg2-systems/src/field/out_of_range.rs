//! The errors of the value types of fields narrower than a byte

use core::fmt;

/// Failure to make the value of a 2-bit field of a value 2 bits cannot carry
///
/// The value types of 2-bit fields, such as `transport_scrambling_control`
/// and `PES_scrambling_control`, share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TwoBitFieldOutOfRange {
    value: u8,
}

impl TwoBitFieldOutOfRange {
    /// Returns the value given, 4 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }

    /// Returns `value` if 2 bits carry it
    ///
    /// Use it in the `TryFrom<u8>` of the value type of a 2-bit field, in
    /// this crate or another, so the conversion returns this error.
    ///
    /// # Errors
    ///
    /// [`TwoBitFieldOutOfRange`]: `value` is 4 or more.
    ///
    /// # Examples
    ///
    /// ```
    /// use mpeg2_systems::field::TwoBitFieldOutOfRange;
    ///
    /// assert_eq!(TwoBitFieldOutOfRange::check(3), Ok(3));
    /// // 2 bits cannot carry 4
    /// let error = TwoBitFieldOutOfRange::check(4).unwrap_err();
    /// assert_eq!(error.value(), 4);
    /// ```
    pub const fn check(value: u8) -> Result<u8, Self> {
        if value < 4 {
            Ok(value)
        } else {
            Err(Self { value })
        }
    }
}

impl fmt::Display for TwoBitFieldOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "value {} does not fit in 2 bits", self.value)
    }
}

impl core::error::Error for TwoBitFieldOutOfRange {}

/// Failure to make the value of a 4-bit field of a value 4 bits cannot carry
///
/// The value types of 4-bit fields, such as `frame_rate_code` and
/// `hierarchy_type`, share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourBitFieldOutOfRange {
    value: u8,
}

impl FourBitFieldOutOfRange {
    /// Returns the value given, 16 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }

    /// Returns `value` if 4 bits carry it
    ///
    /// Use it in the `TryFrom<u8>` of the value type of a 4-bit field, in
    /// this crate or another, so the conversion returns this error.
    ///
    /// # Errors
    ///
    /// [`FourBitFieldOutOfRange`]: `value` is 16 or more.
    ///
    /// # Examples
    ///
    /// ```
    /// use mpeg2_systems::field::FourBitFieldOutOfRange;
    ///
    /// assert_eq!(FourBitFieldOutOfRange::check(15), Ok(15));
    /// // 4 bits cannot carry 16
    /// let error = FourBitFieldOutOfRange::check(16).unwrap_err();
    /// assert_eq!(error.value(), 16);
    /// ```
    pub const fn check(value: u8) -> Result<u8, Self> {
        if value < 16 {
            Ok(value)
        } else {
            Err(Self { value })
        }
    }
}

impl fmt::Display for FourBitFieldOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "value {} does not fit in 4 bits", self.value)
    }
}

impl core::error::Error for FourBitFieldOutOfRange {}
