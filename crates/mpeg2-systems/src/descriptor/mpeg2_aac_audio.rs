//! MPEG-2 AAC audio descriptor, which identifies the coding parameters of an
//! ISO/IEC 13818-7 audio stream

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{DescriptorTag, Error, RawDescriptor, fixed_body};
#[cfg(feature = "alloc")]
use super::{field_error, write_descriptor};

/// Bytes of the body
const BODY_LENGTH: usize = 3;

/// Whether an AAC bitstream embeds bandwidth extension data
///
/// `MPEG-2_AAC_additional_information`, ISO/IEC 13818-1 §2.6.69, Table 2-95
///
/// 0x02 to 0xFF are reserved.
#[doc(alias = "MPEG-2_AAC_additional_information")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mpeg2AacAdditionalInformation(u8);

impl Mpeg2AacAdditionalInformation {
    /// AAC data according to ISO/IEC 13818-7
    pub const AAC: Self = Self(0x00);
    /// AAC data with bandwidth extension data present
    pub const AAC_WITH_BANDWIDTH_EXTENSION: Self = Self(0x01);

    /// Returns the 8-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<u8> for Mpeg2AacAdditionalInformation {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

/// MPEG-2 AAC audio descriptor, which identifies the coding parameters of an
/// ISO/IEC 13818-7 audio stream
///
/// `MPEG-2_AAC_audio_descriptor`, ISO/IEC 13818-1 §2.6.68, Table 2-94,
/// semantics §2.6.69
///
/// `MPEG-2_AAC_profile` and `MPEG-2_AAC_channel_configuration` index tables
/// of ISO/IEC 13818-7; the crate does not check them against it.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::mpeg2_aac_audio::{
///     Mpeg2AacAdditionalInformation, Mpeg2AacAudioDescriptor,
/// };
///
/// // Low Complexity profile, 2 channels, with bandwidth extension
/// let bytes = [0x2b, 0x03, 0x01, 0x02, 0x01];
/// let descriptor = Mpeg2AacAudioDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(
///     descriptor.mpeg_2_aac_additional_information,
///     Mpeg2AacAdditionalInformation::AAC_WITH_BANDWIDTH_EXTENSION
/// );
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
#[doc(alias("MPEG-2_AAC_audio_descriptor", "MPEG-2 AAC_audio_descriptor"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mpeg2AacAudioDescriptor {
    /// AAC profile, the index of Table 31 of ISO/IEC 13818-7
    pub mpeg_2_aac_profile: u8,
    /// Number and configuration of the audio channels, 1 to 6 as in Table 42
    /// of ISO/IEC 13818-7, any other value undefined
    pub mpeg_2_aac_channel_configuration: u8,
    /// Whether the bitstream embeds bandwidth extension data
    pub mpeg_2_aac_additional_information: Mpeg2AacAdditionalInformation,
}

impl Mpeg2AacAudioDescriptor {
    /// Creates the descriptor of a profile and channel configuration, without
    /// bandwidth extension data
    #[must_use]
    pub const fn new(mpeg_2_aac_profile: u8, mpeg_2_aac_channel_configuration: u8) -> Self {
        Self {
            mpeg_2_aac_profile,
            mpeg_2_aac_channel_configuration,
            mpeg_2_aac_additional_information: Mpeg2AacAdditionalInformation::AAC,
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
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`Forbidden`](Error::Forbidden): `MPEG-2_AAC_additional_information`
    /// is 0x02 to 0xFF, reserved.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let additional_information = self.mpeg_2_aac_additional_information.get();
        if !matches!(
            self.mpeg_2_aac_additional_information,
            Mpeg2AacAdditionalInformation::AAC
                | Mpeg2AacAdditionalInformation::AAC_WITH_BANDWIDTH_EXTENSION
        ) {
            return Err(Error::Forbidden {
                field: "MPEG-2_AAC_additional_information",
                value: u32::from(additional_information),
            });
        }

        write_descriptor(
            out,
            DescriptorTag::MPEG_2_AAC_AUDIO,
            BODY_LENGTH,
            |writer| {
                writer
                    .write_bytes(&[
                        self.mpeg_2_aac_profile,
                        self.mpeg_2_aac_channel_configuration,
                        additional_information,
                    ])
                    .map_err(field_error("descriptor body"))
            },
        )
    }
}

impl TryFrom<&RawDescriptor<'_>> for Mpeg2AacAudioDescriptor {
    type Error = Error;

    /// Takes the fields of an MPEG-2 AAC audio descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x2B.
    /// * [`BodyLength`](Error::BodyLength): the body is not 3 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let [profile, channel_configuration, additional_information] =
            *fixed_body::<BODY_LENGTH>(raw, DescriptorTag::MPEG_2_AAC_AUDIO)?;

        Ok(Self {
            mpeg_2_aac_profile: profile,
            mpeg_2_aac_channel_configuration: channel_configuration,
            mpeg_2_aac_additional_information: Mpeg2AacAdditionalInformation(
                additional_information,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{Mpeg2AacAdditionalInformation, Mpeg2AacAudioDescriptor};
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    #[test]
    fn a_reserved_additional_information_is_kept_when_read() {
        let mut expected = Mpeg2AacAudioDescriptor::new(1, 2);
        expected.mpeg_2_aac_additional_information = Mpeg2AacAdditionalInformation::from(0x02);

        assert_eq!(
            [
                RawDescriptor::read(&[0x2b, 0x03, 0x01, 0x02, 0x02]).unwrap(),
                RawDescriptor::new(DescriptorTag::MPEG_2_AAC_AUDIO, &[0x01, 0x02]),
            ]
            .map(|raw| Mpeg2AacAudioDescriptor::try_from(&raw)),
            [
                Ok(expected),
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::MPEG_2_AAC_AUDIO,
                    length: 2
                })
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_reserved_additional_information_is_refused_before_a_byte_is_written() {
        let mut descriptor = Mpeg2AacAudioDescriptor::new(1, 2);
        descriptor.mpeg_2_aac_additional_information = Mpeg2AacAdditionalInformation::from(0x02);
        let mut written = Vec::new();

        assert_eq!(
            [
                descriptor.write(&mut written),
                Mpeg2AacAudioDescriptor::new(1, 2).write(&mut written)
            ],
            [
                Err(Error::Forbidden {
                    field: "MPEG-2_AAC_additional_information",
                    value: 0x02
                }),
                Ok(())
            ]
        );
        assert_eq!(written, [0x2b, 0x03, 0x01, 0x02, 0x00]);
    }
}
