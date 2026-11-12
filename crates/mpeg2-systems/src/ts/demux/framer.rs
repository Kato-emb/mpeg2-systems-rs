//! Framing of Transport Packets off the packet grid
//!
//! Every packet is reported with the input offset of its unit, the stride of
//! bytes it takes on the wire, counted from the first byte ever handed to the
//! [`Framer`](crate::framing::Framer); every other byte is reported as
//! unsynchronized.

use core::ops::Range;

use crate::framing::{Buffer, Decoder, Skipped};
use crate::ts::packet::{PACKET_LENGTH, SYNC_BYTE};

// ETSI TR 101 290: declare loss after two consecutive missing sync bytes.
const SYNC_LOSS_COUNT: u8 = 2;

// ETSI TR 101 290: lock onto the grid after five consecutive sync bytes at a
// fixed period.
const SYNC_LOCK_COUNT: usize = 5;

/// Bytes from one Transport Packet to the next on the wire, which stay the same
/// through a Transport Stream
///
/// The 188-byte `transport_packet` of ISO/IEC 13818-1 §2.4.3.2, alone or with
/// the bytes a carrier adds around each one. The demuxer frames the 188 bytes
/// and does not read what the carrier added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PacketSize {
    /// The Transport Packet alone, 188 bytes
    Standard188,
    /// A 4-byte timestamp before each Transport Packet, 192 bytes
    Timestamped192,
    /// 16 Reed-Solomon parity bytes after each Transport Packet, 204 bytes
    ReedSolomon204,
}

impl PacketSize {
    /// Returns the bytes from one `sync_byte` to the next
    #[must_use]
    pub const fn stride(self) -> usize {
        match self {
            Self::Standard188 => PACKET_LENGTH,
            Self::Timestamped192 => 192,
            Self::ReedSolomon204 => 204,
        }
    }

    /// Bytes the carrier puts before each `sync_byte`
    const fn prefix(self) -> usize {
        match self {
            Self::Timestamped192 => 4,
            Self::Standard188 | Self::ReedSolomon204 => 0,
        }
    }
}

/// What the search saw at the front of the bytes it has not ruled on
///
/// A unit is the stride of bytes one packet takes on the wire; it starts
/// [`PacketSize::prefix`] bytes before its `sync_byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchObservation {
    /// This many bytes before the first place a unit can start
    LeadingGarbage(usize),
    /// A unit starts at the front, and its `sync_byte` repeats at this period
    GridConfirmed(PacketSize),
    /// Too few bytes to confirm or rule out every candidate period
    Undecided,
    /// A unit could start at the front, but no candidate period repeats it
    NoGridAtFront,
}

/// Every packet size, smallest first
const CANDIDATES: [PacketSize; 3] = [
    PacketSize::Standard188,
    PacketSize::Timestamped192,
    PacketSize::ReedSolomon204,
];

enum GridCheck {
    Confirmed,
    RuledOut,
    NeedMoreData,
}

