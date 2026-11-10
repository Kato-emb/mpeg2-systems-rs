//! A fixed bitrate: the SCRs on the line of equations 2-22 and 2-23, and the
//! padding that fills the time before a PES packet

use super::*;
use crate::ps::mux::schedule::{Gap, PackFill, line_bytes, line_ticks, pack_fill};
use crate::time::SystemTimeClock;

/// Ticks of the STC that 7 bytes take to arrive at [`RATE`], 1 260 000 bytes
/// a second
const TICKS_OF_SEVEN_BYTES: u64 = 150;

/// Ticks of the STC in the longest time from one SCR to the next, 0.7 s
/// (§2.7.1)
const LONGEST_SCR_INTERVAL: u64 = 18_900_000;

/// The packs of padding that fill `bytes` in packs of at most `longest`, as
/// their bytes and the `stream_id` of their PES packets: `longest` each but
/// the last, which takes the rest, the one before it giving up what the
/// last needs to make a pack header; a pack header carries the bytes past
/// it as stuffing up to 7, and a PES packet of padding past them
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the lengths are those of a few packs, written as the rule reads"
)]
fn padding_packs(bytes: usize, longest: usize) -> Vec<(usize, Vec<u8>)> {
    let count = bytes.div_ceil(longest);
    let rest = bytes - (count - 1) * longest;
    let mut lengths = vec![longest; count - 1];
    lengths.push(rest);
    if rest < 14 {
        lengths[count - 2] -= 14 - rest;
        lengths[count - 1] = 14;
    }
    lengths
        .into_iter()
        .map(|length| {
            let stream_ids = if length > 14 + 7 { vec![0xbe] } else { vec![] };
            (length, stream_ids)
        })
        .collect()
}

/// The packs of `stream`, as their bytes and the `stream_id` of their PES
/// packets
fn pack_shapes(stream: &[u8]) -> Vec<(usize, Vec<u8>)> {
    carried(stream)
        .into_iter()
        .map(|pack| (pack.length, pack.stream_ids))
        .collect()
}

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
        .map(|&(at, _)| (at, first + (at - first_at) * TICKS_OF_SEVEN_BYTES / 7))
        .collect();
    assert_eq!(scrs, on_line);
    assert!(scrs.len() > 100, "{} packs", scrs.len());
    let longest_step = scrs
        .windows(2)
        .map(|pair| pair[1].1 - pair[0].1)
        .max()
        .unwrap();
    assert!(longest_step <= LONGEST_SCR_INTERVAL, "{longest_step}");
}

#[test]
fn at_a_fixed_rate_the_system_header_has_fixed_flag_set() {
    let stream = multiplexed_by(muxer_of(fixed_rate()), &inputs()[..1]);
    let fixed = SystemHeader::read(carried(&stream)[0].system_header.unwrap()).unwrap();
    let variable = multiplexed_by(muxer(), &inputs()[..1]);
    let variable = SystemHeader::read(carried(&variable)[0].system_header.unwrap()).unwrap();

    assert_eq!(fixed, variable.with_fixed_flag(true));
}

/// What the muxer of `config` writes of two PES packets of video 1.5 s
/// apart, and how many bytes the packs of padding between them take
#[expect(
    clippy::arithmetic_side_effects,
    reason = "a stream has at least the packs of its two PES packets"
)]
fn padded_across_a_second_and_a_half(config: MuxerConfig) -> (Vec<u8>, usize) {
    let packets = [
        pes_packet(VIDEO, &[0xaa; 100], Some(90_000)),
        pes_packet(VIDEO, &[0xaa; 100], Some(225_000)),
    ];
    let stream = multiplexed_by(muxer_of(config), &packets);
    let packs = carried(&stream);
    let padding = packs[1..packs.len() - 1]
        .iter()
        .map(|pack| pack.length)
        .sum();
    (stream, padding)
}

#[test]
fn the_time_before_a_pes_packet_is_filled_with_padding_up_to_its_time() {
    let (stream, padding) = padded_across_a_second_and_a_half(fixed_rate());

    let packs = carried(&stream);
    let mut expected = vec![(packs[0].length, vec![VIDEO])];
    expected.extend(padding_packs(padding, 2_048));
    expected.push((packs.last().unwrap().length, vec![VIDEO]));
    assert_eq!(pack_shapes(&stream), expected);
    let padding_bytes: Vec<u8> = pes_packets(&stream)
        .into_iter()
        .filter(|packet| packet.stream_id == StreamId::PADDING_STREAM)
        .flat_map(|packet| packet.payload.to_vec())
        .collect();
    assert_eq!(padding_bytes, vec![0xff; padding_bytes.len()]);
    // The SCR of the PES packet is the first on the line at or after its
    // time, 1.5 s after the first, less mux_delay
    let (first, last) = (&packs[0], packs.last().unwrap());
    let on_line = |byte: u64| first.scr + (byte - first.scr_byte) * TICKS_OF_SEVEN_BYTES / 7;
    let due = first.scr + 135_000 * 300;
    assert_eq!(
        (on_line(last.scr_byte - 1) < due, last.scr >= due),
        (true, true)
    );
}

