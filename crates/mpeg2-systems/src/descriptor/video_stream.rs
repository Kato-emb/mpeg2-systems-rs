//! Video stream descriptor, which identifies the coding parameters of an
//! ISO/IEC 11172-2 or Rec. ITU-T H.262 | ISO/IEC 13818-2 video stream

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::write_descriptor;
use super::{DescriptorTag, Error, RawDescriptor, body_of, field_error};
use crate::field::{FieldReader, FourBitFieldOutOfRange, TwoBitFieldOutOfRange};

/// Bytes of the body when `MPEG_1_only_flag` is `'1'`
const MPEG_1_BODY_LENGTH: usize = 1;

/// Bytes of the body when `MPEG_1_only_flag` is `'0'`
const MPEG_2_BODY_LENGTH: usize = 3;

/// Returns the bytes of the body `MPEG_1_only_flag` makes
const fn body_length(mpeg_1_only_flag: bool) -> usize {
    if mpeg_1_only_flag {
        MPEG_1_BODY_LENGTH
    } else {
        MPEG_2_BODY_LENGTH
    }
}

/// Frame rate of a video stream
///
/// `frame_rate_code`, defined in Rec. ITU-T H.262 | ISO/IEC 13818-2 §6.3.3,
/// Table 6-4; ISO/IEC 13818-1 §2.6.3, Table 2-47, lets it include other
/// frame rates when `multiple_frame_rate_flag` is set
///
/// 0 is forbidden and 9 to 15 are reserved.
#[doc(alias = "frame_rate_code")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameRateCode(u8);

impl FrameRateCode {
    /// 24000 ÷ 1001, about 23.976
    pub const FRAME_RATE_23_976: Self = Self(1);
    /// 24
    pub const FRAME_RATE_24: Self = Self(2);
    /// 25
    pub const FRAME_RATE_25: Self = Self(3);
    /// 30000 ÷ 1001, about 29.97
    pub const FRAME_RATE_29_97: Self = Self(4);
    /// 30
    pub const FRAME_RATE_30: Self = Self(5);
    /// 50
    pub const FRAME_RATE_50: Self = Self(6);
    /// 60000 ÷ 1001, about 59.94
    pub const FRAME_RATE_59_94: Self = Self(7);
    /// 60
    pub const FRAME_RATE_60: Self = Self(8);

    /// Returns the 4-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for FrameRateCode {
    type Error = FourBitFieldOutOfRange;

    /// Takes a 4-bit value
    ///
    /// # Errors
    ///
    /// [`FourBitFieldOutOfRange`]: `value` is 16 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        FourBitFieldOutOfRange::check(value).map(Self)
    }
}

/// Sampling of the chrominance of a video stream
///
/// `chroma_format`, defined in Rec. ITU-T H.262 | ISO/IEC 13818-2 §6.3.5,
/// Table 6-5
///
/// `'00'` is reserved.
#[doc(alias = "chroma_format")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChromaFormat(u8);

impl ChromaFormat {
    /// 4:2:0
    pub const CHROMA_420: Self = Self(0b01);
    /// 4:2:2
    pub const CHROMA_422: Self = Self(0b10);
    /// 4:4:4
    pub const CHROMA_444: Self = Self(0b11);

    /// Returns the 2-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for ChromaFormat {
    type Error = TwoBitFieldOutOfRange;

    /// Takes a 2-bit value
    ///
    /// # Errors
    ///
    /// [`TwoBitFieldOutOfRange`]: `value` is 4 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        TwoBitFieldOutOfRange::check(value).map(Self)
    }
}

/// Profile and level of a video stream
///
/// `profile_and_level_indication`, coded as in Rec. ITU-T H.262 | ISO/IEC
/// 13818-2 §8, Tables 8-1 to 8-4
///
/// The first bit is the escape bit, then 3 bits of profile and 4 of level.
/// With the escape bit `'0'`, the profiles `'001'` (High) to `'101'`
/// (Simple) and the levels `'0010'` (`HighP`), `'0100'` (High), `'0110'`
/// (High 1440), `'1000'` (Main) and `'1010'` (Low) are assigned; with it
/// `'1'`, 0x82 and 0x85 (4:2:2 profile at High and Main Level) and 0x8A,
/// 0x8B, 0x8D and 0x8E (Multi-view profile at High, High 1440, Main and Low
/// Level). Every other value is reserved, which
/// [`is_reserved`](Self::is_reserved) tests.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::video_stream::ProfileAndLevelIndication;
///
/// // Main Profile at Main Level
/// let main_at_main = ProfileAndLevelIndication::from(0x48);
/// assert_eq!(
///     (main_at_main.escape_bit(), main_at_main.profile(), main_at_main.level()),
///     (false, 0b100, 0b1000)
/// );
/// ```
#[doc(alias = "profile_and_level_indication")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileAndLevelIndication(u8);

