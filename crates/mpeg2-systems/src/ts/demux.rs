//! Demuxing a Transport Stream: input of any length in, events out

use alloc::collections::VecDeque;
use core::fmt;
use core::ops::Range;

use crate::psi::section::{self, Section};
use crate::time::ClockReference;
use crate::ts::{ContinuityCounter, Pid, packet};

mod analyzer;
mod framer;

use analyzer::PacketAnalyzer;
pub use framer::sync::PacketSize;
use framer::{Output, PacketFramer};

/// Settings of a [`Demuxer`]
///
/// Build one from the default and set the fields that differ:
///
/// ```
/// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, PacketSize};
///
/// let mut config = DemuxerConfig::default();
/// config.packet_size = Some(PacketSize::Timestamped192);
/// let demuxer = Demuxer::new(config);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DemuxerConfig {
    /// The packet size the stream is known to have, or `None` to detect it
    /// from the input (the default)
    pub packet_size: Option<PacketSize>,
}

/// What a [`Demuxer`] observed in its input
///
/// An event borrows the bytes it carries from the demuxer, until the next call
/// on the demuxer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Event<'a> {
    /// The packet grid was found, and Transport Packets are cut off it from
    /// here on
    SyncAcquired {
        /// The packet size the grid has
        packet_size: PacketSize,
    },
    /// A program clock reference, the 27 MHz time a decoder locks its clock
    /// to, found in a Transport Packet
    ///
    /// Reported for every packet that carries one, whichever PID the program
    /// map names as `PCR_PID`, but not for a packet with
    /// `transport_error_indicator` set. The demuxer does not recover a clock
    /// from the values; that is up to the caller.
    #[doc(alias = "PCR")]
    ProgramClockReference {
        /// The PID of the packet
        pid: Pid,
        /// The value found
        program_clock_reference: ClockReference,
    },
    /// A section reassembled on a PID that carries PSI, its `CRC_32`
    /// verified
    ///
    /// Sections are reassembled on the PAT and CAT PIDs, and on each PID a
    /// PAT names for a program map. A section with `section_syntax_indicator`
    /// `'0'` has no `CRC_32` and is reported as read. The demuxer reports
    /// every section it reassembles, a repeated one included; reading a table
    /// from it is up to the caller.
    Section {
        /// The PID the section was carried on
        pid: Pid,
        /// The section, borrowed from the demuxer
        section: Section<'a>,
    },
    /// Input that was not cut into a Transport Packet
    ///
    /// Reported for the bytes before the grid is found, those the search
    /// walks past after [`Error::SyncLost`], and those [`Demuxer::finish`]
    /// leaves that fill no packet. A long run arrives as several events, so
    /// the bytes the demuxer holds stay bounded. The bytes
    /// of these events and of the packets cut add up to every byte handed to
    /// [`Demuxer::handle_input`].
    Unsynchronized {
        /// The bytes, in input order
        bytes: &'a [u8],
    },
    /// The input broke a rule of the Transport Stream; demuxing goes on
    Error(Error),
}

