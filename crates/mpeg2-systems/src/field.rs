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
            Self::ValueTooWide { width, value } => {
                write!(
                    formatter,
                    "value {value} does not fit in a field of {width} bits"
                )
            }
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
    pub(crate) value: u8,
}

impl TwoBitFieldOutOfRange {
    /// Returns the value given, 4 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
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
    pub(crate) value: u8,
}

impl FourBitFieldOutOfRange {
    /// Returns the value given, 16 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }
}

impl fmt::Display for FourBitFieldOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "value {} does not fit in 4 bits", self.value)
    }
}

impl core::error::Error for FourBitFieldOutOfRange {}

/// Cursor reading the fields of a syntax element off the front of its bytes
///
/// `uimsbf` and `bslbf` fields, ISO/IEC 13818-1 §2.2.6
///
/// Each field is an unsigned integer of 1 to 64 bits, most significant bit
/// first (`uimsbf`, and `bslbf` read as an integer), at whatever bit the field
/// before it ended. The width is a constant of the syntax table, so it is a
/// const parameter, and a width outside 1 to 64 bits, or wider than the
/// integer it is read into, does not compile. A run of whole bytes, such as a
/// payload or a `private_data_byte` loop, is borrowed from the input once the
/// position is on a byte boundary.
///
/// A read that fails leaves the position where it was. Every read is bounded
/// by the input, so no length read from the input can take the cursor past its
/// end.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::{Error, FieldReader};
///
/// // The four header bytes of a null Transport Packet
/// let mut reader = FieldReader::new(&[0x47, 0x1f, 0xff, 0x10]);
/// assert_eq!(reader.read_u8::<8>(), Ok(0x47));
/// assert_eq!(reader.read_bool(), Ok(false));
/// assert_eq!(reader.read_bool(), Ok(false));
/// assert_eq!(reader.read_bool(), Ok(false));
/// assert_eq!(reader.read_u16::<13>(), Ok(0x1fff));
///
/// // A field longer than what is left names both
/// assert_eq!(
///     reader.read_bits::<9>(),
///     Err(Error::Truncated { required_bits: 9, remaining_bits: 8 })
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldReader<'input> {
    input: &'input [u8],
    position_bits: u64,
}

impl<'input> FieldReader<'input> {
    /// Creates a cursor at the first bit of `input`
    #[must_use]
    pub fn new(input: &'input [u8]) -> Self {
        Self {
            input,
            position_bits: 0,
        }
    }

    /// Returns the index of the byte the position is in
    fn byte_offset(&self) -> usize {
        // Why not an error: the position never passes the end of the input,
        // whose length is a usize, so the conversion cannot fail.
        usize::try_from(self.position_bits / 8).unwrap_or(usize::MAX)
    }

    /// Returns how many bits of the byte the position is in are already read
    fn bit_offset(&self) -> u32 {
        let [.., bit_offset] = (self.position_bits % 8).to_be_bytes();
        u32::from(bit_offset)
    }

