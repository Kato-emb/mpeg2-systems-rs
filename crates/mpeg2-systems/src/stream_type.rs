//! Kind of program element a stream carries, shared by the program map table
//! and the program stream map

/// Kind of program element a stream carries
///
/// `stream_type`, ISO/IEC 13818-1 §2.4.4.10, Table 2-34
///
/// Every value has a meaning in Table 2-34, so any `u8` converts. The values
/// the table assigns are associated constants, named after the words of the
/// table; the values it leaves reserved have none, and `0x80` to `0xFF` are
/// user private, which [`is_user_private`](Self::is_user_private) tests.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::stream_type::StreamType;
///
/// assert_eq!(StreamType::from(0x1b), StreamType::AVC_VIDEO);
/// assert!(StreamType::from(0x80).is_user_private());
/// ```
#[doc(alias = "stream_type")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamType(u8);

impl StreamType {
    /// ISO/IEC 11172-2 Video
    pub const ISO_IEC_11172_2_VIDEO: Self = Self(0x01);
    /// Rec. ITU-T H.262 | ISO/IEC 13818-2 Video, or ISO/IEC 11172-2
    /// constrained parameter video stream
    pub const ISO_IEC_13818_2_VIDEO: Self = Self(0x02);
    /// ISO/IEC 11172-3 Audio
    pub const ISO_IEC_11172_3_AUDIO: Self = Self(0x03);
    /// ISO/IEC 13818-3 Audio
    pub const ISO_IEC_13818_3_AUDIO: Self = Self(0x04);
    /// Rec. ITU-T H.222.0 | ISO/IEC 13818-1 `private_sections`
    pub const ISO_IEC_13818_1_PRIVATE_SECTIONS: Self = Self(0x05);
    /// Rec. ITU-T H.222.0 | ISO/IEC 13818-1 PES packets containing private
    /// data
    pub const ISO_IEC_13818_1_PES_PRIVATE_DATA: Self = Self(0x06);
    /// ISO/IEC 13522 MHEG
    pub const ISO_IEC_13522_MHEG: Self = Self(0x07);
    /// Rec. ITU-T H.222.0 | ISO/IEC 13818-1 Annex A DSM-CC
    pub const ISO_IEC_13818_1_DSM_CC: Self = Self(0x08);
    /// Rec. ITU-T H.222.1
    pub const ITU_T_H222_1: Self = Self(0x09);
    /// ISO/IEC 13818-6 type A
    pub const ISO_IEC_13818_6_TYPE_A: Self = Self(0x0a);
    /// ISO/IEC 13818-6 type B
    pub const ISO_IEC_13818_6_TYPE_B: Self = Self(0x0b);
    /// ISO/IEC 13818-6 type C
    pub const ISO_IEC_13818_6_TYPE_C: Self = Self(0x0c);
    /// ISO/IEC 13818-6 type D
    pub const ISO_IEC_13818_6_TYPE_D: Self = Self(0x0d);
    /// Rec. ITU-T H.222.0 | ISO/IEC 13818-1 auxiliary
    pub const ISO_IEC_13818_1_AUXILIARY: Self = Self(0x0e);
    /// ISO/IEC 13818-7 Audio with ADTS transport syntax
    pub const ISO_IEC_13818_7_AUDIO: Self = Self(0x0f);
    /// ISO/IEC 14496-2 Visual
    pub const ISO_IEC_14496_2_VISUAL: Self = Self(0x10);
    /// ISO/IEC 14496-3 Audio with the LATM transport syntax
    pub const ISO_IEC_14496_3_AUDIO_LATM: Self = Self(0x11);
    /// ISO/IEC 14496-1 SL-packetized stream or `M4Mux` stream carried in PES
    /// packets
    pub const ISO_IEC_14496_1_IN_PES: Self = Self(0x12);
    /// ISO/IEC 14496-1 SL-packetized stream or `M4Mux` stream carried in
    /// `ISO_IEC_14496_sections`
    pub const ISO_IEC_14496_1_IN_SECTIONS: Self = Self(0x13);
    /// ISO/IEC 13818-6 Synchronized Download Protocol
    pub const ISO_IEC_13818_6_SYNCHRONIZED_DOWNLOAD: Self = Self(0x14);
    /// Metadata carried in PES packets
    pub const METADATA_IN_PES: Self = Self(0x15);
    /// Metadata carried in `metadata_sections`
    pub const METADATA_IN_METADATA_SECTIONS: Self = Self(0x16);
    /// Metadata carried in ISO/IEC 13818-6 Data Carousel
    pub const METADATA_IN_DATA_CAROUSEL: Self = Self(0x17);
    /// Metadata carried in ISO/IEC 13818-6 Object Carousel
    pub const METADATA_IN_OBJECT_CAROUSEL: Self = Self(0x18);
    /// Metadata carried in ISO/IEC 13818-6 Synchronized Download Protocol
    pub const METADATA_IN_SYNCHRONIZED_DOWNLOAD: Self = Self(0x19);
    /// IPMP stream defined in ISO/IEC 13818-11, MPEG-2 IPMP
    pub const ISO_IEC_13818_11_IPMP: Self = Self(0x1a);
    /// AVC video stream, Rec. ITU-T H.264 | ISO/IEC 14496-10 Annex A, or an
    /// AVC video sub-bitstream
    pub const AVC_VIDEO: Self = Self(0x1b);
    /// ISO/IEC 14496-3 Audio without any additional transport syntax
    pub const ISO_IEC_14496_3_AUDIO: Self = Self(0x1c);
    /// ISO/IEC 14496-17 Text
    pub const ISO_IEC_14496_17_TEXT: Self = Self(0x1d);
    /// Auxiliary video stream as defined in ISO/IEC 23002-3
    pub const ISO_IEC_23002_3_AUXILIARY_VIDEO: Self = Self(0x1e);
    /// SVC video sub-bitstream of an AVC video stream
    pub const SVC_VIDEO_SUB_BITSTREAM: Self = Self(0x1f);
    /// MVC video sub-bitstream of an AVC video stream
    pub const MVC_VIDEO_SUB_BITSTREAM: Self = Self(0x20);
    /// Video stream of Rec. ITU-T T.800 | ISO/IEC 15444-1
    pub const ISO_IEC_15444_1_VIDEO: Self = Self(0x21);
    /// Additional view Rec. ITU-T H.262 | ISO/IEC 13818-2 video stream for
    /// service-compatible stereoscopic 3D services
    pub const ISO_IEC_13818_2_ADDITIONAL_VIEW_VIDEO: Self = Self(0x22);
    /// Additional view Rec. ITU-T H.264 | ISO/IEC 14496-10 video stream for
    /// service-compatible stereoscopic 3D services
    pub const AVC_ADDITIONAL_VIEW_VIDEO: Self = Self(0x23);
    /// Rec. ITU-T H.265 | ISO/IEC 23008-2 video stream, or an HEVC temporal
    /// video sub-bitstream
    pub const HEVC_VIDEO: Self = Self(0x24);
    /// HEVC temporal video subset of an HEVC video stream
    pub const HEVC_TEMPORAL_VIDEO_SUBSET: Self = Self(0x25);
    /// MVCD video sub-bitstream of an AVC video stream
    pub const MVCD_VIDEO_SUB_BITSTREAM: Self = Self(0x26);
    /// Timeline and External Media Information Stream (Annex U)
    pub const TIMELINE_AND_EXTERNAL_MEDIA_INFORMATION: Self = Self(0x27);
    /// HEVC enhancement sub-partition which includes `TemporalId` 0, Rec.
    /// ITU-T H.265 | ISO/IEC 23008-2 Annex G
    pub const HEVC_ANNEX_G_ENHANCEMENT_SUB_PARTITION: Self = Self(0x28);
    /// HEVC temporal enhancement sub-partition, Rec. ITU-T H.265 | ISO/IEC
    /// 23008-2 Annex G
    pub const HEVC_ANNEX_G_TEMPORAL_ENHANCEMENT_SUB_PARTITION: Self = Self(0x29);
    /// HEVC enhancement sub-partition which includes `TemporalId` 0, Rec.
    /// ITU-T H.265 | ISO/IEC 23008-2 Annex H
    pub const HEVC_ANNEX_H_ENHANCEMENT_SUB_PARTITION: Self = Self(0x2a);
    /// HEVC temporal enhancement sub-partition, Rec. ITU-T H.265 | ISO/IEC
    /// 23008-2 Annex H
    pub const HEVC_ANNEX_H_TEMPORAL_ENHANCEMENT_SUB_PARTITION: Self = Self(0x2b);
    /// Green access units carried in MPEG-2 sections
    pub const GREEN_ACCESS_UNITS_IN_SECTIONS: Self = Self(0x2c);
    /// ISO/IEC 23008-3 Audio with MHAS transport syntax, main stream
    pub const ISO_IEC_23008_3_MHAS_MAIN_AUDIO: Self = Self(0x2d);
    /// ISO/IEC 23008-3 Audio with MHAS transport syntax, auxiliary stream
    pub const ISO_IEC_23008_3_MHAS_AUXILIARY_AUDIO: Self = Self(0x2e);
    /// Quality access units carried in sections
    pub const QUALITY_ACCESS_UNITS_IN_SECTIONS: Self = Self(0x2f);
    /// Media Orchestration Access Units carried in sections
    pub const MEDIA_ORCHESTRATION_ACCESS_UNITS_IN_SECTIONS: Self = Self(0x30);
    /// Substream of an HEVC video stream that contains a Motion Constrained
    /// Tile Set, parameter sets, slice headers or a combination thereof
    pub const HEVC_MOTION_CONSTRAINED_TILE_SET_SUBSTREAM: Self = Self(0x31);
    /// JPEG XS video stream, ISO/IEC 21122-2
    pub const JPEG_XS_VIDEO: Self = Self(0x32);
    /// VVC video stream or a VVC temporal video sub-bitstream, Rec. ITU-T
    /// H.266 | ISO/IEC 23090-3
    pub const VVC_VIDEO: Self = Self(0x33);
    /// VVC temporal video subset of a VVC video stream
    pub const VVC_TEMPORAL_VIDEO_SUBSET: Self = Self(0x34);
    /// EVC video stream or an EVC temporal video sub-bitstream, ISO/IEC
    /// 23094-1
    pub const EVC_VIDEO: Self = Self(0x35);
    /// LCEVC video stream, ISO/IEC 23094-2
    pub const LCEVC_VIDEO: Self = Self(0x36);
    /// IPMP stream
    pub const IPMP: Self = Self(0x7f);