/// Failure of the Transport Stream a [`Demuxer`] reads, reported as an
/// [`Event::Error`]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// One packet on the grid did not start with `sync_byte` 0x47
    ///
    /// The packet's bytes are passed over and the grid is kept.
    SyncByteMissing {
        /// The byte found where `sync_byte` was expected
        found: u8,
    },
    /// Two packets in a row did not start with `sync_byte` 0x47, and the grid
    /// was dropped
    ///
    /// The search for a grid starts again at the second of them; what it
    /// walks past comes as [`Event::Unsynchronized`].
    SyncLost,
    /// A Transport Packet on the grid could not be read
    ///
    /// Nothing else is taken from the packet.
    Packet {
        /// How the packet failed
        error: packet::Error,
    },
    /// A Transport Packet has `transport_error_indicator` set
    ///
    /// Nothing else is taken from the packet: its counter, PCR and payload
    /// are passed over.
    TransportErrorIndicator {
        /// The PID of the packet
        pid: Pid,
    },
    /// A Transport Packet's `continuity_counter` is not the one expected
    ///
    /// A section being reassembled on the PID is dropped. One repeat of the
    /// previous counter is a duplicate, which ISO/IEC 13818-1 §2.4.3.3
    /// allows; a second repeat is reported here.
    Discontinuity {
        /// The PID of the packet
        pid: Pid,
        /// The counter a continuous stream would carry
        expected: ContinuityCounter,
        /// The counter found
        found: ContinuityCounter,
    },
    /// A section reassembled on a PID that carries PSI could not be read,
    /// its `CRC_32` failing included
    ///
    /// The bytes of the section are dropped.
    Section {
        /// The PID the section was carried on
        pid: Pid,
        /// How the section failed
        error: section::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SyncByteMissing { found } => {
                write!(formatter, "sync_byte {found:#04x} is not 0x47")
            }
            Self::SyncLost => write!(
                formatter,
                "sync_byte is missing from two transport packets in a row"
            ),
            Self::Packet { .. } => write!(formatter, "a transport packet could not be read"),
            Self::TransportErrorIndicator { pid } => write!(
                formatter,
                "transport_error_indicator is set in a transport packet of PID {:#06x}",
                pid.get()
            ),
            Self::Discontinuity {
                pid,
                expected,
                found,
            } => write!(
                formatter,
                "continuity_counter {} of PID {:#06x} is not the expected {}",
                found.get(),
                pid.get(),
                expected.get()
            ),
            Self::Section { pid, .. } => write!(
                formatter,
                "a section on PID {:#06x} could not be read",
                pid.get()
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Packet { error } => Some(error),
            Self::Section { error, .. } => Some(error),
            Self::SyncByteMissing { .. }
            | Self::SyncLost
            | Self::TransportErrorIndicator { .. }
            | Self::Discontinuity { .. } => None,
        }
    }
}

/// An event queued for [`Demuxer::poll_event`], holding the ranges of the
/// bytes it lends rather than the bytes
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Pending {
    /// An event that lends nothing
    Ready(Event<'static>),
    /// A range of the framer's buffer
    Unsynchronized(Range<usize>),
    /// A section read whole, without the body it lends
    Section {
        pid: Pid,
        section: Section<'static>,
        body: Lent,
    },
}

/// Where the body of a queued section is
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Lent {
    /// A range of the framer's buffer: the section lay inside one packet
    Packet(Range<usize>),
    /// A range of the reassembly buffer of the section's PID: the section
    /// crossed packets
    Reassembled(Range<usize>),
}

/// Transport Stream demuxer, which takes input of any length and reports what
/// it finds as events
///
/// The demuxer finds the packet grid (188, 192 or 204 bytes), cuts Transport
/// Packets off it, and reports each byte it could not cut. From each packet it
/// reports a broken `continuity_counter`, a PCR, and the PSI sections it
/// reassembles. Handing input over never fails: a broken stream is reported
/// as [`Event::Error`] and demuxing goes on. The demuxer does no I/O; reading
/// the stream is up to the caller.
///
/// ```
/// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event, PacketSize};
///
/// let stream = include_bytes!("../../tests/fixtures/188.ts");
/// let mut demuxer = Demuxer::new(DemuxerConfig::default());
/// let (mut sections, mut clock_references, mut others) = (0, 0, Vec::new());
/// let mut take = |event: Event<'_>| match event {
///     Event::Section { .. } => sections += 1,
///     Event::ProgramClockReference { .. } => clock_references += 1,
///     other => others.push(format!("{other:?}")),
/// };
///
/// // Hand over the input in any pieces, taking the events after each
/// for chunk in stream.chunks(1000) {
///     demuxer.handle_input(chunk);
///     while let Some(event) = demuxer.poll_event() {
///         take(event);
///     }
/// }
///
/// // End the input and take the events it leaves
/// demuxer.finish();
/// while let Some(event) = demuxer.poll_event() {
///     take(event);
/// }
///
/// let acquired = Event::SyncAcquired { packet_size: PacketSize::Standard188 };
/// assert_eq!((sections, clock_references), (8, 3));
/// assert_eq!(others, [format!("{acquired:?}")]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Demuxer {
    framer: PacketFramer,
    analyzer: PacketAnalyzer,
    pending: VecDeque<Pending>,
}

