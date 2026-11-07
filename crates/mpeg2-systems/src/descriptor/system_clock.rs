//! System clock descriptor, which tells how accurate the clock the
//! timestamps were made from is

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::write_descriptor;
use super::{DescriptorTag, Error, RawDescriptor, field_error, fixed_body};
use crate::field::FieldReader;

/// Bytes of the body
const BODY_LENGTH: usize = 2;

/// System clock descriptor, which tells how accurate the clock the
/// timestamps were made from is
///
/// `system_clock_descriptor`, ISO/IEC 13818-1 §2.6.20, Table 2-64, semantics
/// §2.6.21
///
/// `clock_accuracy_integer` and `clock_accuracy_exponent` give the accuracy
/// in parts per million; an integer of 0 stands for the 30 ppm the
/// specification requires.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::system_clock::SystemClockDescriptor;
///
/// // An external reference, accurate to 5 × 10^-2 ppm
/// let bytes = [0x0b, 0x02, 0xc5, 0x5f];
/// let descriptor = SystemClockDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// let mut expected = SystemClockDescriptor::new(5, 2);
/// expected.external_clock_reference_indicator = true;
/// assert_eq!(descriptor, expected);
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "system_clock_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SystemClockDescriptor {
    /// Whether the system clock was derived from an external frequency
    /// reference the decoder may have too
    pub external_clock_reference_indicator: bool,
    /// Integer part of the accuracy, 6 bits
    pub clock_accuracy_integer: u8,
    /// Exponent of the accuracy, 3 bits
    pub clock_accuracy_exponent: u8,
}

impl SystemClockDescriptor {
    /// Creates the descriptor of a clock of the accuracy given, not derived
    /// from an external reference
    #[must_use]
    pub const fn new(clock_accuracy_integer: u8, clock_accuracy_exponent: u8) -> Self {
        Self {
            external_clock_reference_indicator: false,
            clock_accuracy_integer,
            clock_accuracy_exponent,
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub const fn encoded_len(&self) -> usize {
        super::HEADER_LENGTH.saturating_add(BODY_LENGTH)
    }

    /// Appends the descriptor to `out`, its `reserved` bits all `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`Field`](Error::Field): `clock_accuracy_integer` wider than 6 bits,
    /// or `clock_accuracy_exponent` wider than 3.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write_descriptor(out, DescriptorTag::SYSTEM_CLOCK, BODY_LENGTH, |writer| {
            writer
                .write_bool(self.external_clock_reference_indicator)
                .map_err(field_error("external_clock_reference_indicator"))?;
            writer
                .write_bool(true)
                .and_then(|()| writer.write_bits::<6>(self.clock_accuracy_integer))
                .map_err(field_error("clock_accuracy_integer"))?;
            writer
                .write_bits::<3>(self.clock_accuracy_exponent)
                .and_then(|()| writer.write_bits::<5>(0b1_1111_u8))
                .map_err(field_error("clock_accuracy_exponent"))
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for SystemClockDescriptor {
    type Error = Error;

    /// Takes the fields of a system clock descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x0B.
    /// * [`BodyLength`](Error::BodyLength): the body is not 2 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::SYSTEM_CLOCK)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            let external_clock_reference_indicator = reader.read_bool()?;
            reader.read_bool()?;
            Ok(Self {
                external_clock_reference_indicator,
                clock_accuracy_integer: reader.read_u8::<6>()?,
                clock_accuracy_exponent: reader.read_u8::<3>()?,
            })
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::SystemClockDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};
    #[cfg(feature = "alloc")]
    use crate::field;

    #[cfg(feature = "alloc")]
    #[test]
    fn reserved_bits_read_as_0_are_written_as_1() {
        let descriptor = SystemClockDescriptor::try_from(
            &RawDescriptor::read(&[0x0b, 0x02, 0x85, 0x40]).unwrap(),
        )
        .unwrap();
        let mut written = Vec::new();
        descriptor.write(&mut written).unwrap();

        assert_eq!(written, [0x0b, 0x02, 0xc5, 0x5f]);
    }

    #[test]
    fn a_body_of_other_than_2_bytes_is_refused() {
        assert_eq!(
            SystemClockDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::SYSTEM_CLOCK,
                &[0xc5]
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::SYSTEM_CLOCK,
                length: 1
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_value_wider_than_its_field_is_refused_before_a_byte_is_written() {
        let mut written = Vec::new();

        assert_eq!(
            [
                SystemClockDescriptor::new(64, 0),
                SystemClockDescriptor::new(0, 8)
            ]
            .map(|descriptor| descriptor.write(&mut written)),
            [
                Err(Error::Field {
                    field: "clock_accuracy_integer",
                    error: field::Error::ValueTooWide {
                        width: 6,
                        value: 64
                    }
                }),
                Err(Error::Field {
                    field: "clock_accuracy_exponent",
                    error: field::Error::ValueTooWide { width: 3, value: 8 }
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