/// Classifies the front of `bytes` against the candidate packet grids
///
/// A fixed packet size restricts the search to that period; `None` tries all
/// three smallest-first. [`SearchObservation::LeadingGarbage`] counts at most
/// `garbage_limit` bytes.
fn observe(bytes: &[u8], fixed: Option<PacketSize>, garbage_limit: usize) -> SearchObservation {
    let candidates = fixed
        .as_ref()
        .map_or(&CANDIDATES[..], core::slice::from_ref);
    let longest_prefix = candidates
        .iter()
        .map(|packet_size| packet_size.prefix())
        .max()
        .unwrap_or_default();
    // Why stop short of the end by the longest prefix when no unit start is
    // found: a sync_byte past the end can still start a unit that many bytes
    // before it.
    let horizon = garbage_limit.min(bytes.len().saturating_sub(longest_prefix));
    let mut leading_garbage = horizon;
    let scanned = bytes
        .get(..horizon.saturating_add(longest_prefix))
        .unwrap_or(bytes);
    for (index, _) in scanned
        .iter()
        .enumerate()
        .filter(|&(_, &byte)| byte == SYNC_BYTE)
    {
        // Why stop here: a later sync_byte starts a unit at most the longest
        // prefix before it, so none can start one earlier than found.
        if index >= leading_garbage.saturating_add(longest_prefix) {
            break;
        }
        for packet_size in candidates {
            if let Some(start) = index.checked_sub(packet_size.prefix()) {
                leading_garbage = leading_garbage.min(start);
            }
        }
    }
    if leading_garbage > 0 {
        return SearchObservation::LeadingGarbage(leading_garbage);
    }
    let mut undecided = false;
    for &packet_size in candidates {
        match check_grid(bytes, packet_size) {
            // Why not tie-break against a larger period confirming first:
            // candidates run smallest-first, and a smaller period being
            // undecided means the bytes are too few for any larger one to
            // confirm (188 undecided => len <= 4 * 188, but 192 needs
            // 4 + 4 * 192 + 1), so the first Confirmed is always the smallest
            // matching period.
            GridCheck::Confirmed => return SearchObservation::GridConfirmed(packet_size),
            GridCheck::RuledOut => {}
            GridCheck::NeedMoreData => undecided = true,
        }
    }
    if undecided {
        SearchObservation::Undecided
    } else {
        SearchObservation::NoGridAtFront
    }
}

/// Checks the `sync_byte` cadence of units starting at the front
fn check_grid(bytes: &[u8], packet_size: PacketSize) -> GridCheck {
    let sampled = bytes
        .get(packet_size.prefix()..)
        .unwrap_or_default()
        .iter()
        .step_by(packet_size.stride())
        .take(SYNC_LOCK_COUNT);
    if sampled.clone().any(|&byte| byte != SYNC_BYTE) {
        GridCheck::RuledOut
    } else if sampled.count() < SYNC_LOCK_COUNT {
        GridCheck::NeedMoreData
    } else {
        GridCheck::Confirmed
    }
}

/// What the unit at the front of the bytes holds on the locked grid
enum FrameObservation {
    OnGrid,
    SyncByteMissing {
        found: u8,
    },
    /// Fewer bytes than a unit, but a packet on the grid without the carrier
    /// bytes after it
    PacketWithoutCarrier,
    /// Fewer bytes than a unit, and no packet on the grid among them
    Short,
}

/// Classifies the unit at the front of `bytes` against the locked grid
fn observe_on_grid(bytes: &[u8], packet_size: PacketSize) -> FrameObservation {
    let prefix = packet_size.prefix();
    match (bytes.get(prefix), bytes.len()) {
        (Some(&SYNC_BYTE), length) if length >= packet_size.stride() => FrameObservation::OnGrid,
        (Some(&found), length) if length >= packet_size.stride() => {
            FrameObservation::SyncByteMissing { found }
        }
        (Some(&SYNC_BYTE), length) if length >= prefix.saturating_add(PACKET_LENGTH) => {
            FrameObservation::PacketWithoutCarrier
        }
        _ => FrameObservation::Short,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Searching {
        /// Bytes after the cut ones that the search walked past and has not
        /// reported yet
        // Why not rescan from the cut bytes instead: the run is reported only
        // at a grid, the report limit or the end, so rescanning it on every
        // step costs time quadratic in the run.
        skipped: Skipped,
    },
    Synced {
        packet_size: PacketSize,
        consecutive_misses: u8,
    },
}

/// A unit [`TransportPackets`] cut
///
/// A range indexes the buffer through
/// [`Framer::bytes`](crate::framing::Framer::bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Output {
    /// The grid was confirmed at this packet size
    SyncAcquired(PacketSize),
    /// One Transport Packet was cut off the grid
    Packet {
        /// The 188 bytes of the packet, without what the carrier added
        range: Range<usize>,
        /// Input offset of the packet's unit, from where the grid can be
        /// picked up again
        offset: u64,
    },
    /// One `sync_byte` was missing; the unit was passed over and the grid is
    /// kept for now
    SyncByteMissing {
        /// The byte found where `sync_byte` was expected
        found: u8,
        /// The unit passed over
        range: Range<usize>,
    },
    /// Consecutive `sync_byte`s were missing and the grid was dropped
    SyncLost,
    /// These bytes were not cut into a packet
    Unsynchronized(Range<usize>),
}

