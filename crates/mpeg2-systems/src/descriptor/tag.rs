//! Kind of descriptor, which names the syntax of its body

/// Kind of descriptor, which names the syntax of its body
///
/// `descriptor_tag`, ISO/IEC 13818-1 §2.6.1, Table 2-45
///
/// Every value has a meaning in Table 2-45, so any `u8` converts. The tags
/// the table names are associated constants; 0x13 to 0x1A, defined in
/// ISO/IEC 13818-6, and the reserved tags have none, and `0x40` to `0xFF`
/// are user private, which [`is_user_private`](Self::is_user_private) tests.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::DescriptorTag;
///
/// assert_eq!(DescriptorTag::from(0x0a), DescriptorTag::ISO_639_LANGUAGE);
/// assert!(DescriptorTag::from(0x52).is_user_private());
/// ```
#[doc(alias = "descriptor_tag")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DescriptorTag(u8);

impl DescriptorTag {
    /// Forbidden: in a program stream map, a pseudo descriptor of this tag carries `elementary_stream_id_extension`
    pub const FORBIDDEN: Self = Self(0x01);
    /// `video_stream_descriptor`
    pub const VIDEO_STREAM: Self = Self(0x02);
    /// `audio_stream_descriptor`
    pub const AUDIO_STREAM: Self = Self(0x03);
    /// `hierarchy_descriptor`
    pub const HIERARCHY: Self = Self(0x04);
    /// `registration_descriptor`
    pub const REGISTRATION: Self = Self(0x05);
    /// `data_stream_alignment_descriptor`
    pub const DATA_STREAM_ALIGNMENT: Self = Self(0x06);
    /// `target_background_grid_descriptor`
    pub const TARGET_BACKGROUND_GRID: Self = Self(0x07);
    /// `video_window_descriptor`
    pub const VIDEO_WINDOW: Self = Self(0x08);
    /// `CA_descriptor`
    pub const CA: Self = Self(0x09);
    /// `ISO_639_language_descriptor`
    pub const ISO_639_LANGUAGE: Self = Self(0x0a);
    /// `system_clock_descriptor`
    pub const SYSTEM_CLOCK: Self = Self(0x0b);
    /// `multiplex_buffer_utilization_descriptor`
    pub const MULTIPLEX_BUFFER_UTILIZATION: Self = Self(0x0c);
    /// `copyright_descriptor`
    pub const COPYRIGHT: Self = Self(0x0d);
    /// `maximum_bitrate_descriptor`
    pub const MAXIMUM_BITRATE: Self = Self(0x0e);
    /// `private_data_indicator_descriptor`
    pub const PRIVATE_DATA_INDICATOR: Self = Self(0x0f);
    /// `smoothing_buffer_descriptor`
    pub const SMOOTHING_BUFFER: Self = Self(0x10);
    /// `STD_descriptor`
    pub const STD: Self = Self(0x11);
    /// `IBP_descriptor`
    pub const IBP: Self = Self(0x12);
    /// `MPEG-4_video_descriptor`
    pub const MPEG_4_VIDEO: Self = Self(0x1b);
    /// `MPEG-4_audio_descriptor`
    pub const MPEG_4_AUDIO: Self = Self(0x1c);
    /// `IOD_descriptor`
    pub const IOD: Self = Self(0x1d);
    /// `SL_descriptor`
    pub const SL: Self = Self(0x1e);
    /// `FMC_descriptor`
    pub const FMC: Self = Self(0x1f);
    /// `external_ES_ID_descriptor`
    pub const EXTERNAL_ES_ID: Self = Self(0x20);
    /// `MuxCode_descriptor`
    pub const MUX_CODE: Self = Self(0x21);
    /// `M4MuxBufferSize_descriptor`
    pub const M4_MUX_BUFFER_SIZE: Self = Self(0x22);
    /// `multiplexBuffer_descriptor`
    pub const MULTIPLEX_BUFFER: Self = Self(0x23);
    /// `content_labeling_descriptor`
    pub const CONTENT_LABELING: Self = Self(0x24);
    /// `metadata_pointer_descriptor`
    pub const METADATA_POINTER: Self = Self(0x25);
    /// `metadata_descriptor`
    pub const METADATA: Self = Self(0x26);
    /// `metadata_STD_descriptor`
    pub const METADATA_STD: Self = Self(0x27);
    /// AVC video descriptor
    pub const AVC_VIDEO: Self = Self(0x28);
    /// `IPMP_descriptor`, defined in ISO/IEC 13818-11
    pub const IPMP: Self = Self(0x29);
    /// AVC timing and HRD descriptor
    pub const AVC_TIMING_AND_HRD: Self = Self(0x2a);
    /// `MPEG-2_AAC_audio_descriptor`
    pub const MPEG_2_AAC_AUDIO: Self = Self(0x2b);
    /// `M4MuxTiming_descriptor`
    pub const M4_MUX_TIMING: Self = Self(0x2c);
    /// `MPEG-4_text_descriptor`
    pub const MPEG_4_TEXT: Self = Self(0x2d);
    /// `MPEG-4_audio_extension_descriptor`
    pub const MPEG_4_AUDIO_EXTENSION: Self = Self(0x2e);
    /// `Auxiliary_video_stream_descriptor`
    pub const AUXILIARY_VIDEO_STREAM: Self = Self(0x2f);
    /// SVC extension descriptor
    pub const SVC_EXTENSION: Self = Self(0x30);
    /// MVC extension descriptor
    pub const MVC_EXTENSION: Self = Self(0x31);
    /// J2K video descriptor
    pub const J2K_VIDEO: Self = Self(0x32);
    /// MVC operation point descriptor
    pub const MVC_OPERATION_POINT: Self = Self(0x33);
    /// `MPEG2_stereoscopic_video_format_descriptor`
    pub const MPEG2_STEREOSCOPIC_VIDEO_FORMAT: Self = Self(0x34);
    /// `Stereoscopic_program_info_descriptor`
    pub const STEREOSCOPIC_PROGRAM_INFO: Self = Self(0x35);
    /// `Stereoscopic_video_info_descriptor`
    pub const STEREOSCOPIC_VIDEO_INFO: Self = Self(0x36);
    /// `Transport_profile_descriptor`
    pub const TRANSPORT_PROFILE: Self = Self(0x37);
    /// HEVC video descriptor
    pub const HEVC_VIDEO: Self = Self(0x38);
    /// VVC video descriptor
    pub const VVC_VIDEO: Self = Self(0x39);
    /// EVC video descriptor
    pub const EVC_VIDEO: Self = Self(0x3a);
    /// `Extension_descriptor`
    pub const EXTENSION: Self = Self(0x3f);

    /// Returns the 8-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns whether the tag is user private, `0x40` to `0xFF`
    #[must_use]
    pub const fn is_user_private(self) -> bool {
        self.0 >= 0x40
    }
}

impl From<u8> for DescriptorTag {
    fn from(value: u8) -> Self {
        Self(value)
    }
}
