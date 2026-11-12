//! HEVC video descriptor, which identifies the profile, tier and level of an
//! HEVC video stream

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::typed::write_descriptor;
use super::typed::{body_of, field_error};
use super::{DescriptorTag, Error, RawDescriptor};
use crate::field::FieldReader;

/// Bytes of the body without the temporal layer subset
const BODY_LENGTH: usize = 13;

/// Index in the body of the byte that starts with
/// `temporal_layer_subset_flag`, after `level_idc`
const SUBSET_FLAG_INDEX: usize = 12;

/// Bytes of `temporal_id_min` and `temporal_id_max` with their `reserved`
/// bits
const TEMPORAL_LAYER_SUBSET_LENGTH: usize = 2;

/// Returns the bytes of the body `temporal_layer_subset_flag` makes
const fn body_length(temporal_layer_subset_flag: bool) -> usize {
    if temporal_layer_subset_flag {
        BODY_LENGTH.saturating_add(TEMPORAL_LAYER_SUBSET_LENGTH)
    } else {
        BODY_LENGTH
    }
}

/// The `TemporalId`s an [`HevcVideoDescriptor`] carries when
/// `temporal_layer_subset_flag` is `'1'`
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TemporalLayerSubset {
    /// Lowest `TemporalId` of the access units of the stream, 3 bits
    pub temporal_id_min: u8,
    /// Highest `TemporalId` of the access units of the stream, 3 bits
    pub temporal_id_max: u8,
}

impl TemporalLayerSubset {
    /// Creates the subset of `TemporalId`s `temporal_id_min` to
    /// `temporal_id_max`
    #[must_use]
    pub const fn new(temporal_id_min: u8, temporal_id_max: u8) -> Self {
        Self {
            temporal_id_min,
            temporal_id_max,
        }
    }
}

/// HEVC video descriptor, which identifies the profile, tier and level of an
/// HEVC video stream
///
/// `HEVC_video_descriptor`, ISO/IEC 13818-1 §2.6.95, Table 2-113, semantics
/// §2.6.96
///
/// The profile, tier and level fields are coded as the `general_` or
/// `sub_layer_` fields of Rec. ITU-T H.265 | ISO/IEC 23008-2; the crate does
/// not check them against it. `temporal_layer_subset_flag` follows from
/// whether [`temporal_layer_subset`](Self::temporal_layer_subset) is
/// present, which makes the body 2 bytes longer.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::hevc_video::{HevcVideoDescriptor, TemporalLayerSubset};
///
/// // Main profile at level 3.1, TemporalId 0 to 2
/// let bytes = [
///     0x38, 0x0f, 0x01, 0x60, 0x00, 0x00, 0x00, 0xb0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5d,
///     0x9f, 0x1f, 0x5f,
/// ];
/// let descriptor = HevcVideoDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!((descriptor.profile_idc, descriptor.level_idc), (1, 93));
/// assert_eq!(
///     descriptor.temporal_layer_subset,
///     Some(TemporalLayerSubset::new(0, 2))
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
#[doc(alias = "HEVC_video_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the syntax table has these flags, and each is a field"
)]
pub struct HevcVideoDescriptor {
    /// `general_profile_space` or `sub_layer_profile_space`, 2 bits
    pub profile_space: u8,
    /// `general_tier_flag` or `sub_layer_tier_flag`
    pub tier_flag: bool,
    /// `general_profile_idc` or `sub_layer_profile_idc`, 5 bits
    pub profile_idc: u8,
    /// The 32 profile compatibility flags, flag 0 the most significant bit
    pub profile_compatibility_indication: u32,
    /// `general_progressive_source_flag` or its `sub_layer_` counterpart
    pub progressive_source_flag: bool,
    /// `general_interlaced_source_flag` or its `sub_layer_` counterpart
    pub interlaced_source_flag: bool,
    /// `general_non_packed_constraint_flag` or its `sub_layer_` counterpart
    pub non_packed_constraint_flag: bool,
    /// `general_frame_only_constraint_flag` or its `sub_layer_` counterpart
    pub frame_only_constraint_flag: bool,
    /// The 44 bits of `profile_tier_level()` between
    /// `general_frame_only_constraint_flag` and `general_level_idc`
    pub copied_44bits: u64,
    /// `general_level_idc` or `sub_layer_level_idc`
    pub level_idc: u8,
    /// Whether the stream may include HEVC still pictures
    pub hevc_still_present_flag: bool,
    /// Whether the stream may include HEVC 24-hour pictures
    pub hevc_24hr_picture_present_flag: bool,
    /// `'1'` when the stream has no sub-picture HRD parameters
    pub sub_pic_hrd_params_not_present_flag: bool,
    /// Presence of HDR and WCG video components, Table 2-114, 2 bits
    pub hdr_wcg_idc: u8,
    /// The `TemporalId`s of the stream, present when
    /// `temporal_layer_subset_flag` is `'1'`
    pub temporal_layer_subset: Option<TemporalLayerSubset>,
}