/// The Transport Packets of a stream, cut off the `sync_byte` grid
#[derive(Debug, Clone)]
pub(super) struct TransportPackets {
    fixed_packet_size: Option<PacketSize>,
    state: State,
}

impl TransportPackets {
    pub(super) const fn new(fixed_packet_size: Option<PacketSize>) -> Self {
        Self {
            fixed_packet_size,
            state: State::Searching {
                skipped: Skipped::new(),
            },
        }
    }

    /// Cuts the next unit; `eof` tells that no input follows the bytes held
    fn cut(&mut self, buffer: &mut Buffer, eof: bool) -> Option<Output> {
        loop {
            let (next_state, consume, then) = self.transition(buffer.unconsumed(), eof);
            self.state = next_state;
            let range = buffer.split_to(consume);
            let cut = match then {
                Then::Continue => continue,
                Then::NeedMoreData => return None,
                Then::Output(cut) => cut,
            };
            return Some(match cut {
                Cut::SyncAcquired(packet_size) => Output::SyncAcquired(packet_size),
                Cut::Packet(packet_size) => {
                    let packet_start = range.start.saturating_add(packet_size.prefix());
                    Output::Packet {
                        range: packet_start..packet_start.saturating_add(PACKET_LENGTH),
                        offset: buffer.offset_of(range.start),
                    }
                }
                Cut::SyncByteMissing { found } => Output::SyncByteMissing { found, range },
                Cut::SyncLost => Output::SyncLost,
                Cut::Unsynchronized => Output::Unsynchronized(range),
            });
        }
    }

