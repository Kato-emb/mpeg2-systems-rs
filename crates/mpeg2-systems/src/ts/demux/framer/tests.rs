use alloc::vec;
use alloc::vec::Vec;

use super::sync::SYNC_LOCK_COUNT;
use super::sync::tests::grid;
use super::*;

const STANDARD_188: &[u8] = include_bytes!("../../../../tests/fixtures/188.ts");
const TIMESTAMPED_192: &[u8] = include_bytes!("../../../../tests/fixtures/192.ts");
const REED_SOLOMON_204: &[u8] = include_bytes!("../../../../tests/fixtures/204.ts");

/// Chunk lengths a stream is handed over in, the whole stream among them
const CHUNK_LENGTHS: [usize; 5] = [1, 7, 188, 1000, usize::MAX];

/// An [`Output`] with its bytes copied out of the framer
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    SyncAcquired(PacketSize),
    Packet { bytes: Vec<u8>, offset: u64 },
    SyncByteMissing { found: u8 },
    SyncLost,
    Unsynchronized(Vec<u8>),
}

fn drain(framer: &mut PacketFramer, seen: &mut Vec<Seen>) {
    loop {
        seen.push(match framer.step() {
            Output::NeedMoreData => return,
            Output::SyncAcquired(packet_size) => Seen::SyncAcquired(packet_size),
            Output::Packet { range, offset } => Seen::Packet {
                bytes: framer.bytes(range).to_vec(),
                offset,
            },
            Output::SyncByteMissing { found } => Seen::SyncByteMissing { found },
            Output::SyncLost => Seen::SyncLost,
            Output::Unsynchronized(range) => Seen::Unsynchronized(framer.bytes(range).to_vec()),
        });
    }
}

/// Hands `stream` over in chunks of `chunk_length`, then ends the input
fn frame(stream: &[u8], chunk_length: usize, fixed: Option<PacketSize>) -> Vec<Seen> {
    let mut framer = PacketFramer::new(fixed);
    let mut seen = Vec::new();
    for chunk in stream.chunks(chunk_length) {
        framer.extend(chunk);
        drain(&mut framer, &mut seen);
    }
    framer.finish();
    drain(&mut framer, &mut seen);
    seen
}

/// The packets of a fixture whose first `sync_byte` is at `lead`
fn packets_of(stream: &[u8], packet_size: PacketSize, lead: usize) -> Vec<Seen> {
    (lead..stream.len())
        .step_by(packet_size.stride())
        .map(|start| Seen::Packet {
            bytes: stream[start..start + PACKET_LENGTH].to_vec(),
            offset: u64::try_from(start).unwrap(),
        })
        .collect()
}

#[test]
fn every_fixture_is_cut_into_the_same_packets_whatever_the_chunk_length() {
    for (stream, packet_size, lead) in [
        (STANDARD_188, PacketSize::Standard188, 0),
        (TIMESTAMPED_192, PacketSize::Timestamped192, 4),
        (REED_SOLOMON_204, PacketSize::ReedSolomon204, 0),
    ] {
        let mut expected = Vec::new();
        if lead > 0 {
            expected.push(Seen::Unsynchronized(stream[..lead].to_vec()));
        }
        expected.push(Seen::SyncAcquired(packet_size));
        expected.extend(packets_of(stream, packet_size, lead));
        for chunk_length in CHUNK_LENGTHS {
            assert_eq!(
                frame(stream, chunk_length, None),
                expected,
                "{packet_size:?} in chunks of {chunk_length}"
            );
        }
    }
}

#[test]
fn fewer_sync_bytes_than_the_lock_count_do_not_synchronize() {
    let stream = grid(188, SYNC_LOCK_COUNT - 1, 0);
    assert_eq!(
        frame(&stream, usize::MAX, None),
        [Seen::Unsynchronized(stream.clone())]
    );
}

#[test]
fn a_single_missing_sync_byte_keeps_the_grid() {
    let mut stream = grid(188, 8, 0);
    stream[188 * 5] = 0x00;
    let mut expected = vec![Seen::SyncAcquired(PacketSize::Standard188)];
    expected.extend(packets_of(&stream[..188 * 5], PacketSize::Standard188, 0));
    expected.push(Seen::SyncByteMissing { found: 0x00 });
    expected.extend(packets_of(&stream, PacketSize::Standard188, 188 * 6));
    assert_eq!(frame(&stream, usize::MAX, None), expected);
}

#[test]
fn two_missing_sync_bytes_in_a_row_lose_the_grid_until_it_is_found_again() {
    let mut stream = grid(188, 8, 0);
    stream[188 * 5] = 0x00;
    stream[188 * 6] = 0x00;
    stream.extend(grid(188, SYNC_LOCK_COUNT, 0));
    let mut expected = vec![Seen::SyncAcquired(PacketSize::Standard188)];
    expected.extend(packets_of(&stream[..188 * 5], PacketSize::Standard188, 0));
    expected.extend([
        Seen::SyncByteMissing { found: 0x00 },
        Seen::SyncLost,
        Seen::Unsynchronized(stream[188 * 6..188 * 7].to_vec()),
        Seen::SyncAcquired(PacketSize::Standard188),
    ]);
    expected.extend(packets_of(&stream, PacketSize::Standard188, 188 * 7));
    assert_eq!(frame(&stream, usize::MAX, None), expected);
}

#[test]
fn a_fixed_packet_size_does_not_synchronize_on_another_period() {
    let stream = grid(188, 8, 0);
    assert_eq!(
        frame(&stream, usize::MAX, Some(PacketSize::ReedSolomon204)),
        [Seen::Unsynchronized(stream.clone())]
    );
}

#[test]
fn a_stream_that_never_synchronizes_is_handed_back_in_bounded_pieces() {
    let stream = vec![0xFF; 200 * 1024];
    let mut expected = vec![Seen::Unsynchronized(vec![0xFF; UNSYNCHRONIZED_REPORT_LIMIT]); 3];
    expected.push(Seen::Unsynchronized(
        vec![0xFF; stream.len() - 3 * UNSYNCHRONIZED_REPORT_LIMIT],
    ));
    for chunk_length in [1024, usize::MAX] {
        assert_eq!(
            frame(&stream, chunk_length, None),
            expected,
            "in chunks of {chunk_length}"
        );
    }
}

#[test]
fn the_end_reports_a_partial_packet_on_the_grid_as_unsynchronized() {
    let stream = &STANDARD_188[..188 * 10 + 100];
    let mut expected = vec![Seen::SyncAcquired(PacketSize::Standard188)];
    expected.extend(packets_of(&stream[..188 * 10], PacketSize::Standard188, 0));
    expected.push(Seen::Unsynchronized(stream[188 * 10..].to_vec()));
    assert_eq!(frame(stream, 1000, None), expected);
}

#[test]
fn input_after_the_end_is_framed_on_the_grid_kept() {
    let mut framer = PacketFramer::new(None);
    let mut seen = Vec::new();
    framer.extend(&STANDARD_188[..188 * 5]);
    framer.finish();
    framer.extend(&STANDARD_188[188 * 5..188 * 6]);
    drain(&mut framer, &mut seen);
    assert_eq!(seen, {
        let mut expected = vec![Seen::SyncAcquired(PacketSize::Standard188)];
        expected.extend(packets_of(
            &STANDARD_188[..188 * 6],
            PacketSize::Standard188,
            0,
        ));
        expected
    });
}
