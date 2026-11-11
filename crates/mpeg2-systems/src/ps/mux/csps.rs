//! The bounds a constrained system parameter stream (CSPS) holds a Program
//! Stream to: the rate packets arrive at, and the P-STD buffer of each
//! elementary stream

use core::ops::RangeInclusive;

use crate::pes::StreamId;
use crate::stream_type::StreamType;
use crate::time::SYSTEM_CLOCK_FREQUENCY;

/// The bound a constrained system parameter stream (CSPS) puts on the P-STD
/// buffer of an elementary stream
///
/// "Decoder buffer size", ISO/IEC 13818-1 §2.7.9: the input buffer `BSn` of
/// the stream, which its `P-STD_buffer_size` gives (§2.4.3.7), is bounded by
/// what the stream is. Sizes are in bytes, and `Rvmax` in bits a second.
#[doc(alias("CSPS", "BSn"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecoderBufferBound {
    /// An H.262 or ISO/IEC 11172-2 video elementary stream, whose `BSn` is the
    /// size of its Video Buffer Verifier (VBV) and at most
    /// max[6 × 1024, `Rvmax` × 0.001 / 8] bytes more
    Vbv {
        /// The size of the VBV, in bytes
        size: u32,
        /// `Rvmax`, the maximum bit rate of the stream, in bits a second
        rv_max: u32,
    },
    /// An H.264 video elementary stream, whose `BSn` is `cpb_size`, the size of
    /// its CPB, and at most max[6 × 1024, `Rvmax` × 0.001 / 8] bytes more
    Cpb {
        /// `cpb_size`, the size of the CPB, in bytes
        cpb_size: u32,
        /// `Rvmax`, the maximum bit rate of the stream, in bits a second
        rv_max: u32,
    },
    /// An audio elementary stream, whose `BSn` is at most 4096 bytes
    Audio,
    /// An ISO/IEC 13818-7 ADTS audio elementary stream, whose `BSn` is at most
    /// 8976 bytes
    AdtsAudio,
    /// An elementary stream whose `BSn` §2.7.9 does not bound
    Unbounded,
}

/// The most bytes `BSadd` may be at the least, 6 × 1024 (§2.7.9)
const SHORTEST_LONGEST_BS_ADD: u32 = 6 * 1024;

/// The bits a second of `Rvmax` for each byte of `BSadd`: `Rvmax` × 0.001 / 8
/// (§2.7.9)
// Why not Rvmax × 0.001 bytes as the text has it: Rvmax is in bits a second,
// and the smaller bound of the two readings meets both.
const RV_MAX_PER_BS_ADD_BYTE: u32 = 8_000;

/// The most bytes `BSn` of an audio elementary stream may be (§2.7.9)
const LONGEST_AUDIO_BUFFER: u32 = 4_096;

/// The most bytes `BSn` of an ISO/IEC 13818-7 ADTS audio elementary stream may
/// be (§2.7.9)
const LONGEST_ADTS_AUDIO_BUFFER: u32 = 8_976;

/// The `packet_rate_restriction_flag` the muxer writes in a CSPS
const PACKET_RATE_RESTRICTION_FLAG: bool = false;

/// The bits a second past which the packet rate grows with `rate_bound`, at
/// [`PACKET_RATE_RESTRICTION_FLAG`] `'0'`: 2.5 × 10⁶ (§2.7.9 equation 2-28)
// Why not the 2 000 000 bit/s of the text: equation 2-28 has 2.5 × 10⁶, and
// the text names no equation that takes it.
const PACKET_RATE_KNEE: u64 = 2_500_000;

/// The packets a second a CSPS takes at or below [`PACKET_RATE_KNEE`]
/// (§2.7.9)
const PACKETS_PER_SECOND: u64 = 300;

/// The bits a second of `rate_bound` in a unit of it, 8 × 50 (§2.7.9
/// equation 2-29)
const RATE_BOUND_UNIT_BITS: u64 = 8 * 50;

/// The bounds of §2.7.9 a Program Stream is held to: those of a CSPS, or
/// none
#[derive(Debug, Clone, Copy)]
pub(super) struct Constraints {
    /// `rate_bound` of a CSPS, `None` where the stream is not one
    csps: Option<u32>,
}

impl Constraints {
    /// Returns no bounds, those of a stream that is not a CSPS
    pub(super) const fn none() -> Self {
        Self { csps: None }
    }

    /// Returns the bounds of a CSPS of `rate_bound`
    pub(super) const fn csps(rate_bound: u32) -> Self {
        Self {
            csps: Some(rate_bound),
        }
    }

