//! Stream identifier descriptor, which labels a component of a service

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use mpeg2_systems::descriptor::{DescriptorTag, Error, RawDescriptor};

use super::HEADER_LENGTH;

/// `descriptor_tag` of the stream identifier descriptor
const TAG: u8 = 0x52;

/// Bytes of `component_tag`, the whole body
const BODY_LENGTH: usize = 1;

/// Stream identifier descriptor, which labels a component of a service so
/// that other tables can refer to it
///
/// `stream_identifier_descriptor`, ARIB STD-B10 §6.2.16, Table 6-28
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems_arib::descriptor::stream_identifier::StreamIdentifierDescriptor;
///
/// let bytes = [0x52, 0x01, 0x30];
/// let descriptor = StreamIdentifierDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor.component_tag, 0x30);
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "stream_identifier_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamIdentifierDescriptor {
    /// Label of the component, which the component descriptor of the EIT
    /// and the SDT names too
    pub component_tag: u8,
}

impl StreamIdentifierDescriptor {
    /// Creates the descriptor of `component_tag`
    #[must_use]
    pub const fn new(component_tag: u8) -> Self {
        Self { component_tag }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub const fn encoded_len(&self) -> usize {
        HEADER_LENGTH.saturating_add(BODY_LENGTH)
    }

    /// Appends the descriptor to `out`
    ///
    /// # Errors
    ///
    /// None: the one byte of body fits `descriptor_length`. The [`Result`] is
    /// that of every descriptor's `write`.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        RawDescriptor::new(DescriptorTag::from(TAG), &[self.component_tag]).write(out)
    }
}

impl TryFrom<&RawDescriptor<'_>> for StreamIdentifierDescriptor {
    type Error = Error;

    /// Takes the `component_tag` of a stream identifier descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x52.
    /// * [`BodyLength`](Error::BodyLength): the body is not 1 byte.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let expected = DescriptorTag::from(TAG);
        if raw.descriptor_tag != expected {
            return Err(Error::DescriptorTag {
                expected,
                found: raw.descriptor_tag,
            });
        }
        let &[component_tag] = raw.body else {
            return Err(Error::BodyLength {
                descriptor_tag: expected,
                length: raw.body.len(),
            });
        };

        Ok(Self::new(component_tag))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use mpeg2_systems::descriptor::{DescriptorTag, Error, RawDescriptor};

    use super::StreamIdentifierDescriptor;

    /// The descriptor TSDuck's `tstabcomp` wrote for `component_tag` 0x30
    const TSDUCK: [u8; 3] = [0x52, 0x01, 0x30];

    #[test]
    fn the_one_byte_of_body_is_the_component_tag() {
        assert_eq!(
            [
                RawDescriptor::read(&TSDUCK).unwrap(),
                RawDescriptor::new(DescriptorTag::from(0x53), &[0x30]),
                RawDescriptor::new(DescriptorTag::from(0x52), &[]),
                RawDescriptor::new(DescriptorTag::from(0x52), &[0x30, 0x31]),
            ]
            .map(|raw| StreamIdentifierDescriptor::try_from(&raw)),
            [
                Ok(StreamIdentifierDescriptor::new(0x30)),
                Err(Error::DescriptorTag {
                    expected: DescriptorTag::from(0x52),
                    found: DescriptorTag::from(0x53)
                }),
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::from(0x52),
                    length: 0
                }),
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::from(0x52),
                    length: 2
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_descriptor_is_written_back_as_read() {
        let descriptor = StreamIdentifierDescriptor::new(0x30);
        let mut written = Vec::new();

        assert_eq!(descriptor.write(&mut written), Ok(()));
        assert_eq!(written, TSDUCK);
        assert_eq!(descriptor.encoded_len(), TSDUCK.len());
    }
}