    /// Decides, from the state and what is at the front of `rest`, the next
    /// state, the bytes to cut, and what follows
    fn transition(&self, rest: &[u8], eof: bool) -> (State, usize, Then) {
        match self.state {
            // Why report before observing again: the report is capped, so a
            // stream that never synchronizes hands its bytes back in bounded
            // pieces instead of holding all of them.
            State::Searching { mut skipped } if skipped.is_full() => {
                let length = skipped.report();
                (
                    State::Searching { skipped },
                    length,
                    Then::Output(Cut::Unsynchronized),
                )
            }
            State::Searching { mut skipped } => {
                let searched = rest.get(skipped.count()..).unwrap_or_default();
                // Why cap the garbage counted at what the report has room
                // for: one chunk can carry far more garbage than the limit,
                // and taking it all would make one report exceed the bound;
                // counting past the cap would scan bytes only to scan them
                // again after the report.
                match (
                    observe(searched, self.fixed_packet_size, skipped.room()),
                    eof,
                ) {
                    (SearchObservation::LeadingGarbage(count), _) => {
                        skipped.skip(count);
                        (State::Searching { skipped }, 0, Then::Continue)
                    }
                    (SearchObservation::NoGridAtFront, _) => {
                        skipped.skip(1);
                        (State::Searching { skipped }, 0, Then::Continue)
                    }
                    // Why report before acquiring rather than alongside it: one
                    // cut carries one output, so the run walked past is handed
                    // back here and the next cut acquires on the same bytes.
                    (SearchObservation::GridConfirmed(_), _) if skipped.count() > 0 => {
                        let length = skipped.report();
                        (
                            State::Searching { skipped },
                            length,
                            Then::Output(Cut::Unsynchronized),
                        )
                    }
                    (SearchObservation::GridConfirmed(packet_size), _) => (
                        State::Synced {
                            packet_size,
                            consecutive_misses: 0,
                        },
                        0,
                        Then::Output(Cut::SyncAcquired(packet_size)),
                    ),
                    (SearchObservation::Undecided, false) => {
                        (State::Searching { skipped }, 0, Then::NeedMoreData)
                    }
                    // Why rule an undecided candidate out at the end, rather
                    // than give up the search: no bytes will arrive to confirm
                    // it, and a grid can still start at a later place in the
                    // bytes left.
                    (SearchObservation::Undecided, true) if !searched.is_empty() => {
                        skipped.skip(1);
                        (State::Searching { skipped }, 0, Then::Continue)
                    }
                    (SearchObservation::Undecided, true) => {
                        let length = skipped.report_rest(rest.len());
                        let then = if length > 0 {
                            Then::Output(Cut::Unsynchronized)
                        } else {
                            Then::NeedMoreData
                        };
                        (State::Searching { skipped }, length, then)
                    }
                }
            }
            State::Synced {
                packet_size,
                consecutive_misses,
            } => match (observe_on_grid(rest, packet_size), eof) {
                (FrameObservation::OnGrid, _) => (
                    State::Synced {
                        packet_size,
                        consecutive_misses: 0,
                    },
                    packet_size.stride(),
                    Then::Output(Cut::Packet(packet_size)),
                ),
                // Why cut nothing (unlike a single miss): the real grid may
                // restart inside these bytes, so leave them for the search to
                // rescan.
                (FrameObservation::SyncByteMissing { .. }, _)
                    if consecutive_misses.saturating_add(1) >= SYNC_LOSS_COUNT =>
                {
                    (
                        State::Searching {
                            skipped: Skipped::new(),
                        },
                        0,
                        Then::Output(Cut::SyncLost),
                    )
                }
                (FrameObservation::SyncByteMissing { found }, _) => (
                    State::Synced {
                        packet_size,
                        consecutive_misses: consecutive_misses.saturating_add(1),
                    },
                    packet_size.stride(),
                    Then::Output(Cut::SyncByteMissing { found }),
                ),
                // Why wait for the whole unit mid-stream, not only its packet:
                // the unit is cut whole, so a packet is not cut while the
                // carrier bytes after it (204's parity) may still be on their
                // way.
                (FrameObservation::PacketWithoutCarrier | FrameObservation::Short, false) => {
                    (self.state, 0, Then::NeedMoreData)
                }
                // Why a packet, not a whole unit, at the end: no carrier bytes
                // after it will arrive, so the prefix and 188 bytes on the grid
                // are a whole packet; a 204-byte stream cut before its last
                // parity would otherwise lose its last packet. The grid stays:
                // an end of input is not a grid break, so the packet size still
                // holds if the caller resumes.
                (FrameObservation::PacketWithoutCarrier, true) => (
                    self.state,
                    packet_size.prefix().saturating_add(PACKET_LENGTH),
                    Then::Output(Cut::Packet(packet_size)),
                ),
                // Why report what is left rather than drop it: a byte dropped
                // without an event breaks the guarantee that every byte handed
                // over is reported, and a resume must not join these bytes
                // across the end.
                (FrameObservation::Short, true) if rest.is_empty() => {
                    (self.state, 0, Then::NeedMoreData)
                }
                (FrameObservation::Short, true) => {
                    (self.state, rest.len(), Then::Output(Cut::Unsynchronized))
                }
            },
        }
    }
}

/// What follows a transition
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    /// The cut goes on, observing again
    Continue,
    /// The cut ends with no output: the input holds too few bytes
    NeedMoreData,
    /// The cut ends with this output, over the bytes cut
    Output(Cut),
}

/// What a cut yields, before the bytes it takes are known as a range
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cut {
    SyncAcquired(PacketSize),
    /// A unit, or at the end a packet without the carrier bytes after it
    Packet(PacketSize),
    SyncByteMissing {
        found: u8,
    },
    SyncLost,
    Unsynchronized,
}

impl Decoder for TransportPackets {
    type Item = Output;

    fn decode(&mut self, buffer: &mut Buffer) -> Option<Output> {
        self.cut(buffer, false)
    }

    fn decode_eof(&mut self, buffer: &mut Buffer) -> Option<Output> {
        self.cut(buffer, true)
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "fixtures are built at constant offsets well inside their length"
)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::framing::tests::{Reported, check};
    use crate::framing::{Framer, Next};