    /// Returns the 8-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns whether the stream type is user private, `0x80` to `0xFF`
    #[must_use]
    pub const fn is_user_private(self) -> bool {
        self.0 >= 0x80
    }

    /// Returns whether Table 2-34 names the stream type as carried in sections
    ///
    /// These are 0x05, 0x13, 0x16, 0x2C, 0x2F and 0x30.
    #[must_use]
    pub const fn is_carried_in_sections(self) -> bool {
        matches!(
            self,
            Self::ISO_IEC_13818_1_PRIVATE_SECTIONS
                | Self::ISO_IEC_14496_1_IN_SECTIONS
                | Self::METADATA_IN_METADATA_SECTIONS
                | Self::GREEN_ACCESS_UNITS_IN_SECTIONS
                | Self::QUALITY_ACCESS_UNITS_IN_SECTIONS
                | Self::MEDIA_ORCHESTRATION_ACCESS_UNITS_IN_SECTIONS
        )
    }

    /// Returns whether Table 2-34 gives the stream type to a video stream of
    /// a coding, whether or not the payload is a sub-bitstream, rather than
    /// to an additional view, an enhancement or another part of one
    #[cfg(feature = "alloc")]
    pub(crate) const fn is_video(self) -> bool {
        matches!(
            self,
            Self::ISO_IEC_11172_2_VIDEO
                | Self::ISO_IEC_13818_2_VIDEO
                | Self::ISO_IEC_14496_2_VISUAL
                | Self::AVC_VIDEO
                | Self::ISO_IEC_15444_1_VIDEO
                | Self::HEVC_VIDEO
                | Self::JPEG_XS_VIDEO
                | Self::VVC_VIDEO
                | Self::EVC_VIDEO
        )
    }
}

impl From<u8> for StreamType {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::StreamType;

    #[test]
    fn only_the_types_table_2_34_names_as_in_sections_are_carried_in_sections() {
        let carried_in_sections = |value: u8| StreamType::from(value).is_carried_in_sections();

        assert_eq!(
            (
                [0x05, 0x13, 0x16, 0x2c, 0x2f, 0x30].map(carried_in_sections),
                [0x02, 0x0a, 0x14, 0x15].map(carried_in_sections),
            ),
            ([true; 6], [false; 4])
        );
    }
}
