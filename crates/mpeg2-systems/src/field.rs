//! Fields of a syntax element, read and written bit by bit
//!
//! The syntax tables of ISO/IEC 13818-1 lay their fields end to end without
//! regard for byte boundaries: a 13-bit PID follows three 1-bit flags, and a
//! 33-bit PTS is split by marker bits. [`FieldReader`] takes such fields off
//! the front of a borrowed input, and [`FieldWriter`] appends them to a
//! caller's buffer.

use core::fmt;

mod out_of_range;
mod reader;
#[cfg(feature = "alloc")]
mod writer;

pub use out_of_range::{FourBitFieldOutOfRange, TwoBitFieldOutOfRange};
pub use reader::FieldReader;
#[cfg(feature = "alloc")]
pub use writer::FieldWriter;
#[cfg(feature = "alloc")]
pub(crate) use writer::write_or_truncate;

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
