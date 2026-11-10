//! IBP descriptor, which describes the sequence of picture types of a video
//! stream

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::write_descriptor;
use super::{DescriptorTag, Error, RawDescriptor, field_error, fixed_body};
use crate::field::FieldReader;

/// Bytes of the body
const BODY_LENGTH: usize = 2;

/// IBP descriptor, which describes the sequence of picture types of a video
/// stream
///
/// `IBP_descriptor`, ISO/IEC 13818-1 §2.6.34, Table 2-71, semantics §2.6.35
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::ibp::IbpDescriptor;
///
/// // Closed GOPs of at most 15 pictures
/// let bytes = [0x12, 0x02, 0x80, 0x0f];
/// let descriptor = IbpDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor, IbpDescriptor::new(15).with_closed_gop_flag(true));
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
#[doc(alias("IBP_descriptor", "ibp_descriptor"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IbpDescriptor {
    /// Whether a group of pictures header with `closed_gop` set precedes
    /// every I-frame
    pub closed_gop_flag: bool,
    /// Whether the sequence of picture types between I-pictures is the same
    /// throughout the sequence
    pub identical_gop_flag: bool,
    /// Largest number of coded pictures between two consecutive I-pictures,
    /// 14 bits
    pub max_gop_length: u16,
}

impl IbpDescriptor {
    /// Creates the descriptor of `max_gop_length`, both flags clear
    #[must_use]
    pub const fn new(max_gop_length: u16) -> Self {
        Self {
            closed_gop_flag: false,
            identical_gop_flag: false,
            max_gop_length,
        }
    }

    /// Returns the value with the given `closed_gop_flag`
    #[must_use]
    pub const fn with_closed_gop_flag(mut self, closed_gop_flag: bool) -> Self {
        self.closed_gop_flag = closed_gop_flag;
        self
    }

    /// Returns the value with the given `identical_gop_flag`
    #[must_use]
    pub const fn with_identical_gop_flag(mut self, identical_gop_flag: bool) -> Self {
        self.identical_gop_flag = identical_gop_flag;
        self
    }

    /// Returns the value with the given `max_gop_length`
    #[must_use]
    pub const fn with_max_gop_length(mut self, max_gop_length: u16) -> Self {
        self.max_gop_length = max_gop_length;
        self
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

    /// Appends the descriptor to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `max_gop_length` is 0, which
    ///   §2.6.35 forbids.
    /// * [`Field`](Error::Field): `max_gop_length` wider than 14 bits.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.max_gop_length == 0 {
            return Err(Error::Forbidden {
                field: "max_gop_length",
                value: 0,
            });
        }

        write_descriptor(out, DescriptorTag::IBP, BODY_LENGTH, |writer| {
            writer
                .write_bool(self.closed_gop_flag)
                .map_err(field_error("closed_gop_flag"))?;
            writer
                .write_bool(self.identical_gop_flag)
                .map_err(field_error("identical_gop_flag"))?;
            writer
                .write_bits::<14>(self.max_gop_length)
                .map_err(field_error("max_gop_length"))
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for IbpDescriptor {
    type Error = Error;

    /// Takes the fields of an IBP descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x12.
    /// * [`BodyLength`](Error::BodyLength): the body is not 2 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::IBP)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            Ok(Self {
                closed_gop_flag: reader.read_bool()?,
                identical_gop_flag: reader.read_bool()?,
                max_gop_length: reader.read_u16::<14>()?,
            })
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::IbpDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};
    #[cfg(feature = "alloc")]
    use crate::field;

    #[test]
    fn a_body_of_other_than_2_bytes_is_refused() {
        assert_eq!(
            IbpDescriptor::try_from(&RawDescriptor::new(DescriptorTag::IBP, &[0x80])),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::IBP,
                length: 1
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_max_gop_length_of_0_or_wider_than_14_bits_is_refused_before_a_byte_is_written() {
        let mut written = Vec::new();

        assert_eq!(
            [0, 1 << 14].map(|length| IbpDescriptor::new(length).write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "max_gop_length",
                    value: 0
                }),
                Err(Error::Field {
                    field: "max_gop_length",
                    error: field::Error::ValueTooWide {
                        width: 14,
                        value: 1 << 14
                    }
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
