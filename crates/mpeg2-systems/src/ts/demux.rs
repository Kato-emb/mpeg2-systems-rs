//! Demuxing a Transport Stream: input of any length in, events out

use alloc::collections::VecDeque;
use core::ops::Range;

use crate::pes::PesPacket;
use crate::psi::section::Section;
use crate::ts::Pid;
use crate::ts::packet::TransportScramblingControl;

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
/// let config = DemuxerConfig::default().with_packet_size(PacketSize::Timestamped192);
/// let demuxer = Demuxer::new(config);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DemuxerConfig {
    /// The packet size the stream is known to have, or `None` to detect it
    /// from the input (the default)
    pub packet_size: Option<PacketSize>,
    /// The most bytes a PES packet may gather on one subscribed PID before it
    /// is dropped as [`Error::PesBufferOverrun`] (4 MiB by default)
    ///
    /// The only limit the demuxer takes: a PES packet of a video elementary
    /// stream in a Transport Stream may leave its length unspecified, so
    /// nothing in the stream bounds it. A packet whose `PES_packet_length`
    /// is all in hand is never dropped, though it be longer.
    pub pes_buffer_limit: usize,
}

/// [`DemuxerConfig::pes_buffer_limit`] of the default settings
const DEFAULT_PES_BUFFER_LIMIT: usize = 4 * 1024 * 1024;

impl Default for DemuxerConfig {
    // Why not derive Default: it would set pes_buffer_limit to 0, under which
    // no PES packet longer than one Transport Packet could be reassembled.
    fn default() -> Self {
        Self {
            packet_size: None,
            pes_buffer_limit: DEFAULT_PES_BUFFER_LIMIT,
        }
    }
}

impl DemuxerConfig {
    /// Returns the value with `packet_size` present, set to the one given
    #[must_use]
    pub const fn with_packet_size(mut self, packet_size: PacketSize) -> Self {
        self.packet_size = Some(packet_size);
        self
    }

    /// Returns the value with the given `pes_buffer_limit`
    #[must_use]
    pub const fn with_pes_buffer_limit(mut self, pes_buffer_limit: usize) -> Self {
        self.pes_buffer_limit = pes_buffer_limit;
        self
    }
}

/// An event queued for [`Demuxer::poll_event`], holding the ranges of the
/// bytes it lends rather than the bytes
#[derive(Debug, Clone)]
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
    /// The bytes of a PES packet that ended, read when polled
    PesPacket {
        pid: Pid,
        bytes: Lent,
        start_offset: u64,
    },
    /// A range of the PES reassembly buffer of `pid`: a PES packet whose
    /// Transport Packets are scrambled
    ScrambledPesPacket {
        pid: Pid,
        bytes: Range<usize>,
        transport_scrambling_control: TransportScramblingControl,
        start_offset: u64,
    },
    /// A range of the PES reassembly buffer of `pid`: a PES packet cut short
    TruncatedPesPacket {
        pid: Pid,
        bytes: Range<usize>,
        start_offset: u64,
    },
}

/// Where the bytes a queued section or PES packet lends are
#[derive(Debug, Clone)]
enum Lent {
    /// A range of the framer's buffer: the section or PES packet lay inside
    /// one packet
    Packet(Range<usize>),
    /// A range of the section, or PES, reassembly buffer of the PID: the
    /// section or PES packet crossed packets
    Reassembled(Range<usize>),
}

