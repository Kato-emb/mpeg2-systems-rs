use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::pes::{OptionalHeader, PesPacket, PesPacketLength, pes_packet_length};

/// `stream_id` of the first video stream
const VIDEO: u8 = 0xE0;

fn timestamp(value: u64) -> Timestamp {
    Timestamp::try_from(value).unwrap()
}

/// `length` bytes counting up, so a payload put back together out of order
/// differs
fn counting_payload(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect()
}

/// The PES packets `bytes` hold one after another, each cut by its
/// `PES_packet_length`
fn packets(bytes: &[u8]) -> Vec<PesPacket<'_>> {
    let mut packets = Vec::new();
    let mut rest = bytes;
    while let PesPacketLength::Bounded(length) = pes_packet_length(rest) {
        let (packet, after) = rest.split_at(length);
        packets.push(PesPacket::read(packet).unwrap());
        rest = after;
    }
    assert!(rest.is_empty(), "bytes after the PES packets: {rest:02x?}");
    packets
}

fn joined_payload(packets: &[PesPacket<'_>]) -> Vec<u8> {
    packets
        .iter()
        .flat_map(|packet| packet.payload.iter().copied())
        .collect()
}

/// The timestamps of each packet and the length of its payload
fn shapes(packets: &[PesPacket<'_>]) -> Vec<(Option<u64>, Option<u64>, usize)> {
    packets
        .iter()
        .map(|packet| {
            let header = packet.optional_header.as_ref();
            (
                header.and_then(|header| header.pts).map(Timestamp::get),
                header.and_then(|header| header.dts).map(Timestamp::get),
                packet.payload.len(),
            )
        })
        .collect()
}

fn video_packet<'a>(pts: Option<u64>, dts: Option<u64>, payload: &'a [u8]) -> PesPacket<'a> {
    let mut header = OptionalHeader::new();
    header.pts = pts.map(timestamp);
    header.dts = dts.map(timestamp);
    let mut packet = PesPacket::new(StreamId::from(VIDEO), payload);
    packet.optional_header = Some(header);
    packet
}

#[test]
fn a_payload_too_long_for_one_packet_splits_and_rejoins() {
    let payload = counting_payload(200_000);
    let mut out = Vec::new();
    let written = write_pes_packets(
        StreamId::from(VIDEO),
        Some(timestamp(90_000)),
        Some(timestamp(86_400)),
        &payload,
        &mut out,
    )
    .unwrap();
    let packets = packets(&out);
    assert_eq!(
        (
            written,
            joined_payload(&packets) == payload,
            shapes(&packets)
        ),
        (
            4,
            true,
            vec![
                (Some(90_000), Some(86_400), 65_522),
                (None, None, 65_532),
                (None, None, 65_532),
                (None, None, 3_414),
            ]
        )
    );
}

#[test]
fn a_payload_filling_one_packet_exactly_is_not_split() {
    let payload = counting_payload(65_527);
    let mut out = Vec::new();
    let written = write_pes_packets(
        StreamId::from(VIDEO),
        Some(timestamp(0)),
        None,
        &payload,
        &mut out,
    )
    .unwrap();
    assert_eq!(
        (written, packets(&out)),
        (1, vec![video_packet(Some(0), None, &payload)])
    );
}

#[test]
fn a_payload_one_byte_over_one_packet_goes_on_in_a_second_without_timestamps() {
    let payload = counting_payload(65_528);
    let mut out = Vec::new();
    let written = write_pes_packets(
        StreamId::from(VIDEO),
        Some(timestamp(0)),
        None,
        &payload,
        &mut out,
    )
    .unwrap();
    let (first, second) = payload.split_at(65_527);
    assert_eq!(
        (written, packets(&out)),
        (
            2,
            vec![
                video_packet(Some(0), None, first),
                video_packet(None, None, second),
            ]
        )
    );
}

