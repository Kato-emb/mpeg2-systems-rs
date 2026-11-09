//! ARIB STD-B24 captions: the syntax of caption and superimpose data

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use mpeg2_systems::field::{self, FourBitFieldOutOfRange};

use self::data_group::DataGroupId;

pub mod data_group;
pub mod data_unit;
pub mod management;
mod range;
pub mod statement;
mod time;

pub use range::{SixBitFieldOutOfRange, ThreeBitFieldOutOfRange};
pub use time::{BcdTime, BcdTimeOutOfRange, Tmd};

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
    /// The caption data is read from a data group of another kind
    DataGroupId {
        /// The `data_group_id` of the data group
        found: DataGroupId,
    },
    /// `unit_separator` is not 0x1F
    UnitSeparator {
        /// The `unit_separator` read
        found: u8,
    },
    /// `data_unit_size` runs past the end of the data unit loop
    DataUnitSize {
        /// The `data_unit_size` read
        data_unit_size: u32,
        /// The bytes left in the loop after `data_unit_size`
        available_bytes: usize,
    },
    /// The data unit to write has more `data_unit_data_byte` than the
    /// 0xFFFFFF `data_unit_size` counts
    DataUnitDataLength {
        /// The bytes of data given
        length: usize,
    },
    /// `data_unit_loop_length` does not match the bytes after it, or the
    /// loop to write is longer than the 0xFFFFFF bytes it counts
    DataUnitLoopLength {
        /// The `data_unit_loop_length` read, or the bytes of the loop to
        /// write
        data_unit_loop_length: usize,
        /// The bytes after `data_unit_loop_length`, or 0xFFFFFF when writing
        available_bytes: usize,
    },
    /// The caption management data to write has more languages than the 8
    /// §9.3 allows
    NumLanguages {
        /// The languages given
        count: usize,
    },
    /// A field to write has a value its table reserves or does not define
    Forbidden {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The value given
        value: u64,
    },
    /// A field to write is present where its condition leaves it out, or
    /// absent where its condition asks for it
    Presence {
        /// The name of the field in the syntax table
        field: &'static str,
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
            Self::DataGroupId { found } => write!(
                formatter,
                "data_group_id 0x{:02x} does not carry this caption data",
                found.get()
            ),
            Self::UnitSeparator { found } => {
                write!(formatter, "unit_separator 0x{found:02x} is not 0x1f")
            }
            Self::DataUnitSize {
                data_unit_size,
                available_bytes,
            } => write!(
                formatter,
                "data_unit_size {data_unit_size} runs past the {available_bytes} bytes left"
            ),
            Self::DataUnitDataLength { length } => write!(
                formatter,
                "data_unit_data_byte of {length} bytes is longer than data_unit_size counts"
            ),
            Self::DataUnitLoopLength {
                data_unit_loop_length,
                available_bytes,
            } => write!(
                formatter,
                "data_unit_loop_length {data_unit_loop_length} does not match the \
                 {available_bytes} bytes given"
            ),
            Self::NumLanguages { count } => write!(
                formatter,
                "{count} languages are more than the 8 num_languages allows"
            ),
            Self::Forbidden { field, value } => {
                write!(formatter, "{field} 0x{value:x} is reserved or not defined")
            }
            Self::Presence { field } => write!(
                formatter,
                "{field} is present or absent against its condition"
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::DataGroupSize { .. }
            | Self::DataGroupDataLength { .. }
            | Self::Crc16 { .. }
            | Self::DataGroupId { .. }
            | Self::UnitSeparator { .. }
            | Self::DataUnitSize { .. }
            | Self::DataUnitDataLength { .. }
            | Self::DataUnitLoopLength { .. }
            | Self::NumLanguages { .. }
            | Self::Forbidden { .. }
            | Self::Presence { .. } => None,
        }
    }
}

/// Runs `write` on `out`, cutting off what it appended if `write` fails
#[cfg(feature = "alloc")]
fn write_or_truncate<E>(
    out: &mut Vec<u8>,
    write: impl FnOnce(&mut Vec<u8>) -> Result<(), E>,
) -> Result<(), E> {
    let start = out.len();
    let written = write(out);
    if written.is_err() {
        out.truncate(start);
    }

    written
}

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
    use super::crc_16;

    #[test]
    fn crc_16_is_the_crc_16_xmodem_of_the_crc_catalog() {
        assert_eq!(crc_16(b"123456789"), 0x31c3);
    }
}