    /// Returns `CSPS_flag` of the system header
    pub(super) const fn csps_flag(self) -> bool {
        self.csps.is_some()
    }

    /// Returns `packet_rate_restriction_flag` of the system header
    pub(super) const fn packet_rate_restriction_flag(self) -> bool {
        PACKET_RATE_RESTRICTION_FLAG
    }

    /// Returns the fewest ticks of the STC from the SCR of a pack of
    /// `packets` packets to the next SCR, rounded up (§2.7.9 equation 2-28):
    /// 0 where the stream is not a CSPS
    pub(super) fn packet_ticks(self, packets: usize) -> u64 {
        let Some(rate_bound) = self.csps else {
            return 0;
        };
        // NP × 27 MHz / (300 × max[1, R_max / X]), as one fraction
        let rate = u128::from(RATE_BOUND_UNIT_BITS.saturating_mul(u64::from(rate_bound)));
        let ticks = u128::try_from(packets)
            .unwrap_or(u128::MAX)
            .saturating_mul(u128::from(SYSTEM_CLOCK_FREQUENCY))
            .saturating_mul(u128::from(PACKET_RATE_KNEE))
            .div_ceil(
                u128::from(PACKETS_PER_SECOND).saturating_mul(rate.max(PACKET_RATE_KNEE.into())),
            );
        u64::try_from(ticks).unwrap_or(u64::MAX)
    }

    /// Returns the bytes `BSn` of the stream of `stream_id` and `stream_type`
    /// may be in, `declared` its bound where the caller has given it, or
    /// `None` where nothing bounds it
    ///
    /// # Panics
    ///
    /// Panics if the stream is a CSPS and `declared` is `None` where
    /// [`class`] cannot tell the bound alone.
    pub(super) fn buffer_sizes(
        self,
        stream_id: StreamId,
        stream_type: StreamType,
        declared: Option<DecoderBufferBound>,
    ) -> Option<RangeInclusive<u32>> {
        self.csps?;
        let bound = match class(stream_id, stream_type) {
            Class::Known(bound) => bound,
            Class::Vbv | Class::Cpb | Class::SubBitstream | Class::Undecided => {
                #[expect(
                    clippy::panic,
                    reason = "a BSn written before its bound is declared is the caller's mistake"
                )]
                let Some(bound) = declared else {
                    panic!(
                        "a P-STD buffer size of stream_id {:#04x} is written in a CSPS before \
                         set_decoder_buffer_bound",
                        stream_id.get()
                    );
                };
                bound
            }
        };
        match bound {
            DecoderBufferBound::Vbv { size: vbv, rv_max }
            | DecoderBufferBound::Cpb {
                cpb_size: vbv,
                rv_max,
            } => {
                let bs_add = SHORTEST_LONGEST_BS_ADD.max(rv_max / RV_MAX_PER_BS_ADD_BYTE);
                Some(vbv..=vbv.saturating_add(bs_add))
            }
            DecoderBufferBound::Audio => Some(0..=LONGEST_AUDIO_BUFFER),
            DecoderBufferBound::AdtsAudio => Some(0..=LONGEST_ADTS_AUDIO_BUFFER),
            DecoderBufferBound::Unbounded => None,
        }
    }
}

/// What the `stream_id` and the `stream_type` of a stream tell of its bound
/// in §2.7.9
#[derive(Debug, Clone, Copy)]
enum Class {
    /// The bound itself
    Known(DecoderBufferBound),
    /// An H.262 or ISO/IEC 11172-2 video stream, the size of whose VBV is
    /// the caller's
    Vbv,
    /// An H.264 video stream, the size of whose CPB is the caller's
    Cpb,
    /// A video sub-bitstream of an AVC video stream, which §2.7.9 does not
    /// say it bounds
    SubBitstream,
    /// A stream whose `stream_type` does not say what it carries
    Undecided,
}

