//! Cursor appending fields to a caller's buffer

use alloc::vec::Vec;

use super::Error;

/// Cursor appending the fields of a syntax element to a caller's buffer
///
/// `uimsbf` and `bslbf` fields, ISO/IEC 13818-1 §2.2.6
///
/// The mirror of [`FieldReader`](super::FieldReader): each field is an unsigned integer of 1 to 64
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
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct FieldWriter<'output> {
    output: &'output mut Vec<u8>,
    pending: u8,
    pending_bits: u32,
}

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

    /// Writes `value` as `HIGH` bits, a `marker_bit`, 15 bits, a
    /// `marker_bit`, 15 bits and a `marker_bit`, the markers `'1'`
    ///
    /// A `value` with a bit set above `HIGH` + 30 is a
    /// [`ValueTooWide`](Error::ValueTooWide) of that width.
    pub(crate) fn write_marked<const HIGH: u32>(&mut self, value: u64) -> Result<(), Error> {
        let width = HIGH.saturating_add(30);
        if value.checked_shr(width).unwrap_or(0) != 0 {
            return Err(Error::ValueTooWide { width, value });
        }
        self.write_bits::<HIGH>(value.wrapping_shr(30))?;
        self.write_bool(true)?;
        self.write_bits::<15>(value.wrapping_shr(15) & 0x7fff)?;
        self.write_bool(true)?;
        self.write_bits::<15>(value & 0x7fff)?;
        self.write_bool(true)
    }

    /// Writes the next field, a run of whole bytes
    ///
    /// # Errors
    ///
    /// * [`NotByteAligned`](Error::NotByteAligned): the position is inside a
    ///   byte.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if !self.is_byte_aligned() {
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
        if !self.is_byte_aligned() {
            return Err(Error::IncompleteByte {
                pending_bits: self.pending_bits,
            });
        }

        Ok(())
    }
}

/// Runs `write` on `out`, leaving `out` as it was if `write` fails
pub(crate) fn write_or_truncate<E>(
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

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{Error, FieldWriter};
    use crate::field::FieldReader;

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
