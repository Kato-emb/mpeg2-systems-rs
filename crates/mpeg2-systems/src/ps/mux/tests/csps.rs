//! A constrained system parameter stream: the packet rate of equation 2-28,
//! and the P-STD buffer of each stream within the bound of §2.7.9

use super::*;

/// The `program_mux_rate` of a Video CD, 176 400 bytes a second, at which
/// equation 2-28 asks 1/300 s a packet
const VCD_RATE: u32 = 3_528;

/// The `pack_size` of a Video CD
const VCD_PACK_SIZE: usize = 2_324;

/// The VBV declared for the video stream, under the 224 KiB registered for
/// it by at most the 6 KiB `BSadd` allows
const VBV: DecoderBufferBound = DecoderBufferBound::Vbv {
    size: 220 * 1024,
    rv_max: 1_150_000,
};

/// A muxer of `config`, a CSPS, with a video and an audio stream, the VBV
/// of the video declared
fn csps_muxer(config: MuxerConfig) -> Muxer {
    let mut muxer = muxer_of(config.with_csps(true));
    muxer
        .set_decoder_buffer_bound(StreamId::from(VIDEO), None, VBV)
        .unwrap();
    muxer
}

/// Whether each pack of `stream`, a Program Stream of `rate_bound` and
/// `program_mux_rate` `rate`, meets equation 2-28: NP ≤ (t(i′) − t(i)) × 300 ×
/// max[1, `R_max` / 2.5 × 10⁶], `R_max` = 400 × `rate_bound` bits a second, t(i′)
/// for the last pack the arrival of the last byte of the end code
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the products of the equation are taken in u128, far from its bounds"
)]
fn meets_the_packet_rate(stream: &[u8], rate: u32) -> Vec<bool> {
    let packs = carried(stream);
    let knee: u128 = 2_500_000;
    let allowed = 300 * knee.max(400 * u128::from(rate));
    let end_code_last_byte = u64::try_from(stream.len()).unwrap() - 1;
    packs
        .iter()
        .enumerate()
        .map(|(index, pack)| {
            let packets = u128::from(pack.system_header.is_some())
                + u128::from(pack.map.is_some())
                + u128::try_from(pack.stream_ids.len()).unwrap();
            // NP × X ≤ Δt × 300 × max[X, R_max], Δt in seconds
            match packs.get(index + 1) {
                Some(next) => {
                    packets * knee * 27_000_000 <= u128::from(next.scr - pack.scr) * allowed
                }
                None => {
                    let bytes = u128::from(end_code_last_byte - pack.scr_byte);
                    packets * knee * 50 * u128::from(rate) <= bytes * allowed
                }
            }
        })
        .collect()
}

/// Audio PES packets of 100 bytes 1 ms apart, faster than a Video CD may
/// carry packets
fn audio_every_millisecond() -> Vec<PesPacket<'static>> {
    (90_000..90_900)
        .step_by(90)
        .map(|pts| pes_packet(AUDIO, &[0xbb; 100], Some(pts)))
        .collect()
}

#[test]
fn at_a_variable_rate_each_pack_waits_for_the_packet_rate_of_equation_2_28() {
    let inputs = audio_every_millisecond();
    let csps = multiplexed_by(csps_muxer(MuxerConfig::new(VCD_RATE)), &inputs);
    let not_csps = multiplexed_by(muxer_of(MuxerConfig::new(VCD_RATE)), &inputs);

    let met = meets_the_packet_rate(&csps, VCD_RATE);
    assert_eq!(met, vec![true; met.len()]);
    assert!(meets_the_packet_rate(&not_csps, VCD_RATE).contains(&false));
    assert_eq!(pes_packets(&csps), pes_packets(&not_csps));
}

#[test]
fn a_pack_header_alone_goes_before_the_end_code_where_it_would_come_sooner_than_the_packet_rate() {
    let inputs = [pes_packet(AUDIO, &[0xbb; 100], Some(90_000))];
    let csps = multiplexed_by(csps_muxer(MuxerConfig::new(VCD_RATE)), &inputs);
    let not_csps = multiplexed_by(muxer_of(MuxerConfig::new(VCD_RATE)), &inputs);
    let tail = |stream: &[u8]| -> Vec<bool> {
        elements(stream)
            .into_iter()
            .map(|(element, _)| matches!(element, Element::PackHeader(_)))
            .rev()
            .take(3)
            .collect()
    };

    // The first pack carries 3 packets: 3/300 s, past the 1 ms its bytes
    // take at 176 400 bytes a second
    assert_eq!(meets_the_packet_rate(&csps, VCD_RATE), [true, true]);
    assert_eq!(meets_the_packet_rate(&not_csps, VCD_RATE), [false]);
    assert_eq!(
        (tail(&csps), tail(&not_csps)),
        (vec![false, true, false], vec![false, false, false])
    );
}

