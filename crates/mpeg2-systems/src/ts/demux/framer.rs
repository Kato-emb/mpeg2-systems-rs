//! Framing of Transport Packets off the packet grid
//!
//! Every packet is reported with the input offset of its unit, the stride of
//! bytes it takes on the wire, counted from the first byte ever handed to the
//! [`Framer`](crate::framing::Framer); every other byte is reported as
//! unsynchronized.

use core::ops::Range;

use crate::framing::{Buffer, Decoder, Skipped};
use crate::ts::packet::{PACKET_LENGTH, SYNC_BYTE};

pub(super) mod sync;

use sync::{PacketSize, SearchObservation};

// ETSI TR 101 290: declare loss after two consecutive missing sync bytes.
const SYNC_LOSS_COUNT: u8 = 2;

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

/// What the framer saw in the unit at the front of the buffer on the locked
/// grid
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
            let (packet_size, consecutive_misses) = match &mut self.state {
                State::Searching { skipped } => {
                    // Why report before observing again: the report is capped,
                    // so a stream that never synchronizes hands its bytes back
                    // in bounded pieces instead of holding all of them.
                    if skipped.is_full() {
                        return Some(Output::Unsynchronized(buffer.split_to(skipped.report())));
                    }
                    let searched = buffer
                        .unconsumed()
                        .get(skipped.count()..)
                        .unwrap_or_default();
                    let exhausted = searched.is_empty();
                    // Why cap the garbage counted at what the report has room
                    // for: one chunk can carry far more garbage than the
                    // limit, and taking it all would make one report exceed
                    // the bound; counting past the cap would scan bytes only
                    // to scan them again after the report.
                    match (
                        sync::observe(searched, self.fixed_packet_size, skipped.room()),
                        eof,
                    ) {
                        (SearchObservation::LeadingGarbage(count), _) => skipped.skip(count),
                        (SearchObservation::NoGridAtFront, _) => skipped.skip(1),
                        // Why report before acquiring rather than alongside it:
                        // one cut carries one output, so the run walked past
                        // is handed back here and the next cut acquires on the
                        // same bytes.
                        (SearchObservation::GridConfirmed(_), _) if skipped.count() > 0 => {
                            return Some(Output::Unsynchronized(buffer.split_to(skipped.report())));
                        }
                        (SearchObservation::GridConfirmed(packet_size), _) => {
                            self.state = State::Synced {
                                packet_size,
                                consecutive_misses: 0,
                            };
                            return Some(Output::SyncAcquired(packet_size));
                        }
                        (SearchObservation::Undecided, false) => return None,
                        // Why rule an undecided candidate out at the end,
                        // rather than give up the search: no bytes will arrive
                        // to confirm it, and a grid can still start at a later
                        // place in the bytes left.
                        (SearchObservation::Undecided, true) if !exhausted => skipped.skip(1),
                        (SearchObservation::Undecided, true) => {
                            let left = skipped.report_rest(buffer.unconsumed().len());
                            return (left > 0)
                                .then(|| Output::Unsynchronized(buffer.split_to(left)));
                        }
                    }
                    continue;
                }
                State::Synced {
                    packet_size,
                    consecutive_misses,
                } => (*packet_size, *consecutive_misses),
            };
            let front = buffer.unconsumed();
            let left = front.len();
            return match (observe(front, packet_size), eof) {
                (FrameObservation::OnGrid, _) => {
                    self.state = State::Synced {
                        packet_size,
                        consecutive_misses: 0,
                    };
                    Some(packet(buffer, packet_size, packet_size.stride()))
                }
                (FrameObservation::SyncByteMissing { .. }, _)
                    if consecutive_misses.saturating_add(1) >= SYNC_LOSS_COUNT =>
                {
                    // Why cut nothing (unlike a single miss): the real grid
                    // may restart inside these bytes, so leave them for the
                    // search to rescan.
                    self.state = State::Searching {
                        skipped: Skipped::new(),
                    };
                    Some(Output::SyncLost)
                }
                (FrameObservation::SyncByteMissing { found }, _) => {
                    self.state = State::Synced {
                        packet_size,
                        consecutive_misses: consecutive_misses.saturating_add(1),
                    };
                    Some(Output::SyncByteMissing {
                        found,
                        range: buffer.split_to(packet_size.stride()),
                    })
                }
                // Why wait for the whole unit mid-stream, not only its packet:
                // the unit is cut whole, so a packet is not cut while the
                // carrier bytes after it (204's parity) may still be on their
                // way.
                (FrameObservation::PacketWithoutCarrier | FrameObservation::Short, false) => None,
                // Why a packet, not a whole unit, at the end: no carrier bytes
                // after it will arrive, so the prefix and 188 bytes on the
                // grid are a whole packet; a 204-byte stream cut before its
                // last parity would otherwise lose its last packet. The grid
                // stays: an end of input is not a grid break, so the packet
                // size still holds if the caller resumes.
                (FrameObservation::PacketWithoutCarrier, true) => Some(packet(
                    buffer,
                    packet_size,
                    packet_size.prefix().saturating_add(PACKET_LENGTH),
                )),
                // Why report what is left rather than drop it: a byte dropped
                // without an event breaks the guarantee that every byte
                // handed over is reported, and a resume must not join these
                // bytes across the end.
                (FrameObservation::Short, true) => {
                    (left > 0).then(|| Output::Unsynchronized(buffer.split_to(left)))
                }
            };
        }
    }
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

/// Cuts the `length` bytes of a unit that holds a packet
fn packet(buffer: &mut Buffer, packet_size: PacketSize, length: usize) -> Output {
    let start = buffer.split_to(length).start;
    let packet_start = start.saturating_add(packet_size.prefix());
    Output::Packet {
        range: packet_start..packet_start.saturating_add(PACKET_LENGTH),
        offset: buffer.offset_of(start),
    }
}

/// Classifies the unit at the front of `bytes` against the locked grid
fn observe(bytes: &[u8], packet_size: PacketSize) -> FrameObservation {
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

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "fixtures are built at constant offsets well inside their length"
)]
mod tests;
