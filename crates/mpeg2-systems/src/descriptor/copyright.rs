//! Copyright descriptor, which identifies an audiovisual work

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{DescriptorTag, Error, RawDescriptor, prefix_body};
#[cfg(feature = "alloc")]
use super::{field_error, write_descriptor};

/// Bytes of `copyright_identifier`
const COPYRIGHT_IDENTIFIER_LENGTH: usize = 4;

/// Copyright descriptor, which identifies an audiovisual work
///
/// `copyright_descriptor`, ISO/IEC 13818-1 §2.6.24, Table 2-66, semantics
/// §2.6.25
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::copyright::CopyrightDescriptor;
///
/// // An identifier with two bytes of additional copyright info
/// let bytes = [0x0d, 0x06, 0x49, 0x53, 0x41, 0x4e, 0x01, 0x02];
/// let descriptor = CopyrightDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor.copyright_identifier.to_be_bytes(), *b"ISAN");
/// assert_eq!(descriptor.additional_copyright_info, [0x01, 0x02]);
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
#[doc(alias = "copyright_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CopyrightDescriptor<'input> {
    /// Identifier obtained from the Registration Authority
    pub copyright_identifier: u32,
    /// Bytes whose meaning the assignee of `copyright_identifier` defines
    pub additional_copyright_info: &'input [u8],
}

impl<'input> CopyrightDescriptor<'input> {
    /// Creates the descriptor of `copyright_identifier`, without additional
    /// copyright info
    #[must_use]
    pub const fn new(copyright_identifier: u32) -> Self {
        Self {
            copyright_identifier,
            additional_copyright_info: &[],
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        super::HEADER_LENGTH
            .saturating_add(COPYRIGHT_IDENTIFIER_LENGTH)
            .saturating_add(self.additional_copyright_info.len())
    }

    /// Appends the descriptor to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`DescriptorLength`](Error::DescriptorLength): the body would be
    /// longer than 255 bytes.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let body_length =
            COPYRIGHT_IDENTIFIER_LENGTH.saturating_add(self.additional_copyright_info.len());
        write_descriptor(out, DescriptorTag::COPYRIGHT, body_length, |writer| {
            writer
                .write_bits::<32>(self.copyright_identifier)
                .map_err(field_error("copyright_identifier"))?;
            writer
                .write_bytes(self.additional_copyright_info)
                .map_err(field_error("additional_copyright_info"))
        })
    }
}

impl<'input> TryFrom<&RawDescriptor<'input>> for CopyrightDescriptor<'input> {
    type Error = Error;

    /// Takes the fields of a copyright descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x0D.
    /// * [`BodyLength`](Error::BodyLength): the body is shorter than
    ///   `copyright_identifier`.
    fn try_from(raw: &RawDescriptor<'input>) -> Result<Self, Self::Error> {
        let (copyright_identifier, additional_copyright_info) =
            prefix_body::<COPYRIGHT_IDENTIFIER_LENGTH>(raw, DescriptorTag::COPYRIGHT)?;

        Ok(Self {
            copyright_identifier: u32::from_be_bytes(*copyright_identifier),
            additional_copyright_info,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::CopyrightDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    #[test]
    fn a_body_shorter_than_copyright_identifier_is_refused() {
        assert_eq!(
            CopyrightDescriptor::try_from(&RawDescriptor::new(DescriptorTag::COPYRIGHT, b"ISA")),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::COPYRIGHT,
                length: 3
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_body_longer_than_255_bytes_is_refused_before_a_byte_is_written() {
        let mut descriptor = CopyrightDescriptor::new(0x4953_414e);
        descriptor.additional_copyright_info = &[0; 252];
        let mut written = Vec::new();

        assert_eq!(
            descriptor.write(&mut written),
            Err(Error::DescriptorLength { length: 256 })
        );
        assert_eq!(written, []);
    }
}
