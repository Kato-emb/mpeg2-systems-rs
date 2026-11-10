//! Maximum bitrate descriptor, which bounds the bitrate of a program or a
//! program element

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::write_descriptor;
use super::{DescriptorTag, Error, RawDescriptor, field_error, fixed_body};
use crate::field::FieldReader;

/// Bytes of the body
const BODY_LENGTH: usize = 3;

/// Maximum bitrate descriptor, which bounds the bitrate of a program or a
/// program element
///
/// `maximum_bitrate_descriptor`, ISO/IEC 13818-1 §2.6.26, Table 2-67,
/// semantics §2.6.27
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::maximum_bitrate::MaximumBitrateDescriptor;
///
/// // 40000 × 50 bytes a second, 16 Mbit/s
/// let bytes = [0x0e, 0x03, 0xc0, 0x9c, 0x40];
/// let descriptor = MaximumBitrateDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor, MaximumBitrateDescriptor::new(40000));
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
#[doc(alias = "maximum_bitrate_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MaximumBitrateDescriptor {
    /// Upper bound of the bitrate, transport overhead included, in units of
    /// 50 bytes a second, 22 bits
    pub maximum_bitrate: u32,
}

impl MaximumBitrateDescriptor {
    /// Creates the descriptor of `maximum_bitrate`
    #[must_use]
    pub const fn new(maximum_bitrate: u32) -> Self {
        Self { maximum_bitrate }
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

    /// Appends the descriptor to `out`, its `reserved` bits `'11'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `maximum_bitrate` is 0, which is
    ///   not the positive integer §2.6.27 requires.
    /// * [`Field`](Error::Field): `maximum_bitrate` wider than 22 bits.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.maximum_bitrate == 0 {
            return Err(Error::Forbidden {
                field: "maximum_bitrate",
                value: 0,
            });
        }

        write_descriptor(out, DescriptorTag::MAXIMUM_BITRATE, BODY_LENGTH, |writer| {
            writer
                .write_bits::<2>(0b11_u8)
                .and_then(|()| writer.write_bits::<22>(self.maximum_bitrate))
                .map_err(field_error("maximum_bitrate"))
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for MaximumBitrateDescriptor {
    type Error = Error;

    /// Takes the field of a maximum bitrate descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x0E.
    /// * [`BodyLength`](Error::BodyLength): the body is not 3 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::MAXIMUM_BITRATE)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            reader.read_bits::<2>()?;
            Ok(Self::new(reader.read_u32::<22>()?))
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::MaximumBitrateDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};
    #[cfg(feature = "alloc")]
    use crate::field;

    #[test]
    fn a_body_of_other_than_3_bytes_is_refused() {
        assert_eq!(
            MaximumBitrateDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::MAXIMUM_BITRATE,
                &[0xc0, 0x9c]
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::MAXIMUM_BITRATE,
                length: 2
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_bitrate_of_0_or_wider_than_22_bits_is_refused_before_a_byte_is_written() {
        let mut written = Vec::new();

        assert_eq!(
            [0, 1 << 22].map(|bitrate| MaximumBitrateDescriptor::new(bitrate).write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "maximum_bitrate",
                    value: 0
                }),
                Err(Error::Field {
                    field: "maximum_bitrate",
                    error: field::Error::ValueTooWide {
                        width: 22,
                        value: 1 << 22
                    }
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