impl HevcVideoDescriptor {
    /// Creates the descriptor of `profile_idc` and `level_idc`, every other
    /// field 0 or clear but `HDR_WCG_idc`, 3, which makes no indication
    #[must_use]
    pub const fn new(profile_idc: u8, level_idc: u8) -> Self {
        Self {
            profile_space: 0,
            tier_flag: false,
            profile_idc,
            profile_compatibility_indication: 0,
            progressive_source_flag: false,
            interlaced_source_flag: false,
            non_packed_constraint_flag: false,
            frame_only_constraint_flag: false,
            copied_44bits: 0,
            level_idc,
            hevc_still_present_flag: false,
            hevc_24hr_picture_present_flag: false,
            sub_pic_hrd_params_not_present_flag: false,
            hdr_wcg_idc: 3,
            temporal_layer_subset: None,
        }
    }

    /// Returns whether the descriptor carries `temporal_id_min` and
    /// `temporal_id_max`
    #[must_use]
    pub const fn temporal_layer_subset_flag(&self) -> bool {
        self.temporal_layer_subset.is_some()
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        super::raw::HEADER_LENGTH.saturating_add(body_length(self.temporal_layer_subset_flag()))
    }

    /// Appends the descriptor to `out`, its `reserved` bits all `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `temporal_id_min` is above
    ///   `temporal_id_max`, so no `TemporalId` lies between them.
    /// * [`Field`](Error::Field): `profile_space`, `profile_idc`,
    ///   `copied_44bits`, `HDR_WCG_idc` or a `TemporalId` wider than its
    ///   field.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if let Some(subset) = self.temporal_layer_subset
            && subset.temporal_id_min > subset.temporal_id_max
        {
            return Err(Error::Forbidden {
                field: "temporal_id_min",
                value: u32::from(subset.temporal_id_min),
            });
        }