    /// Returns how many bits of the input are left unread
    #[must_use]
    pub fn remaining_bits(&self) -> u64 {
        u64::try_from(self.input.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(8)
            .saturating_sub(self.position_bits)
    }

    /// Returns how many whole bytes of the input are left unread
    ///
    /// A byte partly read does not count.
    #[must_use]
    pub fn remaining_bytes(&self) -> usize {
        self.input
            .len()
            .saturating_sub(self.byte_offset())
            .saturating_sub(usize::from(!self.is_byte_aligned()))
    }

    /// Returns whether the position is on a byte boundary
    #[must_use]
    pub fn is_byte_aligned(&self) -> bool {
        self.position_bits.is_multiple_of(8)
    }

    /// Reads the next field as an unsigned integer of `WIDTH` bits
    ///
    /// `WIDTH` outside 1 to 64 does not compile.
    ///
    /// # Errors
    ///
    /// * [`Truncated`](Error::Truncated): the input ends inside the field.
    pub fn read_bits<const WIDTH: u32>(&mut self) -> Result<u64, Error> {
        const {
            assert!(
                WIDTH >= 1 && WIDTH <= u64::BITS,
                "a field is 1 to 64 bits wide"
            )
        };
        let remaining_bits = self.remaining_bits();
        if u64::from(WIDTH) > remaining_bits {
            return Err(Error::Truncated {
                required_bits: u64::from(WIDTH),
                remaining_bits,
            });
        }

        // Why not u64: a 64-bit field that starts inside a byte spans 9 bytes.
        // The bits up to `end_bit` are at most 7 + 64, so a u128 holds them.
        let end_bit = self.bit_offset().saturating_add(WIDTH);
        let mut window = 0_u128;
        let mut loaded_bits = 0_u32;
        for byte in self.input.iter().skip(self.byte_offset()) {
            if loaded_bits >= end_bit {
                break;
            }
            window = window.wrapping_shl(8) | u128::from(*byte);
            loaded_bits = loaded_bits.wrapping_add(8);
        }

        let [_, _, _, _, _, _, _, _, low @ ..] = window
            .wrapping_shr(loaded_bits.wrapping_sub(end_bit))
            .to_be_bytes();
        let value = u64::from_be_bytes(low) & u64::MAX.wrapping_shr(u64::BITS.wrapping_sub(WIDTH));
        self.position_bits = self.position_bits.saturating_add(u64::from(WIDTH));

        Ok(value)
    }

    /// Reads the next field as a `u8` of `WIDTH` bits
    ///
    /// `WIDTH` outside 1 to 8 does not compile.
    ///
    /// # Errors
    ///
    /// * [`Truncated`](Error::Truncated): the input ends inside the field.
    pub fn read_u8<const WIDTH: u32>(&mut self) -> Result<u8, Error> {
        const { assert!(WIDTH <= u8::BITS, "the field is wider than a u8") };
        // Why not try_from: WIDTH fits in the low byte, so its error is unreachable.
        let [.., low] = self.read_bits::<WIDTH>()?.to_be_bytes();

        Ok(low)
    }

    /// Reads the next field as a `u16` of `WIDTH` bits
    ///
    /// `WIDTH` outside 1 to 16 does not compile.
    ///
    /// # Errors
    ///
    /// * [`Truncated`](Error::Truncated): the input ends inside the field.
    pub fn read_u16<const WIDTH: u32>(&mut self) -> Result<u16, Error> {
        const { assert!(WIDTH <= u16::BITS, "the field is wider than a u16") };
        // Why not try_from: WIDTH fits in the low 2 bytes, so its error is unreachable.
        let [.., high, low] = self.read_bits::<WIDTH>()?.to_be_bytes();

        Ok(u16::from_be_bytes([high, low]))
    }

    /// Reads the next field as a `u32` of `WIDTH` bits
    ///
    /// `WIDTH` outside 1 to 32 does not compile.
    ///
    /// # Errors
    ///
    /// * [`Truncated`](Error::Truncated): the input ends inside the field.
    pub fn read_u32<const WIDTH: u32>(&mut self) -> Result<u32, Error> {
        const { assert!(WIDTH <= u32::BITS, "the field is wider than a u32") };
        // Why not try_from: WIDTH fits in the low 4 bytes, so its error is unreachable.
        let [_, _, _, _, low @ ..] = self.read_bits::<WIDTH>()?.to_be_bytes();

        Ok(u32::from_be_bytes(low))
    }

    /// Reads the next field as a single bit, set or clear
    ///
    /// # Errors
    ///
    /// * [`Truncated`](Error::Truncated): the input has no bit left.
    pub fn read_bool(&mut self) -> Result<bool, Error> {
        Ok(self.read_bits::<1>()? == 1)
    }

    /// Borrows the next field, a run of `length` whole bytes
    ///
    /// # Errors
    ///
    /// * [`NotByteAligned`](Error::NotByteAligned): the position is inside a
    ///   byte.
    /// * [`Truncated`](Error::Truncated): the input ends inside the field.
    pub fn read_bytes(&mut self, length: usize) -> Result<&'input [u8], Error> {
        if !self.is_byte_aligned() {
            return Err(Error::NotByteAligned {
                bit_offset: self.bit_offset(),
            });
        }

        let required_bits = u64::try_from(length).unwrap_or(u64::MAX).saturating_mul(8);
        let (field, _) = self
            .input
            .get(self.byte_offset()..)
            .and_then(|rest| rest.split_at_checked(length))
            .ok_or_else(|| Error::Truncated {
                required_bits,
                remaining_bits: self.remaining_bits(),
            })?;
        self.position_bits = self.position_bits.saturating_add(required_bits);

        Ok(field)
    }
}