/// Transport Stream demuxer, which takes input of any length and reports what
/// it finds as events
///
/// The demuxer finds the packet grid (188, 192 or 204 bytes), cuts Transport
/// Packets off it, and reports each byte it could not cut. From each packet it
/// reports a broken `continuity_counter`, a PCR, and the PSI sections it
/// reassembles: those of the PAT, the PMTs the PAT names and the CAT typed,
/// each at a version not reported before; and the PES packets of the PIDs
/// [`Demuxer::subscribe_pes`] names. Handing input over never fails: a broken
/// stream is reported as [`Event::Error`] and demuxing goes on. The demuxer does no I/O; reading
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
#[derive(Debug, Clone)]
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
            analyzer: PacketAnalyzer::new(config.pes_buffer_limit),
            pending: VecDeque::new(),
        }
    }

    /// Reassembles the PES packets of `pid` from here on, reported as
    /// [`Event::PesPacket`]
    ///
    /// Reassembly starts at the next packet of the PID with
    /// `payload_unit_start_indicator` set. Subscribing a PID already
    /// subscribed keeps the PES packet in flight. A subscription stays across
    /// [`Error::SyncLost`] and [`Demuxer::finish`]. The demuxer does not
    /// choose the PIDs to subscribe to; the elementary streams a
    /// [`Event::TsProgramMapSection`] names are the usual choice, and
    /// subscribing on its event takes the PES packets that follow it.
    ///
    /// ```
    /// use mpeg2_systems::ts::Pid;
    /// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
    ///
    /// let stream = include_bytes!("../../tests/fixtures/188.ts");
    /// let mut demuxer = Demuxer::new(DemuxerConfig::default());
    /// let (mut video, mut audio) = (Vec::new(), Vec::new());
    /// let mut take = |demuxer: &mut Demuxer| {
    ///     while let Some(event) = demuxer.poll_event() {
    ///         match event {
    ///             // Subscribe to every elementary stream the program map names
    ///             Event::TsProgramMapSection { section, .. } => {
    ///                 let pids: Vec<Pid> = section
    ///                     .elementary_streams
    ///                     .iter()
    ///                     .flatten()
    ///                     .map(|stream| stream.elementary_pid)
    ///                     .collect();
    ///                 for pid in pids {
    ///                     demuxer.subscribe_pes(pid);
    ///                 }
    ///             }
    ///             Event::PesPacket { pid, packet, .. } if pid.get() == 0x100 => {
    ///                 video.push(packet.payload.len());
    ///             }
    ///             Event::PesPacket { packet, .. } => audio.push(packet.payload.len()),
    ///             _ => {}
    ///         }
    ///     }
    /// };
    ///
    /// // Hand over the stream, then end it: the last video packet, of an
    /// // unspecified length, ends there
    /// demuxer.handle_input(stream);
    /// take(&mut demuxer);
    /// demuxer.finish();
    /// take(&mut demuxer);
    ///
    /// assert_eq!(video, [1803, 1050, 716]);
    /// assert_eq!(audio, [2925, 1881]);
    /// ```
    pub fn subscribe_pes(&mut self, pid: Pid) {
        self.analyzer.subscribe_pes(pid);
    }

    /// Stops reassembling the PES packets of `pid`
    ///
    /// The PES packet in flight on the PID, and the events of the PID not
    /// yet polled that [`Demuxer::subscribe_pes`] gave, are dropped without
    /// a word. Unsubscribing a PID not subscribed does nothing. Subscribing
    /// again starts at the next `payload_unit_start_indicator`, as a first
    /// subscription does.
    pub fn unsubscribe_pes(&mut self, pid: Pid) {
        self.analyzer.unsubscribe_pes(pid, &mut self.pending);
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
                Output::Packet { range, offset } => {
                    let start = range.start;
                    self.analyzer.analyze(
                        self.framer.bytes(range),
                        start,
                        offset,
                        &mut self.pending,
                    );
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
                    self.analyzer.finish(&mut self.pending);
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
            // Why read the PES packet when polled, not when it ends: the
            // queue holds what it lends as a range, and the packet read
            // borrows those bytes.
            Pending::PesPacket {
                pid,
                bytes,
                start_offset,
            } => {
                let bytes = match bytes {
                    Lent::Packet(range) => self.framer.bytes(range),
                    Lent::Reassembled(range) => self.analyzer.pes_bytes(pid, range),
                };
                PesPacket::read(bytes).map_or_else(
                    |error| Event::UnreadablePesPacket {
                        pid,
                        bytes,
                        error,
                        start_offset,
                    },
                    |packet| Event::PesPacket {
                        pid,
                        packet,
                        start_offset,
                    },
                )
            }
            Pending::ScrambledPesPacket {
                pid,
                bytes,
                transport_scrambling_control,
                start_offset,
            } => Event::ScrambledPesPacket {
                pid,
                bytes: self.analyzer.pes_bytes(pid, bytes),
                transport_scrambling_control,
                start_offset,
            },
            Pending::TruncatedPesPacket {
                pid,
                bytes,
                start_offset,
            } => Event::TruncatedPesPacket {
                pid,
                bytes: self.analyzer.pes_bytes(pid, bytes),
                start_offset,
            },
        };
        Some(event)
    }

    /// Ends the input at the bytes handed over so far
    ///
    /// The events [`Demuxer::poll_event`] returns next include a last packet
    /// that lacks only the carrier bytes after it, and every other byte left
    /// as [`Event::Unsynchronized`], so nothing is joined across the end. A
    /// PES packet in flight whose `PES_packet_length` is 0 ends there and is
    /// reported as an [`Event::PesPacket`]; one that has not reached its
    /// length comes as an [`Event::TruncatedPesPacket`]. The sections in
    /// flight and the `continuity_counter` history are dropped at the end.
    /// Poll until `None` before handing over more input. The packet size
    /// found, the PIDs learned from a PAT, the table versions reported and
    /// the PES subscriptions stay, so the same demuxer can then take a stream
    /// that goes on.
    pub fn finish(&mut self) {
        self.framer.finish();
    }
}

#[cfg(test)]
mod tests;
