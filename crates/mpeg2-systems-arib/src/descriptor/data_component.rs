//! Data component descriptor, which names the coding of a data component,
//! and the caption information it carries

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use mpeg2_systems::descriptor::{DescriptorTag, Error, RawDescriptor};
use mpeg2_systems::field::TwoBitFieldOutOfRange;

use super::HEADER_LENGTH;
use crate::caption::Dmf;

/// `descriptor_tag` of the data component descriptor
const TAG: u8 = 0xfd;

/// Bytes of `data_component_id`
const DATA_COMPONENT_ID_LENGTH: usize = 2;

/// Largest body `descriptor_length` counts
#[cfg(feature = "alloc")]
const MAX_BODY_LENGTH: usize = 255;

/// Data component descriptor, which names the coding of a data component
///
/// `data_component_descriptor`, ARIB STD-B10 §6.2.20, Table 6-32
///
/// The crate does not read `additional_data_component_info` by itself,
/// since its syntax depends on `data_component_id`; for captions,
/// [`AdditionalAribCaptionInfo::read`] reads it.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems_arib::descriptor::data_component::{DataComponentDescriptor, DataComponentId};
///
/// let bytes = [0xfd, 0x03, 0x00, 0x08, 0xad];
/// let descriptor = DataComponentDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor.data_component_id, DataComponentId::CAPTION_AND_SUPERIMPOSE);
/// assert_eq!(descriptor.additional_data_component_info, [0xad]);
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "data_component_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataComponentDescriptor<'input> {
    /// Coding of the data component
    pub data_component_id: DataComponentId,
    /// Bytes whose syntax the coding of `data_component_id` defines
    pub additional_data_component_info: &'input [u8],
}

impl<'input> DataComponentDescriptor<'input> {
    /// Creates the descriptor of `data_component_id`, without additional
    /// data component info
    #[must_use]
    pub const fn new(data_component_id: DataComponentId) -> Self {
        Self {
            data_component_id,
            additional_data_component_info: &[],
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        HEADER_LENGTH
            .saturating_add(DATA_COMPONENT_ID_LENGTH)
            .saturating_add(self.additional_data_component_info.len())
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
        let length =
            DATA_COMPONENT_ID_LENGTH.saturating_add(self.additional_data_component_info.len());
        let mut buffer = [0; MAX_BODY_LENGTH];
        let Some(body) = buffer.get_mut(..length) else {
            return Err(Error::DescriptorLength { length });
        };
        // Why not refuse when the split fails: length counts data_component_id,
        // so the body is never shorter than it.
        if let Some((id, info)) = body.split_first_chunk_mut::<DATA_COMPONENT_ID_LENGTH>() {
            *id = self.data_component_id.get().to_be_bytes();
            info.copy_from_slice(self.additional_data_component_info);
        }

        RawDescriptor::new(DescriptorTag::from(TAG), body).write(out)
    }
}

impl<'input> TryFrom<&RawDescriptor<'input>> for DataComponentDescriptor<'input> {
    type Error = Error;

    /// Takes the fields of a data component descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0xFD.
    /// * [`BodyLength`](Error::BodyLength): the body is shorter than
    ///   `data_component_id`.
    fn try_from(raw: &RawDescriptor<'input>) -> Result<Self, Self::Error> {
        let expected = DescriptorTag::from(TAG);
        if raw.descriptor_tag != expected {
            return Err(Error::DescriptorTag {
                expected,
                found: raw.descriptor_tag,
            });
        }
        let Some((data_component_id, additional_data_component_info)) =
            raw.body.split_first_chunk::<DATA_COMPONENT_ID_LENGTH>()
        else {
            return Err(Error::BodyLength {
                descriptor_tag: expected,
                length: raw.body.len(),
            });
        };

        Ok(Self {
            data_component_id: DataComponentId::from(u16::from_be_bytes(*data_component_id)),
            additional_data_component_info,
        })
    }
}

/// Coding of a data component, which a standardization organization assigns
///
/// `data_component_id`, ARIB STD-B10 §6.2.20 and Annex J
#[doc(alias = "data_component_id")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataComponentId(u16);

impl DataComponentId {
    /// The caption and superimpose coding ARIB specifies, which ARIB STD-B10
    /// Annex J calls "ARIB-Subtitle & teletext coding" (ARIB STD-B24
    /// Volume 1 Part 3 §9.6)
    pub const CAPTION_AND_SUPERIMPOSE: Self = Self(0x0008);