        let body_length = body_length(self.temporal_layer_subset_flag());
        write_descriptor(out, DescriptorTag::HEVC_VIDEO, body_length, |writer| {
            writer
                .write_bits::<2>(self.profile_space)
                .map_err(field_error("profile_space"))?;
            writer
                .write_bool(self.tier_flag)
                .map_err(field_error("tier_flag"))?;
            writer
                .write_bits::<5>(self.profile_idc)
                .map_err(field_error("profile_idc"))?;
            (|| {
                writer.write_bits::<32>(self.profile_compatibility_indication)?;
                writer.write_bool(self.progressive_source_flag)?;
                writer.write_bool(self.interlaced_source_flag)?;
                writer.write_bool(self.non_packed_constraint_flag)?;
                writer.write_bool(self.frame_only_constraint_flag)
            })()
            // Why not an error per field: every value fits its field's width.
            .map_err(field_error("descriptor body"))?;
            writer
                .write_bits::<44>(self.copied_44bits)
                .map_err(field_error("copied_44bits"))?;
            (|| {
                writer.write_bits::<8>(self.level_idc)?;
                writer.write_bool(self.temporal_layer_subset_flag())?;
                writer.write_bool(self.hevc_still_present_flag)?;
                writer.write_bool(self.hevc_24hr_picture_present_flag)?;
                writer.write_bool(self.sub_pic_hrd_params_not_present_flag)?;
                writer.write_bits::<2>(0b11_u8)
            })()
            // Why not an error per field: every value fits its field's width.
            .map_err(field_error("descriptor body"))?;
            writer
                .write_bits::<2>(self.hdr_wcg_idc)
                .map_err(field_error("HDR_WCG_idc"))?;
            if let Some(subset) = self.temporal_layer_subset {
                writer
                    .write_bits::<3>(subset.temporal_id_min)
                    .and_then(|()| writer.write_bits::<5>(0b1_1111_u8))
                    .map_err(field_error("temporal_id_min"))?;
                writer
                    .write_bits::<3>(subset.temporal_id_max)
                    .and_then(|()| writer.write_bits::<5>(0b1_1111_u8))
                    .map_err(field_error("temporal_id_max"))?;
            }
            Ok(())
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for HevcVideoDescriptor {
    type Error = Error;

    /// Takes the fields of an HEVC video descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x38.
    /// * [`BodyLength`](Error::BodyLength): the body is not 13 bytes with
    ///   `temporal_layer_subset_flag` `'0'`, nor 15 bytes with it `'1'`.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = body_of(raw, DescriptorTag::HEVC_VIDEO)?;
        let temporal_layer_subset_flag = body
            .get(SUBSET_FLAG_INDEX)
            .is_some_and(|flags| flags & 0x80 != 0);
        // Why not take whichever length the body has:
        // temporal_layer_subset_flag fixes the length, so a body of the other
        // length contradicts the flag and is corrupt rather than a form to
        // read as it comes.
        if body.len() != body_length(temporal_layer_subset_flag) {
            return Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::HEVC_VIDEO,
                length: body.len(),
            });
        }

        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as
        // temporal_layer_subset_flag makes the fields.
        (|| {
            let profile_space = reader.read_u8::<2>()?;
            let tier_flag = reader.read_bool()?;
            let profile_idc = reader.read_u8::<5>()?;
            let profile_compatibility_indication = reader.read_u32::<32>()?;
            let progressive_source_flag = reader.read_bool()?;
            let interlaced_source_flag = reader.read_bool()?;
            let non_packed_constraint_flag = reader.read_bool()?;
            let frame_only_constraint_flag = reader.read_bool()?;
            let copied_44bits = reader.read_bits::<44>()?;
            let level_idc = reader.read_u8::<8>()?;
            reader.read_bool()?;
            let hevc_still_present_flag = reader.read_bool()?;
            let hevc_24hr_picture_present_flag = reader.read_bool()?;
            let sub_pic_hrd_params_not_present_flag = reader.read_bool()?;
            reader.read_bits::<2>()?;
            let hdr_wcg_idc = reader.read_u8::<2>()?;
            let temporal_layer_subset = if temporal_layer_subset_flag {
                let temporal_id_min = reader.read_u8::<3>()?;
                reader.read_bits::<5>()?;
                let temporal_id_max = reader.read_u8::<3>()?;
                Some(TemporalLayerSubset::new(temporal_id_min, temporal_id_max))
            } else {
                None
            };
            Ok(Self {
                profile_space,
                tier_flag,
                profile_idc,
                profile_compatibility_indication,
                progressive_source_flag,
                interlaced_source_flag,
                non_packed_constraint_flag,
                frame_only_constraint_flag,
                copied_44bits,
                level_idc,
                hevc_still_present_flag,
                hevc_24hr_picture_present_flag,
                sub_pic_hrd_params_not_present_flag,
                hdr_wcg_idc,
                temporal_layer_subset,
            })
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{HevcVideoDescriptor, TemporalLayerSubset};
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};
    #[cfg(feature = "alloc")]
    use crate::field;

    /// Main profile at level 3.1, progressive frames only, without the
    /// temporal layer subset
    const WITHOUT_SUBSET: [u8; 15] = [
        0x38, 0x0d, 0x01, 0x60, 0x00, 0x00, 0x00, 0xb0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5d, 0x1f,
    ];

    /// The same with `TemporalId` 0 to 2
    const WITH_SUBSET: [u8; 17] = [
        0x38, 0x0f, 0x01, 0x60, 0x00, 0x00, 0x00, 0xb0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5d, 0x9f,
        0x1f, 0x5f,
    ];

    fn main_at_level_3_1() -> HevcVideoDescriptor {
        let mut descriptor = HevcVideoDescriptor::new(1, 93);
        descriptor.profile_compatibility_indication = 0x6000_0000;
        descriptor.progressive_source_flag = true;
        descriptor.non_packed_constraint_flag = true;
        descriptor.frame_only_constraint_flag = true;
        descriptor.sub_pic_hrd_params_not_present_flag = true;
        descriptor
    }

    fn with_subset() -> HevcVideoDescriptor {
        let mut descriptor = main_at_level_3_1();
        descriptor.temporal_layer_subset = Some(TemporalLayerSubset::new(0, 2));
        descriptor
    }

    #[test]
    fn temporal_layer_subset_flag_decides_whether_the_temporal_ids_are_read() {
        assert_eq!(
            [WITHOUT_SUBSET.as_slice(), WITH_SUBSET.as_slice()]
                .map(|bytes| HevcVideoDescriptor::try_from(&RawDescriptor::read(bytes).unwrap())),
            [Ok(main_at_level_3_1()), Ok(with_subset())]
        );
    }

    #[test]
    fn a_body_whose_length_contradicts_temporal_layer_subset_flag_is_refused() {
        let flag_set_without_ids = [
            0x01, 0x60, 0x00, 0x00, 0x00, 0xb0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5d, 0x9f,
        ];
        let flag_clear_with_ids = [
            0x01, 0x60, 0x00, 0x00, 0x00, 0xb0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5d, 0x1f, 0x1f,
            0x5f,
        ];
        let body_length = |length| {
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::HEVC_VIDEO,
                length,
            })
        };

        assert_eq!(
            [
                flag_set_without_ids.as_slice(),
                flag_clear_with_ids.as_slice(),
                &[]
            ]
            .map(|body| HevcVideoDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::HEVC_VIDEO,
                body
            ))),
            [body_length(13), body_length(15), body_length(0)]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn both_forms_are_written_as_laid_out_by_hand() {
        let written = [main_at_level_3_1(), with_subset()].map(|descriptor| {
            let mut written = Vec::new();
            descriptor.write(&mut written).unwrap();
            written
        });

        assert_eq!(written, [WITHOUT_SUBSET.to_vec(), WITH_SUBSET.to_vec()]);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn descriptors_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let mut crossed = main_at_level_3_1();
        crossed.temporal_layer_subset = Some(TemporalLayerSubset::new(3, 2));
        let mut wide_idc = main_at_level_3_1();
        wide_idc.hdr_wcg_idc = 4;
        let mut wide_id = main_at_level_3_1();
        wide_id.temporal_layer_subset = Some(TemporalLayerSubset::new(0, 8));
        let mut written = Vec::new();

        assert_eq!(
            [crossed, wide_idc, wide_id].map(|descriptor| descriptor.write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "temporal_id_min",
                    value: 3
                }),
                Err(Error::Field {
                    field: "HDR_WCG_idc",
                    error: field::Error::ValueTooWide { width: 2, value: 4 }
                }),
                Err(Error::Field {
                    field: "temporal_id_max",
                    error: field::Error::ValueTooWide { width: 3, value: 8 }
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