    const STANDARD_188: &[u8] = include_bytes!("../../../tests/fixtures/188.ts");
    const TIMESTAMPED_192: &[u8] = include_bytes!("../../../tests/fixtures/192.ts");
    const REED_SOLOMON_204: &[u8] = include_bytes!("../../../tests/fixtures/204.ts");

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
        EndOfInput,
    }

    fn drain(framer: &mut Framer<TransportPackets>, seen: &mut Vec<Seen>) {
        loop {
            seen.push(match framer.next() {
                Next::NeedMoreData => return,
                Next::EndOfInput => Seen::EndOfInput,
                Next::Item(Output::SyncAcquired(packet_size)) => Seen::SyncAcquired(packet_size),
                Next::Item(Output::Packet { range, offset }) => Seen::Packet {
                    bytes: framer.bytes(range).to_vec(),
                    offset,
                },
                Next::Item(Output::SyncByteMissing { found, .. }) => {
                    Seen::SyncByteMissing { found }
                }
                Next::Item(Output::SyncLost) => Seen::SyncLost,
                Next::Item(Output::Unsynchronized(range)) => {
                    Seen::Unsynchronized(framer.bytes(range).to_vec())
                }
            });
        }
    }

    /// Hands `stream` over in chunks of `chunk_length`, then ends the input, and
    /// returns what came before the end of input
    fn frame(stream: &[u8], chunk_length: usize, fixed: Option<PacketSize>) -> Vec<Seen> {
        let mut framer = Framer::new(TransportPackets::new(fixed));
        let mut seen = Vec::new();
        for chunk in stream.chunks(chunk_length) {
            framer.compact();
            framer.extend(chunk);
            drain(&mut framer, &mut seen);
        }
        framer.finish();
        drain(&mut framer, &mut seen);
        assert_eq!(seen.pop(), Some(Seen::EndOfInput));
        seen
    }

    /// The packets of the units from `first_unit` on that hold a whole packet
    fn packets_of(stream: &[u8], packet_size: PacketSize, first_unit: usize) -> Vec<Seen> {
        let prefix = packet_size.prefix();
        (first_unit..)
            .step_by(packet_size.stride())
            .take_while(|unit| unit + prefix + PACKET_LENGTH <= stream.len())
            .map(|unit| Seen::Packet {
                bytes: stream[unit + prefix..unit + prefix + PACKET_LENGTH].to_vec(),
                offset: u64::try_from(unit).unwrap(),
            })
            .collect()
    }

    #[test]
    fn every_fixture_is_cut_into_the_same_packets_whatever_the_chunk_length() {
        for (stream, packet_size) in [
            (STANDARD_188, PacketSize::Standard188),
            (TIMESTAMPED_192, PacketSize::Timestamped192),
            (REED_SOLOMON_204, PacketSize::ReedSolomon204),
        ] {
            let mut expected = vec![Seen::SyncAcquired(packet_size)];
            expected.extend(packets_of(stream, packet_size, 0));
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
    fn a_192_byte_stream_cut_inside_a_timestamp_is_unsynchronized_up_to_its_next_unit() {
        let stream = &TIMESTAMPED_192[2..];
        let mut expected = vec![
            Seen::Unsynchronized(stream[..190].to_vec()),
            Seen::SyncAcquired(PacketSize::Timestamped192),
        ];
        expected.extend(packets_of(stream, PacketSize::Timestamped192, 190));
        for chunk_length in CHUNK_LENGTHS {
            assert_eq!(
                frame(stream, chunk_length, None),
                expected,
                "in chunks of {chunk_length}"
            );
        }
    }

    #[test]
    fn the_end_salvages_a_last_packet_that_lacks_only_the_carrier_bytes_after_it() {
        for (stream, packet_size, cut) in [
            (TIMESTAMPED_192, PacketSize::Timestamped192, 192),
            (REED_SOLOMON_204, PacketSize::ReedSolomon204, 16),
        ] {
            let stream = &stream[..stream.len() - cut];
            let mut expected = vec![Seen::SyncAcquired(packet_size)];
            expected.extend(packets_of(stream, packet_size, 0));
            assert_eq!(frame(stream, 1000, None), expected, "{packet_size:?}");
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
        let mut expected = vec![Seen::Unsynchronized(vec![0xFF; Skipped::LIMIT]); 3];
        expected.push(Seen::Unsynchronized(vec![
            0xFF;
            stream.len() - 3 * Skipped::LIMIT
        ]));
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
        for (stream, packet_size) in [
            (STANDARD_188, PacketSize::Standard188),
            (TIMESTAMPED_192, PacketSize::Timestamped192),
        ] {
            let units = 10 * packet_size.stride();
            let stream = &stream[..units + 100];
            let mut expected = vec![Seen::SyncAcquired(packet_size)];
            expected.extend(packets_of(&stream[..units], packet_size, 0));
            expected.push(Seen::Unsynchronized(stream[units..].to_vec()));
            assert_eq!(frame(stream, 1000, None), expected, "{packet_size:?}");
        }
    }

    #[test]
    fn input_after_a_drained_end_goes_on_at_the_next_offset_on_the_grid_kept() {
        let mut framer = Framer::new(TransportPackets::new(None));
        let mut seen = Vec::new();
        framer.extend(&STANDARD_188[..188 * 5 + 100]);
        framer.finish();
        drain(&mut framer, &mut seen);
        framer.extend(&STANDARD_188[188 * 6..]);
        framer.finish();
        drain(&mut framer, &mut seen);
        let mut input = STANDARD_188[..188 * 5 + 100].to_vec();
        input.extend_from_slice(&STANDARD_188[188 * 6..]);
        let mut expected = vec![Seen::SyncAcquired(PacketSize::Standard188)];
        expected.extend(packets_of(&input[..188 * 5], PacketSize::Standard188, 0));
        expected.push(Seen::Unsynchronized(input[188 * 5..188 * 5 + 100].to_vec()));
        expected.push(Seen::EndOfInput);
        expected.extend(packets_of(&input, PacketSize::Standard188, 188 * 5 + 100));
        expected.push(Seen::EndOfInput);
        assert_eq!(seen, expected);
    }

    #[test]
    #[should_panic(expected = "after finish")]
    fn input_before_the_end_is_drained_is_refused() {
        let mut framer = Framer::new(TransportPackets::new(None));
        framer.extend(&STANDARD_188[..188 * 5 + 100]);
        framer.finish();
        framer.extend(&STANDARD_188[188 * 6..]);
    }

    #[test]
    fn a_candidate_the_end_leaves_unconfirmed_does_not_hide_a_grid_after_it() {
        let mut stream = vec![0x00; 754];
        for packet in 0..4 {
            stream[204 * packet] = SYNC_BYTE;
        }
        for packet in 0..SYNC_LOCK_COUNT {
            stream[1 + 188 * packet] = SYNC_BYTE;
        }
        let mut expected = vec![
            Seen::Unsynchronized(stream[..1].to_vec()),
            Seen::SyncAcquired(PacketSize::Standard188),
        ];
        expected.extend(packets_of(&stream[..753], PacketSize::Standard188, 1));
        expected.push(Seen::Unsynchronized(stream[753..].to_vec()));
        for chunk_length in CHUNK_LENGTHS {
            assert_eq!(
                frame(&stream, chunk_length, None),
                expected,
                "in chunks of {chunk_length}"
            );
        }
    }

    impl Reported for Output {
        fn range(&self) -> Option<Range<usize>> {
            match self {
                Self::Packet { range, .. }
                | Self::SyncByteMissing { range, .. }
                | Self::Unsynchronized(range) => Some(range.clone()),
                Self::SyncAcquired(_) | Self::SyncLost => None,
            }
        }

        fn is_passed_over(&self) -> bool {
            matches!(self, Self::Unsynchronized(_))
        }
    }

    #[test]
    fn the_framing_of_a_188_byte_stream_follows_the_rules_of_every_framing() {
        let mut missing = grid(188, 8, 0);
        missing[188 * 5] = 0x00;
        let mut lost = grid(188, 8, 0);
        lost[188 * 5] = 0x00;
        lost[188 * 6] = 0x00;
        lost.extend(grid(188, SYNC_LOCK_COUNT, 0));
        let mut behind_garbage = vec![0xFF; Skipped::LIMIT + 100];
        behind_garbage.extend_from_slice(&STANDARD_188[..188 * 6 + 100]);
        for stream in [
            STANDARD_188,
            &missing,
            &lost,
            &behind_garbage,
            &[0xFF; 3 * 1024],
        ] {
            check(&TransportPackets::new(None), stream);
        }
    }

    /// A `sync_byte` at `lead + k * stride` for each of `sync_count` packets,
    /// and no other 0x47
    fn grid(stride: usize, sync_count: usize, lead: usize) -> Vec<u8> {
        let mut bytes = vec![0x00; lead + stride * sync_count];
        for packet in 0..sync_count {
            bytes[lead + stride * packet] = SYNC_BYTE;
        }
        bytes
    }

    #[test]
    fn five_sync_bytes_at_one_period_confirm_the_grid() {
        for packet_size in CANDIDATES {
            assert_eq!(
                observe(
                    &grid(packet_size.stride(), SYNC_LOCK_COUNT, packet_size.prefix()),
                    None,
                    usize::MAX
                ),
                SearchObservation::GridConfirmed(packet_size)
            );
        }
    }

    #[test]
    fn four_sync_bytes_leave_the_grid_undecided() {
        assert_eq!(
            observe(&grid(188, SYNC_LOCK_COUNT - 1, 0), None, usize::MAX),
            SearchObservation::Undecided
        );
    }

    #[test]
    fn a_192_byte_unit_starts_at_the_timestamp_before_its_sync_byte() {
        assert_eq!(
            observe(&grid(192, SYNC_LOCK_COUNT, 4), None, usize::MAX),
            SearchObservation::GridConfirmed(PacketSize::Timestamped192)
        );
    }

    #[test]
    fn bytes_before_the_first_unit_are_leading_garbage() {
        assert_eq!(
            observe(
                &grid(188, SYNC_LOCK_COUNT, 10),
                Some(PacketSize::Standard188),
                usize::MAX
            ),
            SearchObservation::LeadingGarbage(10)
        );
    }

    #[test]
    fn detection_keeps_the_bytes_a_timestamp_could_take_before_a_sync_byte() {
        assert_eq!(
            observe(&grid(188, SYNC_LOCK_COUNT, 10), None, usize::MAX),
            SearchObservation::LeadingGarbage(10 - 4)
        );
    }

    #[test]
    fn a_sync_byte_with_no_room_for_a_timestamp_starts_no_192_byte_unit() {
        assert_eq!(
            observe(
                &grid(192, SYNC_LOCK_COUNT, 2),
                Some(PacketSize::Timestamped192),
                usize::MAX
            ),
            SearchObservation::LeadingGarbage(2 + 192 - 4)
        );
    }

    #[test]
    fn a_sync_byte_no_period_repeats_is_no_grid() {
        let mut bytes = grid(188, SYNC_LOCK_COUNT, 0);
        bytes[188 * 3] = 0x00;
        bytes.extend_from_slice(&[0x00; 204 * SYNC_LOCK_COUNT]);
        assert_eq!(
            observe(&bytes, None, usize::MAX),
            SearchObservation::NoGridAtFront
        );
    }

    #[test]
    fn a_fixed_packet_size_rules_out_the_other_periods() {
        let mut bytes = grid(188, SYNC_LOCK_COUNT, 0);
        bytes.extend_from_slice(&[0x00; 204 * SYNC_LOCK_COUNT]);
        assert_eq!(
            observe(&bytes, Some(PacketSize::ReedSolomon204), usize::MAX),
            SearchObservation::NoGridAtFront
        );
    }
}