    /// Returns the 16 bits
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl From<u16> for DataComponentId {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

/// Additional data component info of caption and superimpose: the display
/// mode and the display timing of the elementary stream
///
/// `additional_arib_caption_info`, ARIB STD-B24 Volume 1 Part 3 §9.6.1,
/// Table 9-15
///
/// The 2 `Reserved` bits are not kept: they are written `'11'`.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems_arib::caption::Dmf;
/// use mpeg2_systems_arib::descriptor::data_component::{
///     AdditionalAribCaptionInfo, DataComponentDescriptor, DataComponentId, Timing,
/// };
///
/// let bytes = [0xfd, 0x03, 0x00, 0x08, 0xad];
/// let descriptor = DataComponentDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// // Only the coding of captions carries this syntax
/// assert_eq!(descriptor.data_component_id, DataComponentId::CAPTION_AND_SUPERIMPOSE);
/// let info = AdditionalAribCaptionInfo::read(descriptor.additional_data_component_info)?;
/// assert_eq!(info.dmf.get(), 0b1010);
/// assert_eq!(info.timing, Timing::PROGRAM_SYNCHRONOUS);
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// info.write(&mut written)?;
/// assert_eq!(written, descriptor.additional_data_component_info);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "additional_arib_caption_info")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AdditionalAribCaptionInfo {
    /// Display mode of the elementary stream, when the same in the caption
    /// management data of every language
    pub dmf: Dmf,
    /// Timing of the caption display
    pub timing: Timing,
}

impl AdditionalAribCaptionInfo {
    /// Creates the information of `dmf` and `timing`
    #[must_use]
    pub const fn new(dmf: Dmf, timing: Timing) -> Self {
        Self { dmf, timing }
    }

    /// Reads the information from the `additional_data_component_info` of a
    /// data component descriptor
    ///
    /// `data_component_id` is not checked: the caller makes sure it is
    /// [`DataComponentId::CAPTION_AND_SUPERIMPOSE`] first.
    ///
    /// # Errors
    ///
    /// [`BodyLength`](Error::BodyLength): `additional_data_component_info`
    /// is not 1 byte; `length` is that of the body of the descriptor.
    pub fn read(additional_data_component_info: &[u8]) -> Result<Self, Error> {
        let &[byte] = additional_data_component_info else {
            return Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::from(TAG),
                length: DATA_COMPONENT_ID_LENGTH
                    .saturating_add(additional_data_component_info.len()),
            });
        };
        // Why not keep the 2 bits between DMF and Timing: they are Reserved
        // and written '11'.
        Ok(Self::new(Dmf(byte >> 4), Timing(byte & 0b11)))
    }

    /// Appends the information to `out`, its `Reserved` bits `'11'`
    ///
    /// A refused write leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`Forbidden`](Error::Forbidden): `Timing` is `'11'`, which Table 9-16
    /// does not define, or the playback half of `DMF` is `'11'`, which
    /// Table 9-5 reserves, in a `DMF` other than `'0011'` and `'1111'`.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.timing.get() == 0b11 {
            return Err(Error::Forbidden {
                field: "Timing",
                value: u32::from(self.timing.get()),
            });
        }
        let dmf = self.dmf.get();
        if dmf & 0b11 == 0b11 && dmf != 0b0011 && dmf != 0b1111 {
            return Err(Error::Forbidden {
                field: "DMF",
                value: u32::from(dmf),
            });
        }
        out.push(dmf << 4 | 0b1100 | self.timing.get());

        Ok(())
    }
}

/// Display timing of captions
///
/// `Timing`, ARIB STD-B24 Volume 1 Part 3 §9.6.1, Table 9-16
///
/// `'11'` is not defined, so it is read but not written.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::TwoBitFieldOutOfRange;
/// use mpeg2_systems_arib::descriptor::data_component::Timing;
///
/// assert_eq!(Timing::try_from(0b01), Ok(Timing::PROGRAM_SYNCHRONOUS));
/// // Timing is 2 bits
/// assert_eq!(Timing::try_from(4).map_err(TwoBitFieldOutOfRange::value), Err(4));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timing(u8);

impl Timing {
    /// `'00'`: captions are shown as they arrive
    pub const ASYNCHRONOUS: Self = Self(0b00);
    /// `'01'`: captions are shown in step with the program, by PTS
    pub const PROGRAM_SYNCHRONOUS: Self = Self(0b01);
    /// `'10'`: captions are shown at a time of day
    pub const TIME_SYNCHRONOUS: Self = Self(0b10);

    /// Returns the 2 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Timing {
    type Error = TwoBitFieldOutOfRange;