/// PES packets of video and audio, one split over packs, with a leap
fn video_and_audio() -> Vec<PesPacket<'static>> {
    vec![
        pes_packet(VIDEO, &[0xaa; 3_000], Some(90_000)),
        pes_packet(AUDIO, &[0xbb; 300], Some(91_000)),
        pes_packet(VIDEO, &[0xaa; 100], Some(93_600)),
        pes_packet(AUDIO, &[0xbb; 30], Some(95_000)),
        pes_packet(VIDEO, &[0xaa; 5_000], Some(97_200)),
        pes_packet(VIDEO, &[0xaa; 100], Some(232_200)),
    ]
}

fn vcd() -> MuxerConfig {
    MuxerConfig::new(VCD_RATE)
        .with_pack_size(VCD_PACK_SIZE)
        .with_fixed_rate(true)
}

#[test]
fn at_a_fixed_rate_each_pack_is_filled_to_the_packet_rate_on_the_line() {
    let stream = multiplexed_by(csps_muxer(vcd()), &video_and_audio());
    let packs = carried(&stream);

    let met = meets_the_packet_rate(&stream, VCD_RATE);
    assert_eq!(met, vec![true; met.len()]);
    let (first_at, first) = (packs[0].scr_byte, packs[0].scr);
    let scrs: Vec<_> = packs.iter().map(|pack| pack.scr).collect();
    let on_line: Vec<_> = packs
        .iter()
        .map(|pack| first + (pack.scr_byte - first_at) * 27_000_000 / (50 * u64::from(VCD_RATE)))
        .collect();
    assert_eq!(scrs, on_line);
    assert_eq!(
        payloads(&stream),
        payloads(&multiplexed_by(muxer_of(vcd()), &video_and_audio()))
    );
}

/// The payloads of the PES packets of `stream` joined by `stream_id`, those
/// of `padding_stream` left out
fn payloads(stream: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut payloads: Vec<(u8, Vec<u8>)> = Vec::new();
    for packet in pes_packets(stream) {
        let stream_id = packet.stream_id.get();
        if packet.stream_id == StreamId::PADDING_STREAM {
            continue;
        }
        match payloads.iter_mut().find(|(id, _)| *id == stream_id) {
            Some((_, payload)) => payload.extend_from_slice(packet.payload),
            None => payloads.push((stream_id, packet.payload.to_vec())),
        }
    }
    payloads
}

#[test]
fn at_a_fixed_rate_the_system_header_and_the_map_go_in_packs_of_their_own() {
    let stream = multiplexed_by(
        csps_muxer(vcd().with_header_interval(Duration::ZERO)),
        &video_and_audio()[..3],
    );
    // A packet takes 90 000 ticks, 588 bytes at 176 400 bytes a second; a
    // pack of a system header or a map, or of a PES packet shorter than 588
    // bytes, is filled with a packet of padding to 2 × 588 bytes, and the
    // rest of the video, 733 bytes, is not
    let shapes: Vec<_> = carried(&stream)
        .into_iter()
        .map(|pack| {
            (
                pack.length,
                pack.system_header.is_some(),
                pack.map.is_some(),
                pack.stream_ids,
            )
        })
        .collect();

    assert_eq!(
        shapes[..7],
        [
            (1_176, true, false, vec![0xbe]),
            (1_176, false, true, vec![0xbe]),
            (VCD_PACK_SIZE, false, false, vec![VIDEO]),
            (733, false, false, vec![VIDEO]),
            (1_176, true, false, vec![0xbe]),
            (1_176, false, true, vec![0xbe]),
            (1_176, false, false, vec![AUDIO, 0xbe]),
        ]
    );
}

#[test]
fn the_system_header_of_a_csps_has_csps_flag_set_and_packet_rate_restriction_flag_clear() {
    let inputs = [pes_packet(VIDEO, &[0xaa; 100], Some(90_000))];
    let csps = multiplexed_by(csps_muxer(MuxerConfig::new(VCD_RATE)), &inputs);
    let not_csps = multiplexed_by(muxer_of(MuxerConfig::new(VCD_RATE)), &inputs);
    let system_header =
        |stream| SystemHeader::read(carried(stream)[0].system_header.unwrap()).unwrap();

    assert_eq!(
        system_header(&csps),
        system_header(&not_csps)
            .with_csps_flag(true)
            .with_packet_rate_restriction_flag(false)
    );
}

