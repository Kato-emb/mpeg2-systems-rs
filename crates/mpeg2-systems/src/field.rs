//! Fields of a syntax element, read and written bit by bit
//!
//! The syntax tables of ISO/IEC 13818-1 lay their fields end to end without
//! regard for byte boundaries: a 13-bit PID follows three 1-bit flags, and a
//! 33-bit PTS is split by marker bits. [`FieldReader`] takes such fields off
//! the front of a borrowed input, and [`FieldWriter`] appends them to a
//! caller's buffer.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

mod reader;
#[cfg(feature = "alloc")]
mod writer;

pub use reader::FieldReader;
#[cfg(feature = "alloc")]
pub use writer::FieldWriter;

/// Failure to read or write a field
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The input ends inside the field
    Truncated {
        /// Bits the field occupies
        required_bits: u64,
        /// Bits of the input left unread before the field
        remaining_bits: u64,
    },
    /// A field of whole bytes was asked for while the position is inside a byte
    NotByteAligned {
        /// Bits of the current byte already read or written, 1 to 7
        bit_offset: u32,
    },
    /// The value has bits set above the width of its field
    ValueTooWide {
        /// The width of the field, in bits
        width: u32,
        /// The value given
        value: u64,
    },
    /// Writing was finished with a byte only partly written
    IncompleteByte {
        /// Bits of the last byte written, 1 to 7
        pending_bits: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated {
                required_bits,
                remaining_bits,
            } => write!(
                formatter,
                "field of {required_bits} bits runs past the end of the input, \
                 which has {remaining_bits} bits left"
            ),
            Self::NotByteAligned { bit_offset } => write!(
                formatter,
                "field of whole bytes starts at bit {bit_offset} of a byte"
            ),
            Self::ValueTooWide { width, value } => write!(
                formatter,
                "value {value} does not fit in a field of {width} bits"
            ),
            Self::IncompleteByte { pending_bits } => write!(
                formatter,
                "writing finished {pending_bits} bits into a byte"
            ),
        }
    }
}

impl core::error::Error for Error {}

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

/// Runs `write` on `out`, cutting off what it appended if `write` fails
///
/// What `write` appends after the bytes `out` already holds is cut off
/// again when it fails; bytes it changes or removes among those already
/// held are not restored. The syntax types of this crate write their fields
/// and loops through it, and a crate of its own syntax types can too.
///
/// # Errors
///
/// `write` fails: its error is returned unchanged.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::write_or_truncate;
///
/// let mut out = vec![0x47];
/// // A write that appends two bytes, then fails
/// let failed = write_or_truncate(&mut out, |out| {
///     out.extend_from_slice(&[0x01, 0x02]);
///     Err("the third field does not fit")
/// });
/// assert_eq!(failed, Err("the third field does not fit"));
/// assert_eq!(out, [0x47]);
///
/// // A write that succeeds keeps what it appended
/// write_or_truncate(&mut out, |out| {
///     out.push(0x01);
///     Ok::<(), &str>(())
/// })?;
/// assert_eq!(out, [0x47, 0x01]);
/// # Ok::<(), &str>(())
/// ```
#[cfg(feature = "alloc")]
pub fn write_or_truncate<E>(
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