    /// Takes the 2 bits of `value`
    ///
    /// # Errors
    ///
    /// [`TwoBitFieldOutOfRange`]: `value` is 4 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        TwoBitFieldOutOfRange::check(value).map(Self)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use mpeg2_systems::descriptor::{DescriptorTag, Error, RawDescriptor};

    use super::{AdditionalAribCaptionInfo, DataComponentDescriptor, DataComponentId, Timing};
    use crate::caption::Dmf;

    /// The descriptor TSDuck's `tstabcomp` wrote for captions, DMF `'1010'`
    /// and Timing `'01'`
    const TSDUCK: [u8; 5] = [0xfd, 0x03, 0x00, 0x08, 0xad];

    fn caption_info() -> AdditionalAribCaptionInfo {
        AdditionalAribCaptionInfo::new(Dmf::try_from(0b1010).unwrap(), Timing::PROGRAM_SYNCHRONOUS)
    }

    fn caption_descriptor() -> DataComponentDescriptor<'static> {
        let mut descriptor = DataComponentDescriptor::new(DataComponentId::CAPTION_AND_SUPERIMPOSE);
        descriptor.additional_data_component_info = &[0xad];
        descriptor
    }

    #[test]
    fn the_bytes_after_data_component_id_are_the_additional_data_component_info() {
        assert_eq!(
            [
                RawDescriptor::read(&TSDUCK).unwrap(),
                RawDescriptor::new(DescriptorTag::from(0x52), &[0x00, 0x08]),
                RawDescriptor::new(DescriptorTag::from(0xfd), &[0x00]),
            ]
            .map(|raw| DataComponentDescriptor::try_from(&raw)),
            [
                Ok(caption_descriptor()),
                Err(Error::DescriptorTag {
                    expected: DescriptorTag::from(0xfd),
                    found: DescriptorTag::from(0x52)
                }),
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::from(0xfd),
                    length: 1
                }),
            ]
        );
    }

    #[test]
    fn the_caption_info_is_one_byte_of_dmf_and_timing() {
        assert_eq!(
            [&[0xad][..], &[], &[0xad, 0x00]].map(AdditionalAribCaptionInfo::read),
            [
                Ok(caption_info()),
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::from(0xfd),
                    length: 2
                }),
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::from(0xfd),
                    length: 4
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_descriptor_and_its_caption_info_are_written_back_as_read() {
        let mut descriptor = Vec::new();
        let mut info = Vec::new();

        assert_eq!(
            [
                caption_descriptor().write(&mut descriptor),
                caption_info().write(&mut info)
            ],
            [Ok(()), Ok(())]
        );
        assert_eq!(descriptor, TSDUCK);
        assert_eq!(caption_descriptor().encoded_len(), TSDUCK.len());
        assert_eq!(info, [0xad]);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_body_past_255_bytes_is_refused_and_leaves_the_output_as_it_was() {
        let mut too_long = caption_descriptor();
        too_long.additional_data_component_info = &[0; 254];
        let mut written = Vec::from([0x47]);

        assert_eq!(
            too_long.write(&mut written),
            Err(Error::DescriptorLength { length: 256 })
        );
        assert_eq!(written, [0x47]);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_reserved_timing_or_dmf_is_refused_and_leaves_the_output_as_it_was() {
        let dmf = |value| Dmf::try_from(value).unwrap();
        let timing = |value| Timing::try_from(value).unwrap();
        let mut written = Vec::from([0x47]);

        assert_eq!(
            [
                AdditionalAribCaptionInfo::new(dmf(0b1010), timing(0b11)),
                AdditionalAribCaptionInfo::new(dmf(0b0111), Timing::PROGRAM_SYNCHRONOUS),
                AdditionalAribCaptionInfo::new(dmf(0b0011), Timing::PROGRAM_SYNCHRONOUS),
                AdditionalAribCaptionInfo::new(dmf(0b1111), Timing::ASYNCHRONOUS),
            ]
            .map(|info| info.write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "Timing",
                    value: 3
                }),
                Err(Error::Forbidden {
                    field: "DMF",
                    value: 7
                }),
                Ok(()),
                Ok(()),
            ]
        );
        assert_eq!(written, [0x47, 0x3d, 0xfc]);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn reserved_bits_read_as_0_are_written_as_1() {
        let mut written = Vec::new();

        assert_eq!(
            AdditionalAribCaptionInfo::read(&[0xa1]).map(|info| info.write(&mut written)),
            Ok(Ok(()))
        );
        assert_eq!(written, [0xad]);
    }
}