/// Returns what `stream_id` (Table 2-22) and `stream_type` (Table 2-34) tell
/// of the bound of a stream
fn class(stream_id: StreamId, stream_type: StreamType) -> Class {
    let adts = stream_type == StreamType::ISO_IEC_13818_7_AUDIO;
    // Why not the stream_type first: Table 2-22 gives these stream_ids to
    // audio streams alone, and an audio bound is the stricter.
    if stream_id.is_audio() || adts {
        return Class::Known(if adts {
            DecoderBufferBound::AdtsAudio
        } else {
            DecoderBufferBound::Audio
        });
    }
    match stream_type {
        StreamType::ISO_IEC_11172_3_AUDIO
        | StreamType::ISO_IEC_13818_3_AUDIO
        | StreamType::ISO_IEC_14496_3_AUDIO_LATM
        | StreamType::ISO_IEC_14496_3_AUDIO
        | StreamType::ISO_IEC_23008_3_MHAS_MAIN_AUDIO
        | StreamType::ISO_IEC_23008_3_MHAS_AUXILIARY_AUDIO => {
            Class::Known(DecoderBufferBound::Audio)
        }
        StreamType::ISO_IEC_11172_2_VIDEO
        | StreamType::ISO_IEC_13818_2_VIDEO
        | StreamType::ISO_IEC_13818_2_ADDITIONAL_VIEW_VIDEO => Class::Vbv,
        StreamType::AVC_VIDEO | StreamType::AVC_ADDITIONAL_VIEW_VIDEO => Class::Cpb,
        StreamType::SVC_VIDEO_SUB_BITSTREAM
        | StreamType::MVC_VIDEO_SUB_BITSTREAM
        | StreamType::MVCD_VIDEO_SUB_BITSTREAM => Class::SubBitstream,
        StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA => Class::Undecided,
        // Why not the values Table 2-34 reserves among the others: a later
        // edition may give them to audio.
        _ if stream_type.is_user_private()
            || stream_type.get() == 0x00
            || (0x37..=0x7e).contains(&stream_type.get()) =>
        {
            Class::Undecided
        }
        _ => Class::Known(DecoderBufferBound::Unbounded),
    }
}

