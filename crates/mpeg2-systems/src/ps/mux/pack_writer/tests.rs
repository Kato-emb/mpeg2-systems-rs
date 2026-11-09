use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::pes::{self, EncodedLen, OptionalHeader, StreamId};
use crate::ps::system_header::{self, StreamBound, StreamBoundLoop, SystemHeader};
use crate::time::{ClockReference, Timestamp};

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
    while let EncodedLen::Bounded(length) = pes::encoded_len(rest) {
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

/// A video PES packet carrying `payload`, with an optional header holding
/// the timestamps given
fn video_packet(pts: Option<u64>, dts: Option<u64>, payload: &[u8]) -> PesPacket<'_> {
    let mut header = OptionalHeader::new();
    header.pts = pts.map(timestamp);
    header.dts = dts.map(timestamp);
    let mut packet = PesPacket::new(StreamId::from(VIDEO), payload);
    packet.optional_header = Some(header);
    packet
}

fn write(packet: &PesPacket<'_>) -> (usize, Vec<u8>) {
    let mut out = Vec::new();
    let written = PackWriter::new()
        .write_pes_packets(packet, &mut out)
        .unwrap();
    (written, out)
}

#[test]
fn a_payload_too_long_for_one_packet_splits_and_rejoins() {
    let payload = counting_payload(200_000);
    let (written, out) = write(&video_packet(Some(90_000), Some(86_400), &payload));
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
    let packet = video_packet(Some(0), None, &payload);
    let (written, out) = write(&packet);
    assert_eq!((written, packets(&out)), (1, vec![packet]));
}