/// Cursor appending the fields of a syntax element to a caller's buffer
///
/// `uimsbf` and `bslbf` fields, ISO/IEC 13818-1 §2.2.6
///
/// The mirror of [`FieldReader`]: each field is an unsigned integer of 1 to 64
/// bits, most significant bit first, with the width a const parameter, and a
/// run of whole bytes is appended once the position is on a byte boundary. A
/// byte reaches the buffer as soon as its last bit is written; the bits of a
/// byte not yet complete are held by the cursor.
///
/// A write that fails leaves the buffer and the position as they were. The
/// cursor is ended with [`finish`](Self::finish), which refuses a byte only
/// partly written; a cursor dropped without it drops those bits.
///
/// Available with the `alloc` feature.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::{Error, FieldWriter};
///
/// // The four header bytes of a null Transport Packet
/// let mut packet = Vec::new();
/// let mut writer = FieldWriter::new(&mut packet);
/// writer.write_bits::<8>(0x47_u8)?;
/// writer.write_bool(false)?;
/// writer.write_bool(false)?;
/// writer.write_bool(false)?;
/// writer.write_bits::<13>(0x1fff_u16)?;
/// writer.write_bits::<2>(0b00_u8)?;
/// writer.write_bits::<2>(0b01_u8)?;
/// writer.write_bits::<4>(0_u8)?;
/// writer.finish()?;
/// assert_eq!(packet, [0x47, 0x1f, 0xff, 0x10]);
///
/// // A value wider than its field is refused
/// let mut writer = FieldWriter::new(&mut packet);
/// assert_eq!(
///     writer.write_bits::<13>(0x2000_u16),
///     Err(Error::ValueTooWide { width: 13, value: 0x2000 })
/// );
/// # Ok::<(), Error>(())
/// ```
#[cfg(feature = "alloc")]
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct FieldWriter<'output> {
    output: &'output mut Vec<u8>,
    pending: u8,
    pending_bits: u32,
}

#[cfg(feature = "alloc")]
impl<'output> FieldWriter<'output> {
    /// Creates a cursor appending to `output` after the bytes it already holds
    #[must_use]
    pub fn new(output: &'output mut Vec<u8>) -> Self {
        Self {
            output,
            pending: 0,
            pending_bits: 0,
        }
    }

    /// Returns whether the position is on a byte boundary
    #[must_use]
    pub fn is_byte_aligned(&self) -> bool {
        self.pending_bits == 0
    }

    /// Writes the next field as an unsigned integer of `WIDTH` bits
    ///
    /// `WIDTH` outside 1 to 64 does not compile.
    ///
    /// # Errors
    ///
    /// * [`ValueTooWide`](Error::ValueTooWide): `value` has a bit set above
    ///   `WIDTH`.
    pub fn write_bits<const WIDTH: u32>(&mut self, value: impl Into<u64>) -> Result<(), Error> {
        const {
            assert!(
                WIDTH >= 1 && WIDTH <= u64::BITS,
                "a field is 1 to 64 bits wide"
            )
        };
        let value = value.into();
        if value.checked_shr(WIDTH).unwrap_or(0) != 0 {
            return Err(Error::ValueTooWide {
                width: WIDTH,
                value,
            });
        }

        // Why not u64: the bits held back from the last byte, at most 7, sit
        // above a field of up to 64 bits.
        let window = u128::from(self.pending).wrapping_shl(WIDTH) | u128::from(value);
        let mut unwritten_bits = self.pending_bits.saturating_add(WIDTH);
        while unwritten_bits >= 8 {
            unwritten_bits = unwritten_bits.wrapping_sub(8);
            let [.., byte] = window.wrapping_shr(unwritten_bits).to_be_bytes();
            self.output.push(byte);
        }

        let [.., low] = window.to_be_bytes();
        self.pending = low
            & u8::MAX
                .checked_shr(8_u32.wrapping_sub(unwritten_bits))
                .unwrap_or(0);
        self.pending_bits = unwritten_bits;

        Ok(())
    }

