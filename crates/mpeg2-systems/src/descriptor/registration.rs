//! Registration descriptor, which identifies the format of private data

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::typed::prefix_body;
#[cfg(feature = "alloc")]
use super::typed::{field_error, write_descriptor};
use super::{DescriptorTag, Error, RawDescriptor};

/// Bytes of `format_identifier`
const FORMAT_IDENTIFIER_LENGTH: usize = 4;

/// Registration descriptor, which identifies the format of private data
///
/// `registration_descriptor`, ISO/IEC 13818-1 §2.6.8, Table 2-51, semantics
/// §2.6.9
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::registration::RegistrationDescriptor;
///
/// // "HDMV", with two bytes of additional identification info
/// let bytes = [0x05, 0x06, 0x48, 0x44, 0x4d, 0x56, 0x01, 0x02];
/// let descriptor = RegistrationDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(
///     descriptor,
///     RegistrationDescriptor::new(u32::from_be_bytes(*b"HDMV"))
///         .with_additional_identification_info(&[0x01, 0x02])
/// );
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "registration_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegistrationDescriptor<'input> {
    /// Format, a value obtained from the Registration Authority
    pub format_identifier: u32,
    /// Bytes whose meaning the assignee of `format_identifier` defines
    pub additional_identification_info: &'input [u8],
}

impl<'input> RegistrationDescriptor<'input> {
    /// Creates the descriptor of `format_identifier`, without additional
    /// identification info
    #[must_use]
    pub const fn new(format_identifier: u32) -> Self {
        Self {
            format_identifier,
            additional_identification_info: &[],
        }
    }

    /// Returns the value with the given `additional_identification_info`
    #[must_use]
    pub const fn with_additional_identification_info(
        mut self,
        additional_identification_info: &'input [u8],
    ) -> Self {
        self.additional_identification_info = additional_identification_info;
        self
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        super::raw::HEADER_LENGTH
            .saturating_add(FORMAT_IDENTIFIER_LENGTH)
            .saturating_add(self.additional_identification_info.len())
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
            FORMAT_IDENTIFIER_LENGTH.saturating_add(self.additional_identification_info.len());
        write_descriptor(out, DescriptorTag::REGISTRATION, body_length, |writer| {
            writer
                .write_bits::<32>(self.format_identifier)
                .map_err(field_error("format_identifier"))?;
            writer
                .write_bytes(self.additional_identification_info)
                .map_err(field_error("additional_identification_info"))
        })
    }
}

impl<'input> TryFrom<&RawDescriptor<'input>> for RegistrationDescriptor<'input> {
    type Error = Error;

    /// Takes the fields of a registration descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x05.
    /// * [`BodyLength`](Error::BodyLength): the body is shorter than
    ///   `format_identifier`.
    fn try_from(raw: &RawDescriptor<'input>) -> Result<Self, Self::Error> {
        let (format_identifier, additional_identification_info) =
            prefix_body::<FORMAT_IDENTIFIER_LENGTH>(raw, DescriptorTag::REGISTRATION)?;

        Ok(Self {
            format_identifier: u32::from_be_bytes(*format_identifier),
            additional_identification_info,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::RegistrationDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    /// "HDMV" with two bytes of additional identification info
    const WITH_INFO: [u8; 8] = [0x05, 0x06, 0x48, 0x44, 0x4d, 0x56, 0x01, 0x02];

    fn with_info() -> RegistrationDescriptor<'static> {
        let mut descriptor = RegistrationDescriptor::new(0x4844_4d56);
        descriptor.additional_identification_info = &[0x01, 0x02];
        descriptor
    }

    #[test]
    fn the_bytes_after_format_identifier_are_the_additional_identification_info() {
        assert_eq!(
            [
                RawDescriptor::read(&WITH_INFO).unwrap(),
                RawDescriptor::new(DescriptorTag::REGISTRATION, &[0x48, 0x44, 0x4d]),
            ]
            .map(|raw| RegistrationDescriptor::try_from(&raw)),
            [
                Ok(with_info()),
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::REGISTRATION,
                    length: 3
                })
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_descriptor_is_written_as_laid_out_by_hand_up_to_255_bytes_of_body() {
        let mut too_long = with_info();
        too_long.additional_identification_info = &[0; 252];
        let mut written = Vec::new();

        assert_eq!(
            [
                too_long.write(&mut written),
                with_info().write(&mut written)
            ],
            [Err(Error::DescriptorLength { length: 256 }), Ok(())]
        );
        assert_eq!(written, WITH_INFO);
    }
}
