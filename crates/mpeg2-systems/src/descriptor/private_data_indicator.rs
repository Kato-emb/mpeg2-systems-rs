//! Private data indicator descriptor, which carries a privately defined
//! indicator

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{DescriptorTag, Error, RawDescriptor, fixed_body};
#[cfg(feature = "alloc")]
use super::{field_error, write_descriptor};

/// Bytes of the body
const BODY_LENGTH: usize = 4;

/// Private data indicator descriptor, which carries a privately defined
/// indicator
///
/// `private_data_indicator_descriptor`, ISO/IEC 13818-1 §2.6.28, Table 2-68,
/// semantics §2.6.29
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::private_data_indicator::PrivateDataIndicatorDescriptor;
///
/// let bytes = [0x0f, 0x04, 0x41, 0x42, 0x43, 0x44];
/// let descriptor = PrivateDataIndicatorDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor.private_data_indicator.to_be_bytes(), *b"ABCD");
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
#[doc(alias = "private_data_indicator_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrivateDataIndicatorDescriptor {
    /// Indicator whose value is private
    pub private_data_indicator: u32,
}

impl PrivateDataIndicatorDescriptor {
    /// Creates the descriptor of `private_data_indicator`
    #[must_use]
    pub const fn new(private_data_indicator: u32) -> Self {
        Self {
            private_data_indicator,
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

    /// Appends the descriptor to `out`
    ///
    /// # Errors
    ///
    /// None: the indicator fits its field. The `Result` is that of every
    /// descriptor's `write`.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write_descriptor(
            out,
            DescriptorTag::PRIVATE_DATA_INDICATOR,
            BODY_LENGTH,
            |writer| {
                writer
                    .write_bits::<32>(self.private_data_indicator)
                    .map_err(field_error("private_data_indicator"))
            },
        )
    }
}

impl TryFrom<&RawDescriptor<'_>> for PrivateDataIndicatorDescriptor {
    type Error = Error;

    /// Takes the field of a private data indicator descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x0F.
    /// * [`BodyLength`](Error::BodyLength): the body is not 4 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::PRIVATE_DATA_INDICATOR)?;

        Ok(Self::new(u32::from_be_bytes(*body)))
    }
}

#[cfg(test)]
mod tests {
    use super::PrivateDataIndicatorDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    #[test]
    fn a_body_of_other_than_4_bytes_is_refused() {
        assert_eq!(
            PrivateDataIndicatorDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::PRIVATE_DATA_INDICATOR,
                b"ABCDE"
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::PRIVATE_DATA_INDICATOR,
                length: 5
            })
        );
    }
}
