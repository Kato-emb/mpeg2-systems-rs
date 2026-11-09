//! ARIB STD-B24 captions: the syntax of caption and superimpose data

use core::fmt;

use mpeg2_systems::field::{self, FourBitFieldOutOfRange};

pub mod data_group;

/// Failure to read or write caption data
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// A field runs past the end of the bytes
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field failed
        error: field::Error,
    },
    /// `data_group_size` does not match the bytes between it and `CRC_16`
    DataGroupSize {
        /// The `data_group_size` read
        data_group_size: u16,
        /// The bytes given between `data_group_size` and `CRC_16`
        available_bytes: usize,
    },
    /// The data group to write has more `data_group_data_byte` than the
    /// 65535 `data_group_size` counts
    DataGroupDataLength {
        /// The bytes of data given
        length: usize,
    },
    /// `CRC_16` does not match the bytes it covers
    Crc16 {
        /// The `CRC_16` read
        crc_16: u16,
        /// The CRC computed over the data group
        computed: u16,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read")
            }
            Self::DataGroupSize {
                data_group_size,
                available_bytes,
            } => write!(
                formatter,
                "data_group_size {data_group_size} does not match the {available_bytes} bytes \
                 given"
            ),
            Self::DataGroupDataLength { length } => write!(
                formatter,
                "data_group_data_byte of {length} bytes is longer than data_group_size counts"
            ),
            Self::Crc16 { crc_16, computed } => write!(
                formatter,
                "CRC_16 0x{crc_16:04x} does not match the 0x{computed:04x} computed"
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::DataGroupSize { .. } | Self::DataGroupDataLength { .. } | Self::Crc16 { .. } => {
                None
            }
        }
    }
}

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

/// Returns the `CRC_16` of `bytes`, ARIB STD-B24 Volume 1 Part 3 §9.2
///
/// The division by G(X) = X^16 + X^12 + X^5 + 1 that §9.2 describes, from 0
/// and with nothing inverted: CRC-16/XMODEM of the CRC catalog.
fn crc_16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0, |crc, &byte| {
        (0..8).fold(crc ^ u16::from(byte) << 8, |crc, _| {
            if crc & 0x8000 == 0 {
                crc << 1
            } else {
                crc << 1 ^ 0x1021
            }
        })
    })
}

/// Display mode, whether a caption is shown on its own when received and
/// when played back from a recording
///
/// `DMF`, ARIB STD-B24 Volume 1 Part 3 Table 9-5 and §9.6.1
///
/// The 4 bits are two halves: b4b3 is the mode on reception and b2b1 the
/// mode on playback of a recording. In each, `'00'` shows the caption on
/// its own, `'01'` does not, and `'10'` lets the viewer choose; `'11'`
/// shows or hides it under a condition on reception and is reserved on
/// playback. In a data component descriptor, `'1111'` says that the DMF of
/// the caption management data changes, and `'0011'` that the elementary
/// stream has a language whose captions are shown on their own.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::FourBitFieldOutOfRange;
/// use mpeg2_systems_arib::caption::Dmf;
///
/// assert_eq!(Dmf::try_from(0b1010).map(Dmf::get), Ok(0b1010));
/// // A DMF is 4 bits
/// assert_eq!(Dmf::try_from(16).map_err(FourBitFieldOutOfRange::value), Err(16));
/// ```
#[doc(alias("DMF", "display_mode"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Dmf(pub(crate) u8);

impl Dmf {
    /// Returns the 4 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Dmf {
    type Error = FourBitFieldOutOfRange;

    /// Takes the 4 bits of `value`
    ///
    /// # Errors
    ///
    /// [`FourBitFieldOutOfRange`]: `value` is 16 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        FourBitFieldOutOfRange::check(value).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::{SixBitFieldOutOfRange, crc_16};

    #[test]
    fn crc_16_is_the_crc_16_xmodem_of_the_crc_catalog() {
        assert_eq!(crc_16(b"123456789"), 0x31c3);
    }

    #[test]
    fn six_bits_carry_values_below_64() {
        assert_eq!(
            [63, 64].map(SixBitFieldOutOfRange::check),
            [Ok(63), Err(SixBitFieldOutOfRange { value: 64 })]
        );
    }
}