impl Demuxer {
    /// Returns a demuxer that has seen no input
    #[must_use]
    pub const fn new(config: DemuxerConfig) -> Self {
        Self {
            framer: PacketFramer::new(config.packet_size),
            analyzer: PacketAnalyzer::new(),
            pending: VecDeque::new(),
        }
    }

    /// Hands over the next bytes of the stream, cut anywhere
    ///
    /// The bytes are only kept; [`Demuxer::poll_event`] reads them.
    ///
    /// # Panics
    /// Panics if called after [`Demuxer::finish`] before
    /// [`Demuxer::poll_event`] has returned `None`.
    pub fn handle_input(&mut self, bytes: &[u8]) {
        self.framer.extend(bytes);
    }

    /// Reads the input as far as the next event and returns it, or `None`
    /// when the input handed over so far holds no more events
    pub fn poll_event(&mut self) -> Option<Event<'_>> {
        // Why queue ranges and lend only at the end: returning a borrow from
        // inside this loop, which steps the framer again on the next turn, is
        // refused by the borrow checker. A packet is analyzed only once the
        // queue is empty, so the bytes the queued ranges point at stay put.
        while self.pending.is_empty() {
            self.analyzer.release();
            let pending = match self.framer.step() {
                Output::NeedMoreData => return None,
                Output::Packet { range, .. } => {
                    let start = range.start;
                    self.analyzer
                        .analyze(self.framer.bytes(range), start, &mut self.pending);
                    continue;
                }
                // Why not keep the continuity history and the sections in
                // flight: after a resynchronization, or across an end of
                // input, the bytes that follow are from an unrelated place in
                // the stream, so the history would report false
                // discontinuities.
                Output::SyncLost => {
                    self.analyzer.reset();
                    Pending::Ready(Event::Error(Error::SyncLost))
                }
                Output::EndOfInput => {
                    self.analyzer.reset();
                    continue;
                }
                Output::SyncAcquired(packet_size) => {
                    Pending::Ready(Event::SyncAcquired { packet_size })
                }
                Output::SyncByteMissing { found } => {
                    Pending::Ready(Event::Error(Error::SyncByteMissing { found }))
                }
                Output::Unsynchronized(range) => Pending::Unsynchronized(range),
            };
            self.pending.push_back(pending);
        }
        let event = match self.pending.pop_front()? {
            Pending::Ready(event) => event,
            Pending::Unsynchronized(range) => Event::Unsynchronized {
                bytes: self.framer.bytes(range),
            },
            Pending::Section { pid, section, body } => Event::Section {
                pid,
                section: Section {
                    body: match body {
                        Lent::Packet(range) => self.framer.bytes(range),
                        Lent::Reassembled(range) => self.analyzer.reassembled(pid, range),
                    },
                    ..section
                },
            },
        };
        Some(event)
    }

    /// Ends the input at the bytes handed over so far
    ///
    /// The events [`Demuxer::poll_event`] returns next include a last packet
    /// that lacks only the carrier bytes after it, and every other byte left
    /// as [`Event::Unsynchronized`], so nothing is joined across the end. The
    /// sections in flight and the `continuity_counter` history are dropped
    /// at the end. Poll until `None` before handing over more input. The
    /// packet size found, and the PIDs learned from a PAT, stay, so the same
    /// demuxer can then take a stream that goes on.
    pub fn finish(&mut self) {
        self.framer.finish();
    }
}
