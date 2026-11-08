//! Demuxing a Transport Stream: input of any length in, events out

use alloc::collections::VecDeque;
use core::ops::Range;

use crate::psi::section::Section;
use crate::ts::Pid;

mod analyzer;
mod event;
mod framer;

use analyzer::PacketAnalyzer;
pub use event::{Error, Event};
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
/// reassembles: those of the PAT, the PMTs the PAT names and the CAT typed,
/// each at a version not reported before. Handing input over never fails: a broken stream is reported
/// as [`Event::Error`] and demuxing goes on. The demuxer does no I/O; reading
/// the stream is up to the caller.
///
/// ```
/// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event, PacketSize};
///
/// let stream = include_bytes!("../../tests/fixtures/188.ts");
/// let mut demuxer = Demuxer::new(DemuxerConfig::default());
/// let (mut program_map_pids, mut elementary_pids) = (Vec::new(), Vec::new());
/// let (mut clock_references, mut others) = (0, Vec::new());
/// let mut take = |event: Event<'_>| match event {
///     Event::ProgramAssociationSection(section) => {
///         for program in section.programs.iter().flatten() {
///             program_map_pids.push(program.pid().get());
///         }
///     }
///     Event::TsProgramMapSection { section, .. } => {
///         for stream in section.elementary_streams.iter().flatten() {
///             elementary_pids.push(stream.elementary_pid.get());
///         }
///     }
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
/// // The stream resends its PAT and PMT at one version: each comes once
/// assert_eq!(program_map_pids, [0x1000]);
/// assert_eq!(elementary_pids, [0x100, 0x101]);
/// assert_eq!(clock_references, 3);
/// let acquired = Event::SyncAcquired { packet_size: PacketSize::Standard188 };
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
        // Why not compact here: a queued event can lend a range of the
        // framer's buffer, which compacting would shift.
        self.framer.extend(bytes);
    }

    /// Reads the input as far as the next event and returns it, or `None`
    /// when the input handed over so far holds no more events
    ///
    /// No packet after the one that gave the event is analyzed before the
    /// next call, so what a caller takes up after an event, such as a
    /// subscription to the PIDs a [`Event::TsProgramMapSection`] names,
    /// applies from the packet after it.
    pub fn poll_event(&mut self) -> Option<Event<'_>> {
        // Why queue ranges and lend only at the end: returning a borrow from
        // inside this loop, which steps the framer again on the next turn, is
        // refused by the borrow checker. A packet is analyzed only once the
        // queue is empty, so the bytes the queued ranges point at stay put,
        // in the framer or in a reassembler.
        while self.pending.is_empty() {
            self.framer.compact();
            let pending = match self.framer.step() {
                Output::NeedMoreData => return None,
                Output::Packet { range, .. } => {
                    let start = range.start;
                    self.analyzer
                        .analyze(self.framer.bytes(range), start, &mut self.pending);
                    continue;
                }
                // Why not keep the continuity history and the sections held:
                // after a resynchronization, or across an end of input, the
                // bytes that follow are from an unrelated place in the stream,
                // so the history would report false discontinuities.
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
            // Why convert the section again rather than queue the typed one:
            // the queue holds what a section lends as a range, and the typed
            // sections borrow their loops; the conversion reads the header
            // fields only, and gives on the same bytes what it gave when
            // analyzed.
            Pending::Section { pid, section, body } => self.analyzer.section_event(
                pid,
                &Section {
                    body: match body {
                        Lent::Packet(range) => self.framer.bytes(range),
                        Lent::Reassembled(range) => self.analyzer.reassembled(pid, range),
                    },
                    ..section
                },
            ),
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
    /// packet size found, the PIDs learned from a PAT and the table versions
    /// reported stay, so the same demuxer can then take a stream that goes
    /// on.
    pub fn finish(&mut self) {
        self.framer.finish();
    }
}

#[cfg(test)]
mod tests;
