//! Stream id, which names the kind and number of the elementary stream a
//! PES packet carries

/// Kind and number of the elementary stream a PES packet carries
///
/// `stream_id`, ISO/IEC 13818-1 §2.4.3.7, Table 2-22
///
/// Every value has a meaning in Table 2-22, so any `u8` converts. The streams
/// the table names by a single value are associated constants; audio and
/// video take a range each, which [`is_audio`](Self::is_audio) and
/// [`is_video`](Self::is_video) test.
#[doc(alias = "stream_id")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamId(pub(crate) u8);

impl StreamId {
    /// `program_stream_map`
    pub const PROGRAM_STREAM_MAP: Self = Self(0xbc);
    /// `private_stream_1`
    pub const PRIVATE_STREAM_1: Self = Self(0xbd);
    /// `padding_stream`, whose packets carry `padding_byte`s
    pub const PADDING_STREAM: Self = Self(0xbe);
    /// `private_stream_2`
    pub const PRIVATE_STREAM_2: Self = Self(0xbf);
    /// `ECM_stream`
    pub const ECM_STREAM: Self = Self(0xf0);
    /// `EMM_stream`
    pub const EMM_STREAM: Self = Self(0xf1);
    /// `DSMCC_stream`
    pub const DSMCC_STREAM: Self = Self(0xf2);
    /// `ISO/IEC_13522_stream`
    pub const ISO_IEC_13522_STREAM: Self = Self(0xf3);
    /// Rec. ITU-T H.222.1 type A stream
    pub const H222_1_TYPE_A_STREAM: Self = Self(0xf4);
    /// Rec. ITU-T H.222.1 type B stream
    pub const H222_1_TYPE_B_STREAM: Self = Self(0xf5);
    /// Rec. ITU-T H.222.1 type C stream
    pub const H222_1_TYPE_C_STREAM: Self = Self(0xf6);
    /// Rec. ITU-T H.222.1 type D stream
    pub const H222_1_TYPE_D_STREAM: Self = Self(0xf7);
    /// Rec. ITU-T H.222.1 type E stream
    pub const H222_1_TYPE_E_STREAM: Self = Self(0xf8);
    /// `ancillary_stream`
    pub const ANCILLARY_STREAM: Self = Self(0xf9);
    /// `ISO/IEC 14496-1_SL-packetized_stream`
    pub const SL_PACKETIZED_STREAM: Self = Self(0xfa);
    /// `ISO/IEC 14496-1_M4Mux_stream`
    pub const M4MUX_STREAM: Self = Self(0xfb);
    /// metadata stream
    pub const METADATA_STREAM: Self = Self(0xfc);
    /// `extended_stream_id`: the stream is identified by `stream_id_extension`
    pub const EXTENDED_STREAM_ID: Self = Self(0xfd);
    /// `program_stream_directory`
    pub const PROGRAM_STREAM_DIRECTORY: Self = Self(0xff);

    /// Returns the 8-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns whether the `stream_id` is that of a PES packet: 0xBD
    /// (`private_stream_1`) and above, those below being the start codes of
    /// the Program Stream's own elements, §2.5.3.1 and Table 2-22
    #[must_use]
    pub const fn is_pes_packet(self) -> bool {
        self.0 >= Self::PRIVATE_STREAM_1.0
    }

    /// Returns whether the stream is an audio stream, `'110x xxxx'`
    #[must_use]
    pub const fn is_audio(self) -> bool {
        self.0 & 0xe0 == 0xc0
    }

    /// Returns whether the stream is a video stream, `'1110 xxxx'`
    #[must_use]
    pub const fn is_video(self) -> bool {
        self.0 & 0xf0 == 0xe0
    }

    /// Returns whether packets of the stream carry the optional PES header
    ///
    /// All do except those of `program_stream_map`, `padding_stream`,
    /// `private_stream_2`, `ECM_stream`, `EMM_stream`,
    /// `program_stream_directory`, `DSMCC_stream` and the H.222.1 type E
    /// stream, whose bytes follow `PES_packet_length` directly.
    #[must_use]
    pub const fn carries_optional_header(self) -> bool {
        !matches!(
            self,
            Self::PROGRAM_STREAM_MAP
                | Self::PADDING_STREAM
                | Self::PRIVATE_STREAM_2
                | Self::ECM_STREAM
                | Self::EMM_STREAM
                | Self::PROGRAM_STREAM_DIRECTORY
                | Self::DSMCC_STREAM
                | Self::H222_1_TYPE_E_STREAM
        )
    }
}

impl From<u8> for StreamId {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::StreamId;

    #[test]
    fn a_pes_packet_has_a_stream_id_of_0xbd_or_more() {
        assert_eq!(
            [0x00, 0xbc, 0xbd, 0xff].map(|value| StreamId(value).is_pes_packet()),
            [false, false, true, true]
        );
    }
}