impl ProfileAndLevelIndication {
    /// Returns the 8-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns the first bit, which when set gives the other 7 bits a meaning
    /// of their own
    #[must_use]
    pub const fn escape_bit(self) -> bool {
        self.0 & 0x80 != 0
    }

    /// Returns the 3 bits of profile
    #[must_use]
    pub const fn profile(self) -> u8 {
        (self.0 & 0x70).wrapping_shr(4)
    }

    /// Returns the 4 bits of level
    #[must_use]
    pub const fn level(self) -> u8 {
        self.0 & 0x0f
    }

    /// Returns whether Rec. ITU-T H.262 | ISO/IEC 13818-2 reserves the value
    #[must_use]
    pub const fn is_reserved(self) -> bool {
        if self.escape_bit() {
            !matches!(self.0, 0x82 | 0x85 | 0x8a | 0x8b | 0x8d | 0x8e)
        } else {
            !matches!(self.profile(), 0b001..=0b101)
                || !matches!(self.level(), 0b0010 | 0b0100 | 0b0110 | 0b1000 | 0b1010)
        }
    }
}

impl From<u8> for ProfileAndLevelIndication {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

/// The fields of a [`VideoStreamDescriptor`] present when `MPEG_1_only_flag`
/// is `'0'`
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mpeg2Fields {
    /// A profile and level equal to or higher than those of any sequence
    pub profile_and_level_indication: ProfileAndLevelIndication,
    /// A chroma format equal to or higher than that of any sequence
    pub chroma_format: ChromaFormat,
    /// Whether a sequence has a non-zero `frame_rate_extension_n` or
    /// `frame_rate_extension_d`
    pub frame_rate_extension_flag: bool,
}

impl Mpeg2Fields {
    /// Creates the fields, `frame_rate_extension_flag` clear
    #[must_use]
    pub const fn new(
        profile_and_level_indication: ProfileAndLevelIndication,
        chroma_format: ChromaFormat,
    ) -> Self {
        Self {
            profile_and_level_indication,
            chroma_format,
            frame_rate_extension_flag: false,
        }
    }
}

/// Video stream descriptor, which identifies the coding parameters of an
/// ISO/IEC 11172-2 or Rec. ITU-T H.262 | ISO/IEC 13818-2 video stream
///
/// `video_stream_descriptor`, ISO/IEC 13818-1 §2.6.2, Table 2-46, semantics
/// §2.6.3
///
/// `MPEG_1_only_flag` follows from whether [`mpeg_2`](Self::mpeg_2) is
/// present.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::video_stream::{
///     ChromaFormat, FrameRateCode, Mpeg2Fields, ProfileAndLevelIndication, VideoStreamDescriptor,
/// };
///
/// // Main Profile at Main Level, 4:2:0, 29.97 frames a second
/// let bytes = [0x02, 0x03, 0x22, 0x48, 0x5f];
/// let descriptor = VideoStreamDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor.frame_rate_code, FrameRateCode::FRAME_RATE_29_97);
/// assert_eq!(
///     descriptor.mpeg_2,
///     Some(Mpeg2Fields::new(
///         ProfileAndLevelIndication::from(0x48),
///         ChromaFormat::CHROMA_420
///     ))
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
#[doc(alias = "video_stream_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoStreamDescriptor {
    /// Whether frame rates other than `frame_rate_code` may be present
    pub multiple_frame_rate_flag: bool,
    /// Frame rate of the stream
    pub frame_rate_code: FrameRateCode,
    /// Whether the stream has no unconstrained ISO/IEC 11172-2 video data
    pub constrained_parameter_flag: bool,
    /// Whether the stream has only still pictures
    pub still_picture_flag: bool,
    /// The fields present when the stream may hold Rec. ITU-T H.262 |
    /// ISO/IEC 13818-2 video data
    pub mpeg_2: Option<Mpeg2Fields>,
}

impl VideoStreamDescriptor {
    /// Creates the descriptor of an ISO/IEC 11172-2 only stream of
    /// `frame_rate_code`, every flag clear
    #[must_use]
    pub const fn new(frame_rate_code: FrameRateCode) -> Self {
        Self {
            multiple_frame_rate_flag: false,
            frame_rate_code,
            constrained_parameter_flag: false,
            still_picture_flag: false,
            mpeg_2: None,
        }
    }