/// Returns whether `declared` may be the bound of the stream of `stream_id`
/// and `stream_type`: the bound its `stream_id` and `stream_type` tell, or of
/// the kind they tell
pub(super) fn is_declarable(
    stream_id: StreamId,
    stream_type: StreamType,
    declared: DecoderBufferBound,
) -> bool {
    match class(stream_id, stream_type) {
        Class::Known(bound) => declared == bound,
        Class::Vbv => matches!(declared, DecoderBufferBound::Vbv { .. }),
        Class::Cpb => matches!(declared, DecoderBufferBound::Cpb { .. }),
        Class::SubBitstream => matches!(
            declared,
            DecoderBufferBound::Cpb { .. } | DecoderBufferBound::Unbounded
        ),
        Class::Undecided => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{Class, Constraints, DecoderBufferBound, class, is_declarable};
    use crate::pes::StreamId;
    use crate::stream_type::StreamType;

    /// The class of each `stream_type` (Table 2-34), on a `stream_id` that
    /// is not of audio, as §2.7.9 reads it
    fn expected_class(stream_type: u8) -> &'static str {
        match stream_type {
            0x03 | 0x04 | 0x11 | 0x1c | 0x2d | 0x2e => "audio",
            0x0f => "adts",
            0x01 | 0x02 | 0x22 => "vbv",
            0x1b | 0x23 => "cpb",
            0x1f | 0x20 | 0x26 => "sub-bitstream",
            0x00 | 0x06 | 0x37..=0x7e | 0x80..=0xff => "undecided",
            _ => "unbounded",
        }
    }

    fn name(class: Class) -> &'static str {
        match class {
            Class::Known(DecoderBufferBound::Audio) => "audio",
            Class::Known(DecoderBufferBound::AdtsAudio) => "adts",
            Class::Known(DecoderBufferBound::Unbounded) => "unbounded",
            Class::Known(DecoderBufferBound::Vbv { .. } | DecoderBufferBound::Cpb { .. }) => {
                "sized"
            }
            Class::Vbv => "vbv",
            Class::Cpb => "cpb",
            Class::SubBitstream => "sub-bitstream",
            Class::Undecided => "undecided",
        }
    }

    #[test]
    fn every_stream_type_is_classed_as_tables_2_22_and_2_34_assign_it() {
        let classes = |stream_id: u8| -> alloc::vec::Vec<&'static str> {
            (0..=u8::MAX)
                .map(|stream_type| {
                    name(class(
                        StreamId::from(stream_id),
                        StreamType::from(stream_type),
                    ))
                })
                .collect()
        };
        let expected: alloc::vec::Vec<_> = (0..=u8::MAX).map(expected_class).collect();
        let audio: alloc::vec::Vec<_> = (0..=u8::MAX)
            .map(|stream_type| if stream_type == 0x0f { "adts" } else { "audio" })
            .collect();

        assert_eq!(
            (classes(0xe0), classes(0xbd), classes(0xc0), classes(0xdf)),
            (expected.clone(), expected, audio.clone(), audio)
        );
    }

    #[test]
    fn a_declaration_is_taken_where_it_is_of_the_class_of_the_stream() {
        let vbv = DecoderBufferBound::Vbv { size: 1, rv_max: 1 };
        let cpb = DecoderBufferBound::Cpb {
            cpb_size: 1,
            rv_max: 1,
        };
        let bounds = [
            vbv,
            cpb,
            DecoderBufferBound::Audio,
            DecoderBufferBound::AdtsAudio,
            DecoderBufferBound::Unbounded,
        ];
        let declarable = |stream_id: u8, stream_type: u8| {
            bounds.map(|bound| {
                is_declarable(
                    StreamId::from(stream_id),
                    StreamType::from(stream_type),
                    bound,
                )
            })
        };

        assert_eq!(
            [
                declarable(0xe0, 0x02),
                declarable(0xe0, 0x1b),
                declarable(0xe0, 0x20),
                declarable(0xbd, 0x81),
                declarable(0xc0, 0x81),
                declarable(0xbd, 0x0f),
                declarable(0xe0, 0x24),
            ],
            [
                [true, false, false, false, false],
                [false, true, false, false, false],
                [false, true, false, false, true],
                [true; 5],
                [false, false, true, false, false],
                [false, false, false, true, false],
                [false, false, false, false, true],
            ]
        );
    }

    #[test]
    fn bs_add_is_6144_bytes_or_rv_max_times_0_001_over_8_past_the_vbv() {
        let csps = Constraints::csps(3_528);
        let video = |rv_max: u32| {
            csps.buffer_sizes(
                StreamId::from(0xe0),
                StreamType::ISO_IEC_13818_2_VIDEO,
                Some(DecoderBufferBound::Vbv {
                    size: 100_000,
                    rv_max,
                }),
            )
        };

        // ⌊49 152 000 × 0.001 / 8⌋ = 6 144
        assert_eq!(
            [1_000_000, 49_151_999, 49_152_000, 49_160_000, 80_000_000].map(video),
            [
                Some(100_000..=106_144),
                Some(100_000..=106_144),
                Some(100_000..=106_144),
                Some(100_000..=106_145),
                Some(100_000..=110_000),
            ]
        );
    }

    #[test]
    fn an_audio_buffer_is_at_most_4096_bytes_or_8976_for_adts_and_nothing_bounds_one_outside_a_csps()
     {
        let sizes = |constraints: Constraints, stream_id: u8, stream_type: StreamType| {
            constraints.buffer_sizes(StreamId::from(stream_id), stream_type, None)
        };

        assert_eq!(
            [
                sizes(
                    Constraints::csps(3_528),
                    0xc0,
                    StreamType::ISO_IEC_11172_3_AUDIO
                ),
                sizes(
                    Constraints::csps(3_528),
                    0xbd,
                    StreamType::ISO_IEC_14496_3_AUDIO
                ),
                sizes(
                    Constraints::csps(3_528),
                    0xc0,
                    StreamType::ISO_IEC_13818_7_AUDIO
                ),
                sizes(Constraints::csps(3_528), 0xe0, StreamType::HEVC_VIDEO),
                sizes(Constraints::none(), 0xc0, StreamType::ISO_IEC_11172_3_AUDIO),
                sizes(Constraints::none(), 0xe0, StreamType::ISO_IEC_13818_2_VIDEO),
            ],
            [
                Some(0..=4_096),
                Some(0..=4_096),
                Some(0..=8_976),
                None,
                None,
                None,
            ]
        );
    }

    #[test]
    #[should_panic(expected = "before set_decoder_buffer_bound")]
    fn a_buffer_of_a_video_stream_whose_vbv_is_not_declared_is_the_callers_mistake_in_a_csps() {
        let _ = Constraints::csps(3_528).buffer_sizes(
            StreamId::from(0xe0),
            StreamType::ISO_IEC_13818_2_VIDEO,
            None,
        );
    }

    #[test]
    fn the_packets_take_a_300th_of_a_second_each_up_to_a_rate_bound_of_6250_and_less_past_it() {
        // 27 000 000 / 300 = 90 000 ticks a packet up to 6 250 × 400 = 2.5 × 10⁶
        // bits a second; past it, NP × 562 500 000 / rate_bound rounded up
        assert_eq!(
            (
                [1, 3].map(|packets| Constraints::csps(6_250).packet_ticks(packets)),
                [1, 3].map(|packets| Constraints::csps(6_251).packet_ticks(packets)),
                Constraints::csps(25_200).packet_ticks(3),
                Constraints::none().packet_ticks(3),
            ),
            ([90_000, 270_000], [89_986, 269_957], 66_965, 0)
        );
    }
}
