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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Searching {
        /// Bytes after the consumed ones that the search walked past and has
        /// not reported yet
        // Why not rescan from consumed instead: the run is reported only at a
        // grid, the report limit or the end, so rescanning it on every step
        // costs time quadratic in the run.
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
    /// Every byte before the end [`PacketFramer::finish`] marked has been
    /// reported
    EndOfInput,
}

struct Transition {
    next_state: State,
    /// Bytes that leave the buffer front, already reported or cut
    consume: usize,
    /// `None` keeps [`PacketFramer::step`] stepping
    output: Option<Output>,
}

/// Grid synchronization state machine over the input buffer
#[derive(Debug, Clone)]
pub(super) struct PacketFramer {
    fixed_packet_size: Option<PacketSize>,
    state: State,
    buffer: Vec<u8>,
    /// Bytes at the front of `buffer` that were reported or cut; the ranges
    /// an [`Output`] carries still point at them
    consumed: usize,
    /// Input offset of `buffer[0]`, counted from the first byte ever extended
    position: u64,
    /// [`PacketFramer::finish`] ended the input, and the framer has not yet
    /// reported every byte before the end
    ended: bool,
}

impl PacketFramer {
    pub(super) const fn new(fixed_packet_size: Option<PacketSize>) -> Self {
        Self {
            fixed_packet_size,
            state: State::Searching { skipped: 0 },
            buffer: Vec::new(),
            consumed: 0,
            position: 0,
            ended: false,
        }
    }

    /// Appends input; the ranges outputs carry keep pointing at the same
    /// bytes
    ///
    /// # Panics
    /// Panics if called after [`PacketFramer::finish`] before
    /// [`PacketFramer::step`] has returned [`Output::NeedMoreData`].
    pub(super) fn extend(&mut self, bytes: &[u8]) {
        assert!(
            !self.ended,
            "input handed over after finish, before the events it left were polled"
        );
        // Why copy every chunk, not only the tail that crosses into the
        // next: an event borrows the demuxer rather than the caller's chunk,
        // so poll_event can lend a packet after handle_input has returned.
        self.buffer.extend_from_slice(bytes);
    }

    #[cfg(test)]
    pub(super) fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// Drops the bytes reported or cut, once no output's range points at
    /// them, if they are no fewer than the bytes left
    pub(super) fn compact(&mut self) {
        // Why not a VecDeque, which drops the front without moving the rest:
        // a packet straddling its wrap has no single slice to lend, so every
        // packet would be copied out. Why not compact whenever bytes were
        // consumed: that moves the unread rest of a large chunk on every
        // packet. Compacting only once the consumed bytes are no fewer than
        // the rest moves each byte a bounded number of times in all.
        if self.consumed < self.buffer.len().saturating_sub(self.consumed) {
            return;
        }
        self.buffer.drain(..self.consumed);
        self.position = self.offset_of(self.consumed);
        self.consumed = 0;
    }

    /// Ends the input at the bytes handed over so far
    ///
    /// The steps that follow cut a final packet the grid holds without the
    /// carrier bytes after it, and report every other byte before the end as
    /// [`Output::Unsynchronized`]. The state stays, so input extended once
    /// the end is reported goes on on the grid found.
    pub(super) fn finish(&mut self) {
        self.ended = true;
    }

    /// Returns the bytes of a range an [`Output`] carries
    pub(super) fn bytes(&self, range: Range<usize>) -> &[u8] {
        self.buffer.get(range).unwrap_or_default()
    }

    /// The bytes not yet reported or cut
    fn unconsumed(&self) -> &[u8] {
        self.buffer.get(self.consumed..).unwrap_or_default()
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
            if self.ended && self.consumed == self.buffer.len() {
                self.ended = false;
                return Output::EndOfInput;
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
        let input = if self.ended {
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
                let bytes = self.buffer.get(cursor..).unwrap_or_default();
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
                    SearchObservation::InputExhausted => self.report_skipped(skipped),
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
                let bytes = self.unconsumed();
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
                        output: Some(Output::Unsynchronized(consumed..self.buffer.len())),
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
