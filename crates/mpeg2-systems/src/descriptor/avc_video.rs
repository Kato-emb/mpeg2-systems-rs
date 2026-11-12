//! AVC video descriptor, which identifies the profile and level of an AVC
//! video stream

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::typed::write_descriptor;
use super::typed::{field_error, fixed_body};
use super::{DescriptorTag, Error, RawDescriptor};
use crate::field::FieldReader;

/// Bytes of the body
const BODY_LENGTH: usize = 4;

/// AVC video descriptor, which identifies the profile and level of an AVC
/// video stream
///
/// `AVC_video_descriptor`, ISO/IEC 13818-1 §2.6.64, Table 2-92, semantics
/// §2.6.65
///
/// `profile_idc`, the constraint set flags, `AVC_compatible_flags` and
/// `level_idc` are coded as in Rec. ITU-T H.264 | ISO/IEC 14496-10; the
/// crate does not check them against it.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::avc_video::AvcVideoDescriptor;
///
/// // High profile at level 4.0, no frame packing SEI
/// let bytes = [0x28, 0x04, 0x64, 0x4c, 0x28, 0x3f];
/// let descriptor = AvcVideoDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!((descriptor.profile_idc, descriptor.level_idc), (100, 40));
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
#[doc(alias = "AVC_video_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the syntax table has these flags, and each is a field"
)]
pub struct AvcVideoDescriptor {
    /// Profile the whole stream conforms to
    pub profile_idc: u8,
    /// `constraint_set0_flag` of Rec. ITU-T H.264 | ISO/IEC 14496-10
    pub constraint_set0_flag: bool,
    /// `constraint_set1_flag` of Rec. ITU-T H.264 | ISO/IEC 14496-10
    pub constraint_set1_flag: bool,
    /// `constraint_set2_flag` of Rec. ITU-T H.264 | ISO/IEC 14496-10
    pub constraint_set2_flag: bool,
    /// `constraint_set3_flag` of Rec. ITU-T H.264 | ISO/IEC 14496-10
    pub constraint_set3_flag: bool,
    /// `constraint_set4_flag` of Rec. ITU-T H.264 | ISO/IEC 14496-10
    pub constraint_set4_flag: bool,
    /// `constraint_set5_flag` of Rec. ITU-T H.264 | ISO/IEC 14496-10
    pub constraint_set5_flag: bool,
    /// The 2 bits between `constraint_set5_flag` and `level_idc` in the
    /// sequence parameter set
    pub avc_compatible_flags: u8,
    /// Level the whole stream conforms to
    pub level_idc: u8,
    /// Whether the stream may include AVC still pictures
    pub avc_still_present: bool,
    /// Whether the stream may include AVC 24-hour pictures
    pub avc_24_hour_picture_flag: bool,
    /// `'0'` when the stream contains the frame packing arrangement or the
    /// stereo video information SEI message
    pub frame_packing_sei_not_present_flag: bool,
}

impl AvcVideoDescriptor {
    /// Creates the descriptor of `profile_idc` and `level_idc`, every flag
    /// clear but `Frame_Packing_SEI_not_present_flag`, which leaves the SEI
    /// messages unspecified
    #[must_use]
    pub const fn new(profile_idc: u8, level_idc: u8) -> Self {
        Self {
            profile_idc,
            constraint_set0_flag: false,
            constraint_set1_flag: false,
            constraint_set2_flag: false,
            constraint_set3_flag: false,
            constraint_set4_flag: false,
            constraint_set5_flag: false,
            avc_compatible_flags: 0,
            level_idc,
            avc_still_present: false,
            avc_24_hour_picture_flag: false,
            frame_packing_sei_not_present_flag: true,
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        super::raw::HEADER_LENGTH.saturating_add(BODY_LENGTH)
    }

    /// Appends the descriptor to `out`, its `reserved` bits all `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`Field`](Error::Field): `avc_compatible_flags` wider than 2 bits.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write_descriptor(out, DescriptorTag::AVC_VIDEO, BODY_LENGTH, |writer| {
            (|| {
                writer.write_bits::<8>(self.profile_idc)?;
                for flag in [
                    self.constraint_set0_flag,
                    self.constraint_set1_flag,
                    self.constraint_set2_flag,
                    self.constraint_set3_flag,
                    self.constraint_set4_flag,
                    self.constraint_set5_flag,
                ] {
                    writer.write_bool(flag)?;
                }
                Ok(())
            })()
            // Why not an error per field: every value fits its field's width.
            .map_err(field_error("descriptor body"))?;
            writer
                .write_bits::<2>(self.avc_compatible_flags)
                .map_err(field_error("AVC_compatible_flags"))?;
            (|| {
                writer.write_bits::<8>(self.level_idc)?;
                writer.write_bool(self.avc_still_present)?;
                writer.write_bool(self.avc_24_hour_picture_flag)?;
                writer.write_bool(self.frame_packing_sei_not_present_flag)?;
                writer.write_bits::<5>(0b1_1111_u8)
            })()
            // Why not an error per field: every value fits its field's width.
            .map_err(field_error("descriptor body"))
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for AvcVideoDescriptor {
    type Error = Error;

    /// Takes the fields of an AVC video descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x28.
    /// * [`BodyLength`](Error::BodyLength): the body is not 4 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::AVC_VIDEO)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            Ok(Self {
                profile_idc: reader.read_u8::<8>()?,
                constraint_set0_flag: reader.read_bool()?,
                constraint_set1_flag: reader.read_bool()?,
                constraint_set2_flag: reader.read_bool()?,
                constraint_set3_flag: reader.read_bool()?,
                constraint_set4_flag: reader.read_bool()?,
                constraint_set5_flag: reader.read_bool()?,
                avc_compatible_flags: reader.read_u8::<2>()?,
                level_idc: reader.read_u8::<8>()?,
                avc_still_present: reader.read_bool()?,
                avc_24_hour_picture_flag: reader.read_bool()?,
                frame_packing_sei_not_present_flag: reader.read_bool()?,
            })
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::AvcVideoDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};
    #[cfg(feature = "alloc")]
    use crate::field;

    #[cfg(feature = "alloc")]
    #[test]
    fn reserved_bits_read_as_0_are_written_as_1() {
        let descriptor = AvcVideoDescriptor::try_from(
            &RawDescriptor::read(&[0x28, 0x04, 0x64, 0x4c, 0x28, 0x20]).unwrap(),
        )
        .unwrap();
        let mut written = Vec::new();
        descriptor.write(&mut written).unwrap();

        assert_eq!(written, [0x28, 0x04, 0x64, 0x4c, 0x28, 0x3f]);
    }

    #[test]
    fn a_body_of_other_than_4_bytes_is_refused() {
        assert_eq!(
            AvcVideoDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::AVC_VIDEO,
                &[0x64, 0x4c, 0x28]
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::AVC_VIDEO,
                length: 3
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn avc_compatible_flags_wider_than_2_bits_are_refused_before_a_byte_is_written() {
        let mut descriptor = AvcVideoDescriptor::new(100, 40);
        descriptor.avc_compatible_flags = 4;
        let mut written = Vec::new();

        assert_eq!(
            descriptor.write(&mut written),
            Err(Error::Field {
                field: "AVC_compatible_flags",
                error: field::Error::ValueTooWide { width: 2, value: 4 }
            })
        );
        assert_eq!(written, []);
    }
}
