//! Cursor reading fields off the front of a syntax element's bytes

use super::Error;

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

    /// Reads the next `HIGH` + 30 bits, laid out as `HIGH` bits, a
    /// `marker_bit`, 15 bits, a `marker_bit`, 15 bits and a `marker_bit`,
    /// ignoring the markers
    pub(crate) fn read_marked<const HIGH: u32>(&mut self) -> Result<u64, Error> {
        let mut value = self.read_bits::<HIGH>()?;
        self.read_bits::<1>()?;
        for _ in 0..2 {
            value = value.wrapping_shl(15) | self.read_bits::<15>()?;
            self.read_bits::<1>()?;
        }
        Ok(value)
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

#[cfg(test)]
mod tests {
    use super::{Error, FieldReader};

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
}
