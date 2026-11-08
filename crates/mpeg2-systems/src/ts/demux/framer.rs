//! Framing of Transport Packets off the packet grid
//!
//! Every packet is reported with the input offset of its unit, the stride of
//! bytes it takes on the wire, counted from the first byte ever handed to
//! [`PacketFramer::extend`]; every other byte is reported as unsynchronized.

use alloc::vec::Vec;
use core::ops::Range;

use crate::ts::packet::{PACKET_LENGTH, SYNC_BYTE};

pub(super) mod sync;

use sync::{PacketSize, SearchObservation};

// ETSI TR 101 290: declare loss after two consecutive missing sync bytes.
const SYNC_LOSS_COUNT: u8 = 2;

/// Bytes walked past that force an [`Output::Unsynchronized`] report out
///
/// A longer run is reported in several outputs, which bounds what a stream
/// that never synchronizes holds while still handing every byte back.
const UNSYNCHRONIZED_REPORT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
enum State {
    Searching {
        /// Bytes after the consumed ones that the search walked past and has
        /// not reported yet
        skipped: usize,
    },
    Synced {
        packet_size: PacketSize,
        consecutive_misses: u8,
    },
}

/// Whether more input may follow the bytes a classifier is given
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    Continues,
    Ends,
}

/// What the framer saw in the unit at the front of the buffer on the locked
/// grid
enum FrameObservation {
    OnGrid,
    SyncByteMissing {
        found: u8,
    },
    NeedMoreData,
    /// At the end of input, a packet on the grid without the carrier bytes
    /// after it
    TrailingPacket,
    /// At the end of input, bytes that fill no packet
    Leftover,
}

/// Output of one [`PacketFramer::step`]
///
/// A range indexes the buffer through [`PacketFramer::bytes`] and stays valid
/// until the next [`PacketFramer::extend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Output {
    /// The buffer holds too few bytes to decide; extend it and step again
    NeedMoreData,
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
    /// One `sync_byte` was missing; the grid is kept for now
    SyncByteMissing {
        /// The byte found where `sync_byte` was expected
        found: u8,
    },
    /// Consecutive `sync_byte`s were missing and the grid was dropped
    SyncLost,
    /// These bytes were not cut into a packet
    Unsynchronized(Range<usize>),
}

struct Transition {
    next_state: State,
    /// Bytes that leave the buffer front, already reported or cut
    consume: usize,
    /// `None` keeps [`PacketFramer::step`] stepping
    output: Option<Output>,
}

/// Grid synchronization state machine over the input buffer
///
/// `position` is the input offset of `buffer[0]`. Bytes before `consumed`
/// were reported or cut and are dropped at the next [`PacketFramer::extend`];
/// until then the ranges an [`Output`] carries still point at them.
#[derive(Debug)]
pub(super) struct PacketFramer {
    fixed_packet_size: Option<PacketSize>,
    state: State,
    buffer: Vec<u8>,
    consumed: usize,
    position: u64,
    /// Buffer index where [`PacketFramer::finish`] ended the input, until the
    /// framer has reported every byte before it
    end: Option<usize>,
}

impl PacketFramer {
    pub(super) const fn new(fixed_packet_size: Option<PacketSize>) -> Self {
        Self {
            fixed_packet_size,
            state: State::Searching { skipped: 0 },
            buffer: Vec::new(),
            consumed: 0,
            position: 0,
            end: None,
        }
    }

    pub(super) fn extend(&mut self, bytes: &[u8]) {
        // Why not a VecDeque, which drops the front without moving the rest:
        // a packet straddling its wrap has no single slice to lend, so every
        // packet would be copied out. Draining moves only what is not yet cut,
        // less than a stride plus one unreported run.
        self.buffer.drain(..self.consumed);
        self.position = self.offset_of(self.consumed);
        self.end = self.end.map(|end| end.saturating_sub(self.consumed));
        self.consumed = 0;
        // Why copy every chunk, not only the tail that crosses into the
        // next: an event borrows the demuxer rather than the caller's chunk,
        // so poll_event can lend a packet after handle_input has returned.
        self.buffer.extend_from_slice(bytes);
    }