    /// Writes the next field as a single bit, set or clear
    ///
    /// # Errors
    ///
    /// Never fails; the `Result` keeps it in step with
    /// [`write_bits`](Self::write_bits) under `?`.
    pub fn write_bool(&mut self, value: bool) -> Result<(), Error> {
        self.write_bits::<1>(value)
    }

    /// Writes the next field, a run of whole bytes
    ///
    /// # Errors
    ///
    /// * [`NotByteAligned`](Error::NotByteAligned): the position is inside a
    ///   byte.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.pending_bits != 0 {
            return Err(Error::NotByteAligned {
                bit_offset: self.pending_bits,
            });
        }

        self.output.extend_from_slice(bytes);

        Ok(())
    }

    /// Ends the writing, which must stop on a byte boundary
    ///
    /// # Errors
    ///
    /// * [`IncompleteByte`](Error::IncompleteByte): the last byte is only
    ///   partly written; its bits are not appended.
    pub fn finish(self) -> Result<(), Error> {
        if self.pending_bits != 0 {
            return Err(Error::IncompleteByte {
                pending_bits: self.pending_bits,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use super::FieldWriter;
    use super::{Error, FieldReader};
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    #[test]
    fn fields_straddling_byte_boundaries_read_most_significant_bit_first() {
        let mut reader = FieldReader::new(&[0x47, 0x41, 0x00, 0x39]);

        let fields = [
            reader.read_bits::<8>(),
            reader.read_bits::<1>(),
            reader.read_bits::<1>(),
            reader.read_bits::<1>(),
            reader.read_bits::<13>(),
            reader.read_bits::<2>(),
            reader.read_bits::<2>(),
            reader.read_bits::<4>(),
        ];

        assert_eq!(
            fields,
            [
                Ok(0x47),
                Ok(0),
                Ok(1),
                Ok(0),
                Ok(0x0100),
                Ok(0),
                Ok(0b11),
                Ok(9)
            ]
        );
        assert_eq!(reader.remaining_bits(), 0);
    }

    #[test]
    fn a_field_reads_into_the_integer_type_its_width_fits() {
        let mut reader = FieldReader::new(&[0x47, 0x41, 0x00, 0x12, 0x34, 0x56, 0x78]);

        assert_eq!(
            (
                reader.read_u8::<8>(),
                reader.read_u8::<3>(),
                reader.read_u16::<13>(),
                reader.read_u32::<32>()
            ),
            (Ok(0x47), Ok(0b010), Ok(0x0100), Ok(0x1234_5678))
        );
    }

    #[test]
    fn a_64_bit_field_starting_inside_a_byte_reads_whole() {
        let mut reader =
            FieldReader::new(&[0b1011_1111, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xe5]);

        assert_eq!(reader.read_bits::<3>(), Ok(0b101));
        assert_eq!(reader.read_bits::<64>(), Ok(u64::MAX));
        assert_eq!(reader.read_bits::<5>(), Ok(0b0_0101));
    }

    #[test]
    fn a_field_running_past_the_input_names_its_width_and_what_is_left() {
        let mut reader = FieldReader::new(&[0xff, 0xff]);
        reader.read_bits::<3>().unwrap();
        let before = reader.clone();

        assert_eq!(
            reader.read_bits::<14>(),
            Err(Error::Truncated {
                required_bits: 14,
                remaining_bits: 13
            })
        );
        assert_eq!(reader, before);
    }

    #[test]
    fn bytes_are_borrowed_only_from_a_byte_boundary() {
        let input = [0xa5, 0x01, 0x02, 0x03];
        let mut reader = FieldReader::new(&input);

        assert_eq!(reader.read_bits::<4>(), Ok(0xa));
        assert_eq!(
            reader.read_bytes(1),
            Err(Error::NotByteAligned { bit_offset: 4 })
        );
        assert_eq!(reader.read_bits::<4>(), Ok(0x5));
        assert_eq!(reader.read_bytes(2), Ok(&input[1..3]));
        assert_eq!(
            reader.read_bytes(2),
            Err(Error::Truncated {
                required_bits: 16,
                remaining_bits: 8
            })
        );
    }

    #[test]
    fn a_partly_read_byte_counts_toward_the_bits_left_but_not_the_bytes() {
        let mut reader = FieldReader::new(&[0, 0, 0]);
        reader.read_bits::<3>().unwrap();

        assert_eq!(
            (
                reader.remaining_bits(),
                reader.remaining_bytes(),
                reader.is_byte_aligned()
            ),
            (21, 2, false)
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn fields_straddling_byte_boundaries_write_most_significant_bit_first() {
        let mut output = Vec::new();
        let mut writer = FieldWriter::new(&mut output);

        writer.write_bits::<8>(0x47_u8).unwrap();
        writer.write_bool(false).unwrap();
        writer.write_bool(true).unwrap();
        writer.write_bool(false).unwrap();
        writer.write_bits::<13>(0x0100_u16).unwrap();
        writer.write_bits::<2>(0_u8).unwrap();
        writer.write_bits::<2>(0b11_u8).unwrap();
        writer.write_bits::<4>(9_u8).unwrap();
        writer.write_bytes(&[0xb7]).unwrap();

        assert_eq!(writer.finish(), Ok(()));
        assert_eq!(output, [0x47, 0x41, 0x00, 0x39, 0xb7]);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn fields_written_at_any_bit_position_read_back_as_written() {
        let mut output = Vec::new();
        let mut writer = FieldWriter::new(&mut output);
        writer.write_bits::<3>(0b101_u8).unwrap();
        writer.write_bits::<64>(0x8123_4567_89ab_cdef_u64).unwrap();
        writer.write_bits::<33>(0x1_2345_6789_u64).unwrap();
        writer.write_bool(true).unwrap();
        writer.write_bits::<3>(0b011_u8).unwrap();
        writer.finish().unwrap();

        let mut reader = FieldReader::new(&output);
        let read_back = [
            reader.read_bits::<3>(),
            reader.read_bits::<64>(),
            reader.read_bits::<33>(),
            reader.read_bits::<1>(),
            reader.read_bits::<3>(),
        ];

        assert_eq!(
            read_back,
            [
                Ok(0b101),
                Ok(0x8123_4567_89ab_cdef),
                Ok(0x1_2345_6789),
                Ok(1),
                Ok(0b011)
            ]
        );
        assert_eq!(reader.remaining_bits(), 0);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_value_wider_than_its_field_is_refused_before_a_bit_is_written() {
        let mut output = Vec::new();
        let mut writer = FieldWriter::new(&mut output);
        writer.write_bits::<3>(0b111_u8).unwrap();

        assert_eq!(
            writer.write_bits::<13>(0x2000_u16),
            Err(Error::ValueTooWide {
                width: 13,
                value: 0x2000
            })
        );
        writer.write_bits::<5>(0_u8).unwrap();
        writer.finish().unwrap();
        assert_eq!(output, [0b1110_0000]);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn writing_cannot_finish_or_append_bytes_inside_a_byte() {
        let mut output = Vec::new();
        let mut writer = FieldWriter::new(&mut output);
        writer.write_bits::<8>(0x47_u8).unwrap();
        writer.write_bits::<3>(0_u8).unwrap();

        assert_eq!(
            writer.write_bytes(&[0]),
            Err(Error::NotByteAligned { bit_offset: 3 })
        );
        assert_eq!(
            writer.finish(),
            Err(Error::IncompleteByte { pending_bits: 3 })
        );
        assert_eq!(output, [0x47]);
    }
}