    /// Returns whether the stream holds ISO/IEC 11172-2 data only
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element reads its derived fields from a reference"
    )]
    pub const fn mpeg_1_only_flag(&self) -> bool {
        self.mpeg_2.is_none()
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub const fn encoded_len(&self) -> usize {
        super::HEADER_LENGTH.saturating_add(body_length(self.mpeg_1_only_flag()))
    }

    /// Appends the descriptor to `out`, its `reserved` bits all `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`Forbidden`](Error::Forbidden): `frame_rate_code` is 0, forbidden,
    /// or 9 to 15, reserved; `profile_and_level_indication` is reserved
    /// ([`ProfileAndLevelIndication::is_reserved`]);
    /// `chroma_format` is `'00'`, reserved; or
    /// [`mpeg_2`](Self::mpeg_2) is present without
    /// `constrained_parameter_flag`, which §2.6.3 requires then.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let frame_rate_code = self.frame_rate_code.get();
        if !matches!(frame_rate_code, 1..=8) {
            return Err(Error::Forbidden {
                field: "frame_rate_code",
                value: u32::from(frame_rate_code),
            });
        }
        if let Some(mpeg_2) = self.mpeg_2 {
            let indication = mpeg_2.profile_and_level_indication;
            if indication.is_reserved() {
                return Err(Error::Forbidden {
                    field: "profile_and_level_indication",
                    value: u32::from(indication.get()),
                });
            }
            if mpeg_2.chroma_format.get() == 0 {
                return Err(Error::Forbidden {
                    field: "chroma_format",
                    value: 0,
                });
            }
            if !self.constrained_parameter_flag {
                return Err(Error::Forbidden {
                    field: "constrained_parameter_flag",
                    value: 0,
                });
            }
        }

        let body_length = body_length(self.mpeg_1_only_flag());
        write_descriptor(out, DescriptorTag::VIDEO_STREAM, body_length, |writer| {
            (|| {
                writer.write_bool(self.multiple_frame_rate_flag)?;
                writer.write_bits::<4>(frame_rate_code)?;
                writer.write_bool(self.mpeg_1_only_flag())?;
                writer.write_bool(self.constrained_parameter_flag)?;
                writer.write_bool(self.still_picture_flag)?;
                if let Some(mpeg_2) = self.mpeg_2 {
                    writer.write_bits::<8>(mpeg_2.profile_and_level_indication.get())?;
                    writer.write_bits::<2>(mpeg_2.chroma_format.get())?;
                    writer.write_bool(mpeg_2.frame_rate_extension_flag)?;
                    writer.write_bits::<5>(0b1_1111_u8)?;
                }
                Ok(())
            })()
            // Why not an error per field: every value fits its field's width.
            .map_err(field_error("descriptor body"))
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for VideoStreamDescriptor {
    type Error = Error;

    /// Takes the fields of a video stream descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x02.
    /// * [`BodyLength`](Error::BodyLength): the body is not 1 byte with
    ///   `MPEG_1_only_flag` `'1'`, nor 3 bytes with it `'0'`.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = body_of(raw, DescriptorTag::VIDEO_STREAM)?;
        let mpeg_1_only_flag = body.first().is_some_and(|flags| flags & 0b100 != 0);
        // Why not take whichever length the body has: MPEG_1_only_flag fixes
        // the length, so a body of the other length contradicts the flag and
        // is corrupt rather than a form to read as it comes.
        if body.len() != body_length(mpeg_1_only_flag) {
            return Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::VIDEO_STREAM,
                length: body.len(),
            });
        }

        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as MPEG_1_only_flag
        // makes the fields.
        (|| {
            let multiple_frame_rate_flag = reader.read_bool()?;
            let frame_rate_code = FrameRateCode(reader.read_u8::<4>()?);
            reader.read_bool()?;
            Ok(Self {
                multiple_frame_rate_flag,
                frame_rate_code,
                constrained_parameter_flag: reader.read_bool()?,
                still_picture_flag: reader.read_bool()?,
                mpeg_2: if mpeg_1_only_flag {
                    None
                } else {
                    Some(Mpeg2Fields {
                        profile_and_level_indication: ProfileAndLevelIndication(
                            reader.read_u8::<8>()?,
                        ),
                        chroma_format: ChromaFormat(reader.read_u8::<2>()?),
                        frame_rate_extension_flag: reader.read_bool()?,
                    })
                },
            })
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{
        ChromaFormat, FrameRateCode, Mpeg2Fields, ProfileAndLevelIndication, VideoStreamDescriptor,
    };
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    /// ISO/IEC 11172-2 only, 25 frames a second or more, still pictures
    const MPEG_1: [u8; 3] = [0x02, 0x01, 0x9d];

    /// Main Profile at Main Level, 4:2:0, 29.97 frames a second
    const MPEG_2: [u8; 5] = [0x02, 0x03, 0x22, 0x48, 0x5f];

    fn mpeg_1() -> VideoStreamDescriptor {
        let mut descriptor = VideoStreamDescriptor::new(FrameRateCode::FRAME_RATE_25);
        descriptor.multiple_frame_rate_flag = true;
        descriptor.still_picture_flag = true;
        descriptor
    }

    fn mpeg_2() -> VideoStreamDescriptor {
        let mut descriptor = VideoStreamDescriptor::new(FrameRateCode::FRAME_RATE_29_97);
        descriptor.constrained_parameter_flag = true;
        descriptor.mpeg_2 = Some(Mpeg2Fields::new(
            ProfileAndLevelIndication::from(0x48),
            ChromaFormat::CHROMA_420,
        ));
        descriptor
    }

    #[test]
    fn mpeg_1_only_flag_decides_whether_the_mpeg_2_fields_are_read() {
        assert_eq!(
            [MPEG_1.as_slice(), MPEG_2.as_slice()]
                .map(|bytes| VideoStreamDescriptor::try_from(&RawDescriptor::read(bytes).unwrap())),
            [Ok(mpeg_1()), Ok(mpeg_2())]
        );
    }

    #[test]
    fn a_body_whose_length_contradicts_mpeg_1_only_flag_is_refused() {
        let body_length = |length| {
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::VIDEO_STREAM,
                length,
            })
        };

        assert_eq!(
            [
                RawDescriptor::new(DescriptorTag::VIDEO_STREAM, &[0x22]),
                RawDescriptor::new(DescriptorTag::VIDEO_STREAM, &[0x9d, 0x48, 0x5f]),
                RawDescriptor::new(DescriptorTag::VIDEO_STREAM, &[]),
            ]
            .map(|raw| VideoStreamDescriptor::try_from(&raw)),
            [body_length(1), body_length(3), body_length(0)]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn both_forms_are_written_as_laid_out_by_hand() {
        let written = [mpeg_1(), mpeg_2()].map(|descriptor| {
            let mut written = Vec::new();
            descriptor.write(&mut written).unwrap();
            written
        });

        assert_eq!(written, [MPEG_1.to_vec(), MPEG_2.to_vec()]);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_descriptor_is_written_only_with_values_the_specification_allows() {
        let mut forbidden_rate = mpeg_1();
        forbidden_rate.frame_rate_code = FrameRateCode::try_from(0).unwrap();
        let mut reserved_rate = mpeg_1();
        reserved_rate.frame_rate_code = FrameRateCode::try_from(9).unwrap();
        let mut reserved_chroma = mpeg_2();
        reserved_chroma.mpeg_2 = Some(Mpeg2Fields::new(
            ProfileAndLevelIndication::from(0x48),
            ChromaFormat::try_from(0).unwrap(),
        ));
        let reserved_indication = |value| {
            let mut descriptor = mpeg_2();
            descriptor.mpeg_2 = Some(Mpeg2Fields::new(
                ProfileAndLevelIndication::from(value),
                ChromaFormat::CHROMA_420,
            ));
            descriptor
        };
        let mut unconstrained = mpeg_2();
        unconstrained.constrained_parameter_flag = false;
        let mut written = Vec::new();

        assert_eq!(
            [
                forbidden_rate,
                reserved_rate,
                reserved_chroma,
                unconstrained,
                reserved_indication(0x00),
                reserved_indication(0x08),
                reserved_indication(0x40),
                reserved_indication(0x68),
                reserved_indication(0x45),
                reserved_indication(0x80),
                reserved_indication(0x8f),
                reserved_indication(0x42),
                reserved_indication(0x85),
            ]
            .map(|descriptor| descriptor.write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "frame_rate_code",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "frame_rate_code",
                    value: 9
                }),
                Err(Error::Forbidden {
                    field: "chroma_format",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "constrained_parameter_flag",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "profile_and_level_indication",
                    value: 0x00
                }),
                Err(Error::Forbidden {
                    field: "profile_and_level_indication",
                    value: 0x08
                }),
                Err(Error::Forbidden {
                    field: "profile_and_level_indication",
                    value: 0x40
                }),
                Err(Error::Forbidden {
                    field: "profile_and_level_indication",
                    value: 0x68
                }),
                Err(Error::Forbidden {
                    field: "profile_and_level_indication",
                    value: 0x45
                }),
                Err(Error::Forbidden {
                    field: "profile_and_level_indication",
                    value: 0x80
                }),
                Err(Error::Forbidden {
                    field: "profile_and_level_indication",
                    value: 0x8f
                }),
                Ok(()),
                Ok(()),
            ]
        );
        assert_eq!(
            written,
            [0x02, 0x03, 0x22, 0x42, 0x5f, 0x02, 0x03, 0x22, 0x85, 0x5f]
        );
    }
}