    /// Ends the input at the bytes handed over so far
    ///
    /// The steps that follow cut a final packet the grid holds without the
    /// carrier bytes after it, and report every other byte before the end as
    /// [`Output::Unsynchronized`]. Bytes extended after the end are framed
    /// once the end is reported.
    pub(super) fn finish(&mut self) {
        self.end = Some(self.buffer.len());
    }

    /// Returns the bytes of a range an [`Output`] carries
    pub(super) fn bytes(&self, range: Range<usize>) -> &[u8] {
        self.buffer.get(range).unwrap_or_default()
    }

    /// Input offset of the buffer byte at `index`
    fn offset_of(&self, index: usize) -> u64 {
        // Why saturating rather than wrapping: u64 spans 16 EiB of input, so
        // the ceiling is unreachable, and stopping at it beats wrapping an
        // offset into one that names the wrong byte.
        self.position
            .saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
    }

    /// Steps the state machine until it has something to report
    pub(super) fn step(&mut self) -> Output {
        loop {
            if self.end == Some(self.consumed) {
                self.end = None;
            }
            let transition = self.transition();
            self.state = transition.next_state;
            self.consumed = self.consumed.saturating_add(transition.consume);
            if let Some(output) = transition.output {
                return output;
            }
        }
    }

    fn transition(&self) -> Transition {
        let consumed = self.consumed;
        let limit = self.end.unwrap_or(self.buffer.len());
        let input = if self.end.is_some() {
            Input::Ends
        } else {
            Input::Continues
        };
        match self.state {
            // Why report before observing again: the report is capped, so a
            // stream that never synchronizes hands its bytes back in bounded
            // pieces instead of holding all of them.
            State::Searching { skipped } if skipped >= UNSYNCHRONIZED_REPORT_LIMIT => {
                self.report_skipped(skipped)
            }
            State::Searching { skipped } => {
                let cursor = consumed.saturating_add(skipped);
                let bytes = self.buffer.get(cursor..limit).unwrap_or_default();
                // Why cap the garbage counted at what the report has room
                // for: one chunk can carry far more garbage than the limit,
                // and taking it all would make one report exceed the bound;
                // counting past the cap would scan bytes only to scan them
                // again after the report.
                let room = UNSYNCHRONIZED_REPORT_LIMIT.saturating_sub(skipped);
                match sync::observe(bytes, self.fixed_packet_size, room, input) {
                    SearchObservation::LeadingGarbage(count) => Transition {
                        next_state: State::Searching {
                            skipped: skipped.saturating_add(count),
                        },
                        consume: 0,
                        output: None,
                    },
                    SearchObservation::NoGridAtFront => Transition {
                        next_state: State::Searching {
                            skipped: skipped.saturating_add(1),
                        },
                        consume: 0,
                        output: None,
                    },
                    // Why report before acquiring rather than alongside it: one
                    // step carries one output, so the run walked past is handed
                    // back here and the next step acquires on the same bytes.
                    SearchObservation::GridConfirmed(_) if skipped > 0 => {
                        self.report_skipped(skipped)
                    }
                    SearchObservation::GridConfirmed(packet_size) => Transition {
                        next_state: State::Synced {
                            packet_size,
                            consecutive_misses: 0,
                        },
                        consume: 0,
                        output: Some(Output::SyncAcquired(packet_size)),
                    },
                    // Why not hold an undecided tail at the end, unlike
                    // mid-stream: no bytes will arrive to decide it, and a
                    // leading sync byte no grid confirmed is unproven cadence,
                    // not a packet.
                    SearchObservation::UndecidedAtEnd => Transition {
                        next_state: State::Searching { skipped: 0 },
                        consume: limit.saturating_sub(consumed),
                        output: Some(Output::Unsynchronized(consumed..limit)),
                    },
                    SearchObservation::Undecided => Transition {
                        next_state: self.state,
                        consume: 0,
                        output: Some(Output::NeedMoreData),
                    },
                }
            }
            State::Synced {
                packet_size,
                consecutive_misses,
            } => {
                let bytes = self.buffer.get(consumed..limit).unwrap_or_default();
                match observe(bytes, packet_size, input) {
                    FrameObservation::OnGrid => Transition {
                        next_state: State::Synced {
                            packet_size,
                            consecutive_misses: 0,
                        },
                        consume: packet_size.stride(),
                        output: Some(self.packet_at(consumed, packet_size)),
                    },
                    FrameObservation::SyncByteMissing { .. }
                        if consecutive_misses.saturating_add(1) >= SYNC_LOSS_COUNT =>
                    {
                        Transition {
                            next_state: State::Searching { skipped: 0 },
                            // Why consume 0 (unlike a single miss): the real
                            // grid may restart inside these bytes, so leave
                            // them for the search to rescan.
                            consume: 0,
                            output: Some(Output::SyncLost),
                        }
                    }
                    FrameObservation::SyncByteMissing { found } => Transition {
                        next_state: State::Synced {
                            packet_size,
                            consecutive_misses: consecutive_misses.saturating_add(1),
                        },
                        consume: packet_size.stride(),
                        output: Some(Output::SyncByteMissing { found }),
                    },
                    // Why keep the state at the end, unlike sync loss: an end
                    // of input is not a grid break, so the acquired packet size
                    // still holds if the caller resumes.
                    FrameObservation::TrailingPacket => Transition {
                        next_state: self.state,
                        consume: packet_size.prefix().saturating_add(PACKET_LENGTH),
                        output: Some(self.packet_at(consumed, packet_size)),
                    },
                    // Why report what is left rather than drop it: a byte
                    // dropped without an event breaks the guarantee that
                    // every byte handed over is reported, and a resume must
                    // not join these bytes across the end.
                    FrameObservation::Leftover => Transition {
                        next_state: self.state,
                        consume: bytes.len(),
                        output: Some(Output::Unsynchronized(consumed..limit)),
                    },
                    FrameObservation::NeedMoreData => Transition {
                        next_state: self.state,
                        consume: 0,
                        output: Some(Output::NeedMoreData),
                    },
                }
            }
        }
    }