#[test]
fn padding_past_the_longest_pes_packet_is_cut_into_packs_of_a_pack_header_and_one() {
    let (stream, padding) = padded_across_a_second_and_a_half(fixed_rate().with_pack_size(100_000));

    let packs = carried(&stream);
    let mut expected = vec![(packs[0].length, vec![VIDEO])];
    expected.extend(padding_packs(padding, 14 + 65_541));
    expected.push((packs.last().unwrap().length, vec![VIDEO]));
    assert_eq!(pack_shapes(&stream), expected);
}

#[test]
fn at_a_fixed_rate_the_payloads_join_back_into_the_access_units() {
    let inputs = inputs();
    let stream = multiplexed_by(muxer_of(fixed_rate()), &inputs);

    let joined = |stream_id: u8, packets: Vec<PesPacket<'_>>| -> Vec<u8> {
        packets
            .into_iter()
            .filter(|packet| packet.stream_id.get() == stream_id)
            .flat_map(|packet| packet.payload.to_vec())
            .collect()
    };
    assert_eq!(
        (
            joined(VIDEO, pes_packets(&stream)),
            joined(AUDIO, pes_packets(&stream))
        ),
        (joined(VIDEO, inputs.clone()), joined(AUDIO, inputs))
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
fn the_padding_fills_the_bytes_missing_exactly_in_packs_of_at_most_the_longest_from_a_pack_header_on()
 {
    let before = Pack {
        at: SystemTimeClock::first(timestamp(90_000)),
        length: 100,
        offset: 0,
    };
    let lengths = |bytes: u64| {
        let time = before.on_line(100 + bytes, RATE);
        Gap::padding(before, time, RATE, 2_048).map(|padding| {
            (
                padding.iter().map(|pack| pack.length).collect::<Vec<_>>(),
                padding.bytes(),
                padding.last().next_offset() - 100,
            )
        })
    };

    // Below a pack header, none; from it on, the bytes missing
    assert_eq!(
        [0, 1, 13, 14, 2_048, 2_049, 2_061, 2_062, 4_097].map(lengths),
        [
            None,
            None,
            None,
            Some((vec![14], 14, 14)),
            Some((vec![2_048], 2_048, 2_048)),
            Some((vec![2_035, 14], 2_049, 2_049)),
            Some((vec![2_047, 14], 2_061, 2_061)),
            Some((vec![2_048, 14], 2_062, 2_062)),
            Some((vec![2_048, 2_035, 14], 4_097, 4_097)),
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

#[test]
fn a_pes_packet_is_at_its_time_or_up_to_13_bytes_earlier_but_never_later() {
    // Of the second of two PES packets of video, `step` 90 kHz ticks after
    // the first: whether padding filled the time to it exactly, or fewer
    // bytes than a pack header were missing and none did
    let shape = |step: u64| {
        let packets = [
            pes_packet(VIDEO, &[0xaa; 100], Some(90_000)),
            pes_packet(VIDEO, &[0xaa; 100], Some(90_000 + step)),
        ];
        let stream = multiplexed_by(muxer_of(fixed_rate()), &packets);
        let packs = carried(&stream);
        let (first, last) = (&packs[0], packs.last().unwrap());
        let on_line = |byte: u64| first.scr + (byte - first.scr_byte) * TICKS_OF_SEVEN_BYTES / 7;
        let due = first.scr + step * 300;
        let next = on_line(first.scr_byte + u64::try_from(first.length).unwrap());
        if due <= next {
            "on the line after the first"
        } else if packs.len() > 2 && last.scr >= due && on_line(last.scr_byte - 1) < due {
            "padded to its time"
        } else if packs.len() == 2
            && last.scr < due
            && due - last.scr <= 13 * TICKS_OF_SEVEN_BYTES / 7
        {
            "up to 13 bytes early"
        } else {
            "wrong"
        }
    };

    let shapes: Vec<_> = (0..200).map(shape).collect();
    assert!(!shapes.contains(&"wrong"), "{shapes:?}");
    assert!(shapes.contains(&"padded to its time") && shapes.contains(&"up to 13 bytes early"));
}