#[test]
fn at_10_mbit_a_second_a_csps_at_a_fixed_rate_takes_a_pack_size_that_holds_two_packets() {
    // 1 042 bytes a packet; a pack of a system header alone is filled with a
    // packet of padding to 2 084 bytes
    let new = |pack_size| {
        Muxer::new(
            MuxerConfig::new(RATE)
                .with_pack_size(pack_size)
                .with_fixed_rate(true)
                .with_csps(true),
        )
        .map(|_| ())
    };

    assert_eq!(
        [new(2_048), new(2_083), new(2_084)],
        [
            Err(Error::PackSize { pack_size: 2_048 }),
            Err(Error::PackSize { pack_size: 2_083 }),
            Ok(())
        ]
    );
    assert!(
        Muxer::new(
            MuxerConfig::new(RATE)
                .with_pack_size(2_048)
                .with_fixed_rate(true)
        )
        .is_ok()
    );
}

/// What a CSPS muxer of a Video CD rate says of the first PES packet of a
/// stream of `stream_type` on `stream_id`, its bound `p_std_buffer`
/// registered, `declared` its bound, or `p_std_buffer` the PES packet carries
fn first_of(
    stream_id: u8,
    stream_type: StreamType,
    registered: PStdBuffer,
    declared: Option<DecoderBufferBound>,
    carried: Option<PStdBuffer>,
) -> Result<(), Error> {
    let mut muxer = Muxer::new(MuxerConfig::new(VCD_RATE).with_csps(true)).unwrap();
    let stream_id = StreamId::from(stream_id);
    muxer
        .add_stream(ElementaryStream::new(stream_type, stream_id), registered)
        .unwrap();
    if let Some(declared) = declared {
        muxer
            .set_decoder_buffer_bound(stream_id, None, declared)
            .unwrap();
    }
    let mut packet = pes_packet(stream_id.get(), &[0xaa; 100], Some(90_000));
    if let Some(carried) = carried {
        let mut extension = PesExtension::new();
        extension.p_std_buffer = Some(carried);
        packet.optional_header.as_mut().unwrap().pes_extension = Some(extension);
    }
    muxer.handle_pes_packet(&packet)
}

fn csps_buffer_size(
    stream_id: u8,
    p_std_buffer: PStdBuffer,
    smallest: u32,
    largest: u32,
) -> Result<(), Error> {
    Err(Error::CspsBufferSize {
        stream_id: StreamId::from(stream_id),
        stream_id_extension: None,
        p_std_buffer,
        smallest,
        largest,
    })
}

#[test]
fn the_p_std_buffer_of_an_audio_stream_is_at_most_4096_bytes_and_8976_for_adts() {
    let audio = |stream_id, stream_type, units| {
        first_of(
            stream_id,
            stream_type,
            PStdBuffer::new(false, units),
            None,
            None,
        )
    };

    // In units of 128 bytes: 32 is 4 096, 70 is 8 960 and 71 is 9 088
    assert_eq!(
        [
            audio(0xc0, StreamType::ISO_IEC_11172_3_AUDIO, 32),
            audio(0xc0, StreamType::ISO_IEC_11172_3_AUDIO, 33),
            audio(0xbd, StreamType::ISO_IEC_14496_3_AUDIO_LATM, 33),
            audio(0xc1, StreamType::ISO_IEC_13818_7_AUDIO, 70),
            audio(0xc1, StreamType::ISO_IEC_13818_7_AUDIO, 71),
        ],
        [
            Ok(()),
            csps_buffer_size(0xc0, PStdBuffer::new(false, 33), 0, 4_096),
            csps_buffer_size(0xbd, PStdBuffer::new(false, 33), 0, 4_096),
            Ok(()),
            csps_buffer_size(0xc1, PStdBuffer::new(false, 71), 0, 8_976),
        ]
    );
}

#[test]
fn the_p_std_buffer_of_a_video_stream_is_its_vbv_and_at_most_bs_add_more() {
    let vbv = DecoderBufferBound::Vbv {
        size: 40 * 1024,
        rv_max: 1_150_000,
    };
    let video = |units, carried| {
        first_of(
            VIDEO,
            StreamType::ISO_IEC_11172_2_VIDEO,
            PStdBuffer::new(true, units),
            Some(vbv),
            carried,
        )
    };

    // BSadd at most 6 KiB: 40 to 46 KiB
    assert_eq!(
        [
            video(46, None),
            video(47, None),
            video(39, None),
            video(224, Some(PStdBuffer::new(true, 39))),
            video(224, Some(PStdBuffer::new(true, 40))),
        ],
        [
            Ok(()),
            csps_buffer_size(VIDEO, PStdBuffer::new(true, 47), 40_960, 47_104),
            csps_buffer_size(VIDEO, PStdBuffer::new(true, 39), 40_960, 47_104),
            csps_buffer_size(VIDEO, PStdBuffer::new(true, 39), 40_960, 47_104),
            Ok(()),
        ]
    );
}