    /// Reports the run the search walked past, which starts at `consumed`
    fn report_skipped(&self, skipped: usize) -> Transition {
        Transition {
            next_state: State::Searching { skipped: 0 },
            consume: skipped,
            output: Some(Output::Unsynchronized(
                self.consumed..self.consumed.saturating_add(skipped),
            )),
        }
    }

    /// The packet of the unit that starts at `start`
    fn packet_at(&self, start: usize, packet_size: PacketSize) -> Output {
        let packet_start = start.saturating_add(packet_size.prefix());
        Output::Packet {
            range: packet_start..packet_start.saturating_add(PACKET_LENGTH),
            offset: self.offset_of(start),
        }
    }
}

/// Classifies the unit at the front of `bytes` against the locked grid
fn observe(bytes: &[u8], packet_size: PacketSize, input: Input) -> FrameObservation {
    let prefix = packet_size.prefix();
    match (bytes.get(prefix), bytes.len(), input) {
        (Some(&SYNC_BYTE), length, _) if length >= packet_size.stride() => FrameObservation::OnGrid,
        (Some(&found), length, _) if length >= packet_size.stride() => {
            FrameObservation::SyncByteMissing { found }
        }
        // Why wait for the whole unit mid-stream, not only its packet: the
        // unit is consumed whole, so a packet is not cut while the carrier
        // bytes after it (204's parity) may still be on their way.
        (_, _, Input::Continues) => FrameObservation::NeedMoreData,
        // Why a packet, not a whole unit, at the end: no carrier bytes after
        // it will arrive, so the prefix and 188 bytes on the grid are a whole
        // packet; a 204-byte stream cut before its last parity would
        // otherwise lose its last packet.
        (Some(&SYNC_BYTE), length, Input::Ends)
            if length >= prefix.saturating_add(PACKET_LENGTH) =>
        {
            FrameObservation::TrailingPacket
        }
        // Why no empty case: step ends the input once every byte before the
        // end is consumed, so the bytes at the end are never empty here.
        (_, _, Input::Ends) => FrameObservation::Leftover,
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "fixtures are built at constant offsets well inside their length"
)]
mod tests;