#[test]
fn an_empty_payload_still_carries_its_timestamps() {
    let mut out = Vec::new();
    let written = write_pes_packets(
        StreamId::from(VIDEO),
        Some(timestamp(45_000)),
        None,
        &[],
        &mut out,
    )
    .unwrap();
    assert_eq!(
        (written, packets(&out)),
        (1, vec![video_packet(Some(45_000), None, &[])])
    );
}

#[test]
fn a_packet_written_without_timestamps_round_trips() {
    let mut out = Vec::new();
    let written =
        write_pes_packets(StreamId::from(VIDEO), None, None, &[0xAA, 0xBB], &mut out).unwrap();
    assert_eq!(
        (written, packets(&out)),
        (1, vec![video_packet(None, None, &[0xAA, 0xBB])])
    );
}

#[test]
fn a_timestamp_at_the_top_of_its_range_round_trips() {
    let top = (1 << 33) - 1;
    let mut out = Vec::new();
    write_pes_packets(
        StreamId::from(VIDEO),
        Some(timestamp(top)),
        Some(timestamp(top)),
        &[0x00],
        &mut out,
    )
    .unwrap();
    assert_eq!(
        packets(&out),
        vec![video_packet(Some(top), Some(top), &[0x00])]
    );
}

#[test]
fn a_stream_without_an_optional_header_gets_its_payload_alone() {
    let payload = counting_payload(70_000);
    let mut out = Vec::new();
    let written =
        write_pes_packets(StreamId::PRIVATE_STREAM_2, None, None, &payload, &mut out).unwrap();
    let (first, second) = payload.split_at(65_535);
    assert_eq!(
        (written, packets(&out)),
        (
            2,
            vec![
                PesPacket::new(StreamId::PRIVATE_STREAM_2, first),
                PesPacket::new(StreamId::PRIVATE_STREAM_2, second),
            ]
        )
    );
}

/// Writes after a byte already in `out`, and returns the result and what
/// `out` holds after
fn written_after_a_byte(
    stream_id: StreamId,
    pts: Option<Timestamp>,
    dts: Option<Timestamp>,
    payload: &[u8],
) -> (Result<usize, Error>, Vec<u8>) {
    let mut out = vec![0x47];
    let result = write_pes_packets(stream_id, pts, dts, payload, &mut out);
    (result, out)
}

#[test]
fn rejects_a_dts_without_a_pts() {
    assert_eq!(
        written_after_a_byte(StreamId::from(VIDEO), None, Some(timestamp(0)), &[0x00]),
        (Err(Error::DtsWithoutPts), vec![0x47])
    );
}

#[test]
fn rejects_a_timestamp_on_a_stream_that_carries_no_optional_header() {
    assert_eq!(
        written_after_a_byte(StreamId::PADDING_STREAM, Some(timestamp(0)), None, &[0x00]),
        (
            Err(Error::TimestampsOnStreamWithoutOptionalHeader {
                stream_id: StreamId::PADDING_STREAM
            }),
            vec![0x47]
        )
    );
}

#[test]
fn rejects_the_stream_id_of_an_element_that_is_not_a_pes_packet() {
    for value in [0xB9, 0xBA, 0xBB, 0xBC, 0x00] {
        let stream_id = StreamId::from(value);
        assert_eq!(
            written_after_a_byte(stream_id, None, None, &[0x00]),
            (Err(Error::StreamId { stream_id }), vec![0x47]),
            "{value:#04x}"
        );
    }
}

#[test]
fn a_packet_the_pes_layer_refuses_leaves_the_output_as_it_was() {
    assert_eq!(
        written_after_a_byte(StreamId::PRIVATE_STREAM_2, None, None, &[]),
        (
            Err(Error::PesPacket {
                error: pes::Error::LengthUnspecified {
                    stream_id: StreamId::PRIVATE_STREAM_2
                }
            }),
            vec![0x47]
        )
    );
}