#[test]
fn outside_a_csps_the_p_std_buffer_is_not_bounded_by_its_stream() {
    let mut muxer = Muxer::new(MuxerConfig::new(VCD_RATE)).unwrap();
    muxer
        .add_stream(
            ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, StreamId::from(AUDIO)),
            PStdBuffer::new(false, 33),
        )
        .unwrap();

    assert_eq!(
        muxer.handle_pes_packet(&pes_packet(AUDIO, &[0xbb; 100], Some(90_000))),
        Ok(())
    );
}

#[test]
#[should_panic(expected = "before set_decoder_buffer_bound")]
fn a_p_std_buffer_of_a_stream_whose_bound_is_not_declared_is_the_callers_mistake_in_a_csps() {
    let _ = first_of(
        0xbd,
        StreamType::from(0x81),
        PStdBuffer::new(false, 32),
        None,
        None,
    );
}

#[test]
fn a_bound_declared_is_taken_where_the_stream_may_have_it() {
    let declare = |stream_type: StreamType, bound| {
        let mut muxer = Muxer::new(MuxerConfig::new(VCD_RATE).with_csps(true)).unwrap();
        muxer
            .add_stream(
                ElementaryStream::new(stream_type, StreamId::from(VIDEO)),
                VIDEO_BUFFER,
            )
            .unwrap();
        muxer.set_decoder_buffer_bound(StreamId::from(VIDEO), None, bound)
    };
    let refused = |bound| {
        Err(Error::DecoderBufferBound {
            stream_id: StreamId::from(VIDEO),
            stream_id_extension: None,
            decoder_buffer_bound: bound,
        })
    };

    assert_eq!(
        [
            declare(StreamType::ISO_IEC_13818_2_VIDEO, VBV),
            declare(StreamType::ISO_IEC_13818_2_VIDEO, DecoderBufferBound::Audio),
            declare(
                StreamType::MVC_VIDEO_SUB_BITSTREAM,
                DecoderBufferBound::Unbounded
            ),
            declare(StreamType::MVC_VIDEO_SUB_BITSTREAM, VBV),
        ],
        [
            Ok(()),
            refused(DecoderBufferBound::Audio),
            Ok(()),
            refused(VBV)
        ]
    );
}

#[test]
fn a_stream_type_the_bound_declared_for_the_stream_does_not_fit_is_refused() {
    let mut declared = csps_muxer(MuxerConfig::new(VCD_RATE));
    let mut undeclared = muxer_of(MuxerConfig::new(VCD_RATE).with_csps(true));
    let avc = ElementaryStream::new(StreamType::AVC_VIDEO, StreamId::from(VIDEO));

    assert_eq!(
        (declared.update_stream(avc), undeclared.update_stream(avc)),
        (
            Err(Error::DecoderBufferBound {
                stream_id: StreamId::from(VIDEO),
                stream_id_extension: None,
                decoder_buffer_bound: VBV,
            }),
            Ok(())
        )
    );
}

#[test]
fn at_a_fixed_rate_a_pack_header_alone_goes_before_the_end_code_where_it_would_come_sooner_than_the_packet_rate()
 {
    // The last pack is filled to the 588 bytes a packet takes to the next
    // SCR, and the end code ends 5 bytes before the byte that SCR is in
    let ends: Vec<(bool, bool)> = (540..=600)
        .map(|length| {
            let stream = multiplexed_by(
                csps_muxer(vcd()),
                &[
                    pes_packet(VIDEO, &[0xaa; 3_000], Some(90_000)),
                    pes_packet(AUDIO, &vec![0xbb; length], Some(91_000)),
                ],
            );
            let last = carried(&stream).pop().unwrap();
            (
                meets_the_packet_rate(&stream, VCD_RATE)
                    .iter()
                    .all(|&met| met),
                last.stream_ids.is_empty(),
            )
        })
        .collect();

    assert!(ends.iter().all(|&(met, _)| met), "{ends:?}");
    assert!(ends.iter().any(|&(_, closing)| closing), "{ends:?}");
    assert!(ends.iter().any(|&(_, closing)| !closing), "{ends:?}");
}
