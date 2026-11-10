//! A fixed bitrate: the SCRs on the line of equations 2-22 and 2-23, and the
//! padding that fills the time before a PES packet

use super::*;
use crate::ps::mux::schedule::{Gap, PackFill, line_bytes, line_ticks, pack_fill};
use crate::time::SystemTimeClock;

/// Ticks of the STC that 7 bytes take to arrive at [`RATE`], 1 260 000 bytes
/// a second
const SEVEN_BYTES: u64 = 150;

fn fixed_rate() -> MuxerConfig {
    MuxerConfig::new(RATE).with_fixed_rate(true)
}

/// PES packets of video and audio over 3 s, one split over packs, with a
/// leap of 1.5 s
fn inputs() -> Vec<PesPacket<'static>> {
    vec![
        pes_packet(VIDEO, &[0xaa; 3_000], Some(90_000)),
        pes_packet(AUDIO, &[0xbb; 300], Some(91_000)),
        pes_packet(VIDEO, &[0xaa; 100], Some(93_600)),
        pes_packet(AUDIO, &[0xbb; 300], Some(95_000)),
        pes_packet(VIDEO, &[0xaa; 5_000], Some(97_200)),
        pes_packet(VIDEO, &[0xaa; 100], Some(232_200)),
        pes_packet(AUDIO, &[0xbb; 300], Some(232_300)),
        pes_packet(VIDEO, &[0xaa; 100], Some(270_000)),
    ]
}

#[test]
fn at_a_fixed_rate_every_scr_is_on_the_line_of_equations_2_22_and_2_23() {
    let stream = multiplexed_by(muxer_of(fixed_rate()), &inputs());

    // SCR(i) = c1 × i + c2 with c1 = 150 / 7 ticks a byte, and c2 = SCR(i₀)
    // − c1 × i₀ through the first SCR; ⌊c1 × i + c2⌋ = SCR(i₀) + ⌊c1 × (i −
    // i₀)⌋
    let scrs: Vec<_> = carried(&stream)
        .into_iter()
        .map(|pack| (pack.scr_byte, pack.scr))
        .collect();
    let (first_at, first) = scrs[0];
    let on_line: Vec<_> = scrs
        .iter()
        .map(|&(at, _)| (at, first + (at - first_at) * SEVEN_BYTES / 7))
        .collect();
    assert_eq!(scrs, on_line);
    assert!(scrs.len() > 100, "{} packs", scrs.len());
}

#[test]
fn at_a_fixed_rate_the_system_header_has_fixed_flag_set() {
    let stream = multiplexed_by(muxer_of(fixed_rate()), &inputs()[..1]);
    let fixed = SystemHeader::read(carried(&stream)[0].system_header.unwrap()).unwrap();
    let variable = multiplexed_by(muxer(), &inputs()[..1]);
    let variable = SystemHeader::read(carried(&variable)[0].system_header.unwrap()).unwrap();

    assert_eq!(fixed, variable.with_fixed_flag(true));
}

#[test]
fn the_time_before_a_pes_packet_is_filled_with_padding_up_to_its_time() {
    let packets = [
        pes_packet(VIDEO, &[0xaa; 100], Some(90_000)),
        pes_packet(VIDEO, &[0xaa; 100], Some(225_000)),
    ];
    let stream = multiplexed_by(muxer_of(fixed_rate()), &packets);

    let packs = carried(&stream);
    let (last, between) = packs[1..].split_last().unwrap();
    assert!(
        between.iter().all(|pack| pack.length <= 2_048
            && pack.stream_ids.iter().all(|&stream_id| stream_id == 0xbe)),
        "{between:?}"
    );
    assert_eq!(last.stream_ids, [VIDEO]);
    let padding: Vec<u8> = pes_packets(&stream)
        .into_iter()
        .filter(|packet| packet.stream_id == StreamId::PADDING_STREAM)
        .flat_map(|packet| packet.payload.to_vec())
        .collect();
    assert!(padding.iter().all(|&byte| byte == 0xff));
    // The pack of the PES packet is due 1.5 s after the first, less no more
    // than the 14 bytes of the shortest pack of padding
    let scrs = scrs(&stream);
    let after = scrs.last().unwrap() - scrs[0];
    assert!(
        (135_000 * 300..135_000 * 300 + 2 * SEVEN_BYTES).contains(&after),
        "{after}"
    );
}

#[test]
fn a_pack_of_padding_takes_stuffing_up_to_7_bytes_and_a_padding_packet_past_them() {
    assert_eq!(
        [14, 21, 22, 23, 14 + 65_541].map(pack_fill),
        [
            PackFill::Stuffing(0),
            PackFill::Stuffing(7),
            PackFill::PaddingPacket(2),
            PackFill::PaddingPacket(3),
            PackFill::PaddingPacket(65_535),
        ]
    );
}

#[test]
fn the_padding_is_cut_into_packs_of_at_most_the_longest_the_last_no_shorter_than_a_pack_header() {
    let last = Pack {
        at: SystemTimeClock::first(timestamp(90_000)),
        length: 100,
        offset: 0,
    };
    let lengths = |bytes: u64| {
        let time = last.on_line(100 + bytes, RATE);
        Gap::padding(last, time, RATE, 2_048).map(|padding| {
            (
                padding.iter().map(|pack| pack.length).collect::<Vec<_>>(),
                padding.bytes(),
            )
        })
    };

    assert_eq!(
        [0, 1, 14, 2_048, 2_049, 4_096].map(lengths),
        [
            None,
            Some((vec![14], 14)),
            Some((vec![14], 14)),
            Some((vec![2_048], 2_048)),
            Some((vec![2_048, 14], 2_062)),
            Some((vec![2_048, 2_048], 4_096)),
        ]
    );
}

#[test]
fn the_line_is_taken_in_whole_ticks_rounded_down_past_the_width_of_u64() {
    // 2^40 bytes: 27 000 000 × 2^40 is past u64
    let far = 1_u64 << 40;
    let far_ticks = u64::try_from(u128::from(far) * 150 / 7).unwrap();

    assert_eq!(
        (
            [0, 1, 6, 7, far].map(|bytes| line_ticks(bytes, RATE)),
            line_ticks(u64::MAX, 1),
            [128, 129, 150, 151].map(|ticks| line_bytes(ticks, RATE)),
        ),
        ([0, 21, 128, 150, far_ticks], u64::MAX, [6, 7, 7, 8])
    );
}

#[test]
fn at_a_variable_rate_each_pack_keeps_where_its_scr_is_in_the_stream() {
    let mut muxer = muxer();
    let mut stream = Vec::new();
    for packet in &inputs() {
        muxer.handle_pes_packet(packet).unwrap();
        muxer.poll_output(&mut stream);
    }

    let packs = carried(&stream);
    assert_eq!(
        muxer.last_pack.map(|pack| pack.offset),
        Some(packs.last().unwrap().scr_byte - packs[0].scr_byte)
    );
}

#[test]
fn padding_past_the_buffer_limit_is_refused_as_a_leap() {
    let mut muxer = muxer_of(fixed_rate().with_buffer_limit(10_000));
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(90_000)))
        .unwrap();

    assert_eq!(
        muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(180_000))),
        Err(Error::TimestampGap { gap: 90_000 })
    );
}