#[test]
fn a_payload_one_byte_over_one_packet_goes_on_in_a_second_without_timestamps() {
    let payload = counting_payload(65_528);
    let (written, out) = write(&video_packet(Some(0), None, &payload));
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
fn the_packets_after_the_first_keep_its_header_but_its_times() {
    let payload = counting_payload(70_000);
    let mut header = OptionalHeader::new();
    header.pts = Some(timestamp(90_000));
    header.escr = Some(ClockReference::new(1_000, 0).unwrap());
    header.copyright = true;
    header.stuffing_bytes = 2;
    let mut packet = PesPacket::new(StreamId::from(VIDEO), &payload);
    packet.optional_header = Some(header.clone());
    let (written, out) = write(&packet);
    let mut continued = header.clone();
    continued.pts = None;
    continued.escr = None;
    let first_length = usize::from(u16::MAX) - header.encoded_len();
    let (first, second) = payload.split_at(first_length);
    let mut expected_first = PesPacket::new(StreamId::from(VIDEO), first);
    expected_first.optional_header = Some(header);
    let mut expected_second = PesPacket::new(StreamId::from(VIDEO), second);
    expected_second.optional_header = Some(continued);
    assert_eq!(
        (written, packets(&out)),
        (2, vec![expected_first, expected_second])
    );
}

#[test]
fn an_empty_payload_still_carries_its_timestamps() {
    let packet = video_packet(Some(45_000), None, &[]);
    let (written, out) = write(&packet);
    assert_eq!((written, packets(&out)), (1, vec![packet]));
}

#[test]
fn a_packet_written_without_timestamps_round_trips() {
    let packet = video_packet(None, None, &[0xAA, 0xBB]);
    let (written, out) = write(&packet);
    assert_eq!((written, packets(&out)), (1, vec![packet]));
}

#[test]
fn a_timestamp_at_the_top_of_its_range_round_trips() {
    let top = (1 << 33) - 1;
    let packet = video_packet(Some(top), Some(top), &[0x00]);
    let (_, out) = write(&packet);
    assert_eq!(packets(&out), vec![packet]);
}

#[test]
fn a_stream_without_an_optional_header_gets_its_payload_alone() {
    let payload = counting_payload(70_000);
    let (written, out) = write(&PesPacket::new(StreamId::PRIVATE_STREAM_2, &payload));
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

/// Writes `packet` after a byte already in `out`, and returns the result
/// and what `out` holds after
fn written_after_a_byte(packet: &PesPacket<'_>) -> (Result<usize, Error>, Vec<u8>) {
    let mut out = vec![0x47];
    let result = PackWriter::new().write_pes_packets(packet, &mut out);
    (result, out)
}

#[test]
fn rejects_a_dts_without_a_pts() {
    assert_eq!(
        written_after_a_byte(&video_packet(None, Some(0), &[0x00])),
        (
            Err(Error::PesPacket {
                error: pes::Error::ForbiddenPtsDtsFlags
            }),
            vec![0x47]
        )
    );
}

#[test]
fn rejects_a_timestamp_on_a_stream_that_carries_no_optional_header() {
    let mut packet = video_packet(Some(0), None, &[0x00]);
    packet.stream_id = StreamId::PADDING_STREAM;
    assert_eq!(
        written_after_a_byte(&packet),
        (
            Err(Error::PesPacket {
                error: pes::Error::OptionalHeaderPresence {
                    stream_id: StreamId::PADDING_STREAM
                }
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
            written_after_a_byte(&PesPacket::new(stream_id, &[0x00])),
            (Err(Error::StreamId { stream_id }), vec![0x47]),
            "{value:#04x}"
        );
    }
}

#[test]
fn rejects_a_packet_of_an_unspecified_length() {
    let mut packet = video_packet(None, None, &[0x00]);
    packet.length_unspecified = true;
    assert_eq!(
        written_after_a_byte(&packet),
        (
            Err(Error::LengthUnspecified {
                stream_id: StreamId::from(VIDEO)
            }),
            vec![0x47]
        )
    );
}

#[test]
fn a_packet_the_pes_layer_refuses_leaves_the_output_as_it_was() {
    assert_eq!(
        written_after_a_byte(&PesPacket::new(StreamId::PRIVATE_STREAM_2, &[])),
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

#[test]
fn a_system_header_is_written_as_it_reads_back() {
    let mut stream_bounds = Vec::new();
    StreamBoundLoop::write_from(
        [
            StreamBound::new(StreamId::from(VIDEO), true, 230),
            StreamBound::new(StreamId::from(0xC0), false, 32),
        ],
        &mut stream_bounds,
    )
    .unwrap();
    let mut header = SystemHeader::new(1_101_198, 1, 1);
    header.stream_bounds = StreamBoundLoop::from(&stream_bounds[..]);
    let mut out = Vec::new();

    PackWriter::new()
        .write_system_header(&header, &mut out)
        .unwrap();

    assert_eq!(SystemHeader::read(&out), Ok(header));
}

#[test]
fn a_system_header_the_system_header_writer_refuses_is_refused_leaving_out_as_it_was() {
    let header = SystemHeader::new(0, 1, 1);
    let mut out = vec![0x00];

    let error = PackWriter::new().write_system_header(&header, &mut out);

    assert_eq!(
        error,
        Err(Error::SystemHeader {
            error: system_header::Error::Forbidden {
                field: "rate_bound",
                value: 0,
            }
        })
    );
    assert_eq!(out, [0x00]);
}

#[test]
fn the_count_and_the_bytes_of_the_pieces_are_known_before_they_are_cut() {
    // Pieces of at most 100 bytes, then 60: a header of 14 bytes with its
    // PTS leaves 86 for the first payload, and one of 9 without it 51
    let shapes: Vec<((usize, usize), (usize, usize))> = [0, 86, 87, 86 + 51, 86 + 51 + 1, 5_000]
        .into_iter()
        .map(|length| {
            let payload = vec![0xaa; length];
            let packet = video_packet(Some(90_000), None, &payload);
            let pieces = PesPacketPieces::new(&packet, 100, 60);
            let known = (pieces.len(), pieces.encoded_len());
            let cut: Vec<PesPacket<'_>> = pieces.collect();
            (
                known,
                (cut.len(), cut.iter().map(PesPacket::encoded_len).sum()),
            )
        })
        .collect();
    let (known, cut): (Vec<_>, Vec<_>) = shapes.into_iter().unzip();
    assert_eq!(
        (known, cut.clone()),
        (
            cut,
            vec![
                (1, 14),
                (1, 100),
                (2, 110),
                (2, 160),
                (3, 170),
                (98, 5_000 + 14 + 97 * 9)
            ]
        )
    );
}
