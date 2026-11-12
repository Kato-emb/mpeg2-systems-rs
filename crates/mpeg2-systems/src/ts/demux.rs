//! Demuxing a Transport Stream: input of any length in, events out

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::framing::{Framer, Next};
use crate::pes::PesPacket;
use crate::psi::section::Section;
use crate::ts::Pid;

mod analyzer;
mod event;
mod framer;
mod pending;

use analyzer::PacketAnalyzer;
pub use event::{Error, Event};
pub use framer::sync::PacketSize;
use framer::{Output, TransportPackets};
use pending::{Lent, Pending};

/// Settings of a [`Demuxer`]
///
/// Build one from the default and set the fields that differ:
///
/// ```
/// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, PacketSize};
///
/// let config = DemuxerConfig::default()
///     .with_packet_size(PacketSize::Timestamped192)
///     .with_subscribe_elementary_pids(true);
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
    /// Whether the demuxer subscribes on its own to the elementary streams
    /// the program maps name (`false` by default)
    ///
    /// When set, the events are those of a caller that, on taking each
    /// [`Event::TsProgramMapSection`], calls [`Demuxer::subscribe_pes`] with
    /// the `elementary_PID` of each entry of its `elementary_streams`
    /// (ISO/IEC 13818-1 §2.4.4.10) whose `stream_type` is not carried in
    /// sections ([`StreamType::is_carried_in_sections`], Table 2-34). So the
    /// subscription applies from the packet after the one that completes
    /// the section; an entry that cannot be read ends the entries taken; a section that fails as a PMT
    /// section subscribes to nothing. A PID a later version leaves out stays
    /// subscribed, and one unsubscribed from with [`Demuxer::unsubscribe_pes`]
    /// is subscribed to again by the next version that names it.
    ///
    /// [`StreamType::is_carried_in_sections`]: crate::stream_type::StreamType::is_carried_in_sections
    pub subscribe_elementary_pids: bool,
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
            subscribe_elementary_pids: false,
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

    /// Returns the value with the given `subscribe_elementary_pids`
    #[must_use]
    pub const fn with_subscribe_elementary_pids(mut self, subscribe_elementary_pids: bool) -> Self {
        self.subscribe_elementary_pids = subscribe_elementary_pids;
        self
    }
}

/// Transport Stream demuxer, which takes input of any length and reports what
/// it finds as events
///
/// The demuxer finds the packet grid (188, 192 or 204 bytes), cuts Transport
/// Packets off it, and reports each byte it could not cut. From each packet it
/// reports a broken `continuity_counter`, a PCR, and the sections it
/// reassembles: those of the PAT, the PMTs the PAT names and the CAT typed,
/// each at a version not reported before, and those of the PIDs
/// [`Demuxer::subscribe_sections`] names; and the PES packets of the PIDs
/// [`Demuxer::subscribe_pes`] names, or, with
/// [`DemuxerConfig::subscribe_elementary_pids`], of the elementary streams
/// the program maps name. Handing input over never fails: a broken
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
    framer: Framer<TransportPackets>,
    analyzer: PacketAnalyzer,
    pending: VecDeque<Pending>,
    subscribe_elementary_pids: bool,
}

impl Demuxer {
    /// Returns a demuxer that has seen no input
    #[must_use]
    pub const fn new(config: DemuxerConfig) -> Self {
        Self {
            framer: Framer::new(TransportPackets::new(config.packet_size)),
            analyzer: PacketAnalyzer::new(config.pes_buffer_limit),
            pending: VecDeque::new(),
            subscribe_elementary_pids: config.subscribe_elementary_pids,
        }
    }

    /// Reassembles the PES packets of `pid` from here on, reported as
    /// [`Event::PesPacket`]
    ///
    /// Reassembly starts at the next packet of the PID with
    /// `payload_unit_start_indicator` set. Subscribing a PID already
    /// subscribed keeps the PES packet in flight. A subscription stays across
    /// [`Error::SyncLost`] and [`Demuxer::finish`]. The demuxer chooses no
    /// PID to subscribe to unless [`DemuxerConfig::subscribe_elementary_pids`]
    /// is set; the elementary streams a [`Event::TsProgramMapSection`] names
    /// are the usual choice, and subscribing on its event takes the PES
    /// packets that follow it.
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

    /// Reassembles the sections of `pid` from here on, reported as
    /// [`Event::Section`]
    ///
    /// Reassembly starts at the next packet of the PID with
    /// `payload_unit_start_indicator` set. Each section is reported as read,
    /// a repeated one and one with `current_next_indicator` `'0'` included,
    /// its `CRC_32` verified when `section_syntax_indicator` is `'1'`; the
    /// demuxer keeps no version of it. Subscribing a PID already subscribed,
    /// or one whose sections the demuxer reassembles anyway (the PAT and CAT
    /// PIDs, and a PID a PAT names for a program map), keeps the section in
    /// flight and changes none of the events of the PID: the PAT, PMT and
    /// CAT sections come typed and by version as before. A subscription
    /// stays across [`Error::SyncLost`] and [`Demuxer::finish`], and across a
    /// PAT that stops naming the PID. It is independent of
    /// [`Demuxer::subscribe_pes`]: a PID subscribed to both is reassembled
    /// both ways. The demuxer chooses no PID to subscribe to for sections,
    /// not even one a program map names with a `stream_type` carried in
    /// sections ([`StreamType::is_carried_in_sections`]).
    ///
    /// [`StreamType::is_carried_in_sections`]: crate::stream_type::StreamType::is_carried_in_sections
    ///
    /// ```
    /// use mpeg2_systems::psi::section::Section;
    /// use mpeg2_systems::psi::TableId;
    /// use mpeg2_systems::ts::Pid;
    /// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
    /// use mpeg2_systems::ts::mux::PacketWriter;
    ///
    /// // A private section on PID 0x0300, which no PAT names, across packets
    /// let pid = Pid::try_from(0x0300)?;
    /// let private_data = [0xaa; 1000];
    /// let mut stream = Vec::new();
    /// PacketWriter::new().write_section(
    ///     pid,
    ///     &Section::new(TableId::from(0x80), &private_data),
    ///     &mut stream,
    /// )?;
    ///
    /// let mut demuxer = Demuxer::new(DemuxerConfig::default());
    /// demuxer.subscribe_sections(pid);
    /// demuxer.handle_input(&stream);
    /// demuxer.finish();
    /// let mut bodies = Vec::new();
    /// while let Some(event) = demuxer.poll_event() {
    ///     if let Event::Section { section, .. } = event {
    ///         bodies.push(section.body.to_vec());
    ///     }
    /// }
    /// assert_eq!(bodies, [private_data]);
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    pub fn subscribe_sections(&mut self, pid: Pid) {
        self.analyzer.subscribe_sections(pid);
    }

    /// Stops reassembling the sections of `pid` that
    /// [`Demuxer::subscribe_sections`] asked for
    ///
    /// The PAT and CAT PIDs, and a PID a PAT names for a program map, are
    /// reassembled as before. On any other PID, the section in flight and
    /// the events of the PID not yet polled that the subscription gave are
    /// dropped without a word. Unsubscribing a PID not subscribed does
    /// nothing. Subscribing again starts at the next
    /// `payload_unit_start_indicator`, as a first subscription does.
    pub fn unsubscribe_sections(&mut self, pid: Pid) {
        self.analyzer.unsubscribe_sections(pid, &mut self.pending);
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
            let output = match self.framer.next() {
                Next::NeedMoreData => return None,
                Next::EndOfInput => {
                    self.analyzer.finish(&mut self.pending);
                    continue;
                }
                Next::Item(output) => output,
            };
            let pending = match output {
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
                Output::SyncAcquired(packet_size) => {
                    Pending::Ready(Event::SyncAcquired { packet_size })
                }
                Output::SyncByteMissing { found, range } => {
                    self.pending
                        .push_back(Pending::Ready(Event::Error(Error::SyncByteMissing {
                            found,
                        })));
                    Pending::Unsynchronized(range)
                }
                Output::Unsynchronized(range) => Pending::Unsynchronized(range),
            };
            self.pending.push_back(pending);
        }
        let pending = self.pending.pop_front()?;
        if let (true, Pending::Section { pid, section, body }) =
            (self.subscribe_elementary_pids, &pending)
        {
            // Why collect the PIDs before subscribing: the event borrows the
            // bytes of the framer or the analyzer, which subscribing borrows
            // mutably.
            let pids: Vec<Pid> = if let Event::TsProgramMapSection { section, .. } =
                self.section_event(*pid, section, body)
            {
                section
                    .elementary_streams
                    .iter()
                    .map_while(Result::ok)
                    .filter(|stream| !stream.stream_type.is_carried_in_sections())
                    .map(|stream| stream.elementary_pid)
                    .collect()
            } else {
                Vec::new()
            };
            for pid in pids {
                self.analyzer.subscribe_pes(pid);
            }
        }
        let event = match pending {
            Pending::Ready(event) => event,
            Pending::Unsynchronized(range) => Event::Unsynchronized {
                bytes: self.framer.bytes(range),
            },
            // Why convert the section again rather than queue the typed one:
            // the queue holds what a section lends as a range, and the typed
            // sections borrow their loops; the conversion reads the header
            // fields only, and gives on the same bytes what it gave when
            // analyzed.
            Pending::Section { pid, section, body } => self.section_event(pid, &section, &body),
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

    /// Returns the event of a section queued whole on `pid`, without the
    /// body it lends
    fn section_event(&self, pid: Pid, section: &Section<'static>, body: &Lent) -> Event<'_> {
        self.analyzer.section_event(
            pid,
            &Section {
                body: match body {
                    Lent::Packet(range) => self.framer.bytes(range.clone()),
                    Lent::Reassembled(range) => self.analyzer.reassembled(pid, range.clone()),
                },
                ..section.clone()
            },
        )
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
    /// the PES and section subscriptions stay, so the same demuxer can then
    /// take a stream that goes on.
    pub fn finish(&mut self) {
        self.framer.finish();
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{Demuxer, DemuxerConfig, Error, Event, PacketSize};
    use crate::pes::{self, PesPacket};
    use crate::psi::cat::CaSection;
    use crate::psi::pat::ProgramAssociationSection;
    use crate::psi::pmt::TsProgramMapSection;
    use crate::psi::section::Section;
    use crate::ts::Pid;

    /// A Transport Packet laid out by hand, its payload padded with 0xFF
    pub(super) fn packet(
        pid: u16,
        payload_unit_start_indicator: bool,
        continuity_counter: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let [high, low] = pid.to_be_bytes();
        let flag = if payload_unit_start_indicator {
            0x40
        } else {
            0x00
        };
        let mut bytes = vec![0x47, high | flag, low, 0x10 | continuity_counter];
        bytes.extend_from_slice(payload);
        bytes.resize(188, 0xFF);
        bytes
    }

    // Why not test through poll_event: bytes the framer keeps after reporting
    // them change no event, only the memory the demuxer holds.
    #[test]
    fn input_handed_over_while_an_event_is_queued_does_not_grow_the_buffer_without_bound() {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        for _ in 0..5 {
            demuxer.handle_input(&packet(0x1FFF, false, 0, &[]));
        }
        // Two short sections per packet, so one stays queued after each poll
        let sections = [
            &[0x00, 0x80, 0x30, 2, 0xAA, 0xAA][..],
            &[0x80, 0x30, 2, 0xBB, 0xBB][..],
        ]
        .concat();
        let mut lengths = Vec::new();
        for continuity_counter in 0..16 {
            demuxer.handle_input(&packet(0x0001, true, continuity_counter, &sections));
            let _ = demuxer.poll_event();
            let _ = demuxer.poll_event();
            lengths.push(demuxer.framer.buffer_len());
        }
        assert!(
            lengths.iter().all(|&length| length <= 2 * 188),
            "{lengths:?}"
        );
    }

    /// Every event the demuxer gives for `stream`, as `Debug` prints it, so the
    /// events can outlive their borrow of the demuxer
    pub(super) fn events_of(demuxer: &mut Demuxer, stream: &[u8]) -> Vec<String> {
        demuxer.handle_input(stream);
        drained(demuxer)
    }

    fn drained(demuxer: &mut Demuxer) -> Vec<String> {
        let mut events = Vec::new();
        while let Some(event) = demuxer.poll_event() {
            events.push(format!("{event:?}"));
        }
        events
    }

    fn described(event: &Event<'_>) -> String {
        format!("{event:?}")
    }

    fn acquired() -> String {
        described(&Event::SyncAcquired {
            packet_size: PacketSize::Standard188,
        })
    }

    /// A long-form section of `table_id`, with `table_id_extension`, at version
    /// `version_number`, in force, its `CRC_32` computed
    fn table_section(
        table_id: u8,
        table_id_extension: u16,
        version_number: u8,
        body: &[u8],
    ) -> Vec<u8> {
        section_of_table(table_id, table_id_extension, version_number, (0, 0), body)
    }

    /// A long-form section as [`table_section`] makes, `section_number` and
    /// `last_section_number` given
    fn section_of_table(
        table_id: u8,
        table_id_extension: u16,
        version_number: u8,
        (section_number, last_section_number): (u8, u8),
        body: &[u8],
    ) -> Vec<u8> {
        let length = u16::try_from(body.len().checked_add(9).unwrap()).unwrap();
        let [high, low] = length.to_be_bytes();
        let [extension_high, extension_low] = table_id_extension.to_be_bytes();
        let mut bytes = vec![
            table_id,
            0xB0 | high,
            low,
            extension_high,
            extension_low,
            0xC1 | (version_number << 1),
            section_number,
            last_section_number,
        ];
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(&crate::crc::crc_32(&bytes).to_be_bytes());
        bytes
    }

    pub(super) fn pat_section(version_number: u8, programs: &[(u16, u16)]) -> Vec<u8> {
        pat_section_of(version_number, (0, 0), programs)
    }

    /// A PAT section as [`pat_section`] makes, `section_number` and
    /// `last_section_number` given
    pub(super) fn pat_section_of(
        version_number: u8,
        numbers: (u8, u8),
        programs: &[(u16, u16)],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        for (program_number, pid) in programs {
            body.extend_from_slice(&program_number.to_be_bytes());
            body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        }
        section_of_table(0x00, 0x0001, version_number, numbers, &body)
    }

    pub(super) fn pmt_section(
        program_number: u16,
        version_number: u8,
        streams: &[(u8, u16)],
    ) -> Vec<u8> {
        let mut body = vec![0xE1, 0x01, 0xF0, 0x00];
        for (stream_type, pid) in streams {
            body.push(*stream_type);
            body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
            body.extend_from_slice(&[0xF0, 0x00]);
        }
        table_section(0x02, program_number, version_number, &body)
    }

    fn cat_section(version_number: u8, descriptors: &[u8]) -> Vec<u8> {
        table_section(0x01, 0xFFFF, version_number, descriptors)
    }

    /// A packet that starts `section` on `pid`
    pub(super) fn section_packet(pid: u16, continuity_counter: u8, section: &[u8]) -> Vec<u8> {
        packet(
            pid,
            true,
            continuity_counter,
            &[&[0x00][..], section].concat(),
        )
    }

    /// Null packets enough to acquire the grid, then `packets`
    pub(super) fn behind_null_packets(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut stream = Vec::new();
        for _ in 0..5 {
            stream.extend(packet(0x1FFF, false, 0, &[]));
        }
        for bytes in packets {
            stream.extend_from_slice(bytes);
        }
        stream
    }

    fn pat_event(bytes: &[u8]) -> String {
        let section = Section::read(bytes).unwrap();
        described(&Event::ProgramAssociationSection(
            ProgramAssociationSection::try_from(&section).unwrap(),
        ))
    }

    fn pmt_event(pid: u16, bytes: &[u8]) -> String {
        let section = Section::read(bytes).unwrap();
        described(&Event::TsProgramMapSection {
            pid: Pid::try_from(pid).unwrap(),
            section: TsProgramMapSection::try_from(&section).unwrap(),
        })
    }

    fn cat_event(bytes: &[u8]) -> String {
        let section = Section::read(bytes).unwrap();
        described(&Event::CaSection(CaSection::try_from(&section).unwrap()))
    }

    fn demux(packets: &[Vec<u8>]) -> Vec<String> {
        events_of(
            &mut Demuxer::new(DemuxerConfig::default()),
            &behind_null_packets(packets),
        )
    }

    #[test]
    fn a_resent_pat_of_the_same_version_reports_once() {
        let pat = pat_section(0, &[(1, 0x0100)]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &pat),
                section_packet(0x0000, 1, &pat)
            ]),
            [acquired(), pat_event(&pat)]
        );
    }

    #[test]
    fn a_new_pat_version_is_reported_again() {
        let first = pat_section(0, &[(1, 0x0100)]);
        let second = pat_section(1, &[(1, 0x0100)]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &first),
                section_packet(0x0000, 1, &second)
            ]),
            [acquired(), pat_event(&first), pat_event(&second)]
        );
    }

    #[test]
    fn a_pat_announcing_its_next_version_is_not_reported() {
        let current = pat_section(0, &[(1, 0x0100)]);
        let in_force = pat_section(1, &[(2, 0x0200)]);
        let (fields, _) = in_force.split_last_chunk::<4>().unwrap();
        let mut next = fields.to_vec();
        *next.get_mut(5).unwrap() &= 0xFE;
        next.extend_from_slice(&crate::crc::crc_32(&next).to_be_bytes());
        let pmt = pmt_section(2, 0, &[]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &current),
                section_packet(0x0000, 1, &next),
                section_packet(0x0000, 2, &current),
                section_packet(0x0200, 0, &pmt),
            ]),
            [acquired(), pat_event(&current)]
        );
    }

    #[test]
    fn a_pmt_on_a_pid_the_pat_names_is_reported() {
        let pat = pat_section(0, &[(1, 0x0100)]);
        let pmt = pmt_section(1, 0, &[(0x1B, 0x0101), (0x0F, 0x0102)]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &pat),
                section_packet(0x0100, 0, &pmt)
            ]),
            [acquired(), pat_event(&pat), pmt_event(0x0100, &pmt)]
        );
    }

    #[test]
    fn a_resent_pmt_of_the_same_version_reports_once() {
        let pat = pat_section(0, &[(1, 0x0100)]);
        let pmt = pmt_section(1, 0, &[(0x1B, 0x0101)]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &pat),
                section_packet(0x0100, 0, &pmt),
                section_packet(0x0100, 1, &pmt),
            ]),
            [acquired(), pat_event(&pat), pmt_event(0x0100, &pmt)]
        );
    }

    #[test]
    fn a_new_pmt_version_is_reported_again() {
        let pat = pat_section(0, &[(1, 0x0100)]);
        let first = pmt_section(1, 0, &[(0x1B, 0x0101)]);
        let second = pmt_section(1, 1, &[(0x1B, 0x0101)]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &pat),
                section_packet(0x0100, 0, &first),
                section_packet(0x0100, 1, &second),
            ]),
            [
                acquired(),
                pat_event(&pat),
                pmt_event(0x0100, &first),
                pmt_event(0x0100, &second)
            ]
        );
    }

    #[test]
    fn pmt_versions_are_tracked_per_program() {
        let pat = pat_section(0, &[(1, 0x0100), (2, 0x0200)]);
        let first_program = pmt_section(1, 3, &[(0x1B, 0x0101)]);
        let second_program = pmt_section(2, 3, &[(0x1B, 0x0201)]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &pat),
                section_packet(0x0100, 0, &first_program),
                section_packet(0x0200, 0, &second_program),
            ]),
            [
                acquired(),
                pat_event(&pat),
                pmt_event(0x0100, &first_program),
                pmt_event(0x0200, &second_program)
            ]
        );
    }

    #[test]
    fn a_program_dropped_from_the_pat_and_re_added_reports_its_pmt_again() {
        let naming_the_program = |version_number| pat_section(version_number, &[(1, 0x0100)]);
        let without_programs = pat_section(1, &[]);
        let pmt = pmt_section(1, 3, &[(0x1B, 0x0101)]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &naming_the_program(0)),
                section_packet(0x0100, 0, &pmt),
                section_packet(0x0000, 1, &without_programs),
                section_packet(0x0000, 2, &naming_the_program(2)),
                section_packet(0x0100, 1, &pmt),
            ]),
            [
                acquired(),
                pat_event(&naming_the_program(0)),
                pmt_event(0x0100, &pmt),
                pat_event(&without_programs),
                pat_event(&naming_the_program(2)),
                pmt_event(0x0100, &pmt),
            ]
        );
    }

    #[test]
    fn a_pmt_on_a_pid_the_pat_no_longer_names_is_not_reported() {
        let naming_the_program = pat_section(0, &[(1, 0x0100)]);
        let without_programs = pat_section(1, &[]);
        let pmt = pmt_section(1, 3, &[(0x1B, 0x0101)]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &naming_the_program),
                section_packet(0x0000, 1, &without_programs),
                section_packet(0x0100, 0, &pmt),
            ]),
            [
                acquired(),
                pat_event(&naming_the_program),
                pat_event(&without_programs)
            ]
        );
    }

    #[test]
    fn a_pmt_that_cannot_be_read_is_reported_as_an_error() {
        let pat = pat_section(0, &[(1, 0x0100)]);
        let pmt = table_section(0x02, 1, 0, &[0xE1, 0x01]);
        let section = Section::read(&pmt).unwrap();
        let error = TsProgramMapSection::try_from(&section).unwrap_err();
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &pat),
                section_packet(0x0100, 0, &pmt)
            ]),
            [
                acquired(),
                pat_event(&pat),
                described(&Event::Error(Error::TsProgramMapSection {
                    pid: Pid::try_from(0x0100).unwrap(),
                    error
                }))
            ]
        );
    }

    #[test]
    fn a_cat_in_the_stream_is_reported() {
        let cat = cat_section(4, &[0x09, 0x04, 0x0B, 0x00, 0x10, 0x20]);
        assert_eq!(
            demux(&[section_packet(0x0001, 0, &cat)]),
            [acquired(), cat_event(&cat)]
        );
    }

    #[test]
    fn a_resent_cat_of_the_same_version_reports_once() {
        let cat = cat_section(0, &[0x09, 0x02, 0x0B, 0x00]);
        assert_eq!(
            demux(&[
                section_packet(0x0001, 0, &cat),
                section_packet(0x0001, 1, &cat)
            ]),
            [acquired(), cat_event(&cat)]
        );
    }

    #[test]
    fn a_new_cat_version_is_reported_again() {
        let first = cat_section(0, &[0x09, 0x02, 0x0B, 0x00]);
        let second = cat_section(1, &[0x09, 0x02, 0x0C, 0x00]);
        assert_eq!(
            demux(&[
                section_packet(0x0001, 0, &first),
                section_packet(0x0001, 1, &second)
            ]),
            [acquired(), cat_event(&first), cat_event(&second)]
        );
    }

    #[test]
    fn a_reported_pat_version_survives_sync_loss_and_the_end_of_input() {
        let pat = pat_section(0, &[(1, 0x0100)]);
        let unsynchronized = [0x00; 188];
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        let mut stream = behind_null_packets(&[section_packet(0x0000, 0, &pat)]);
        stream.extend_from_slice(&unsynchronized);
        stream.extend_from_slice(&unsynchronized);
        stream.extend_from_slice(&behind_null_packets(&[section_packet(0x0000, 0, &pat)]));
        let mut events = events_of(&mut demuxer, &stream);
        demuxer.finish();
        events.extend(drained(&mut demuxer));
        events.extend(events_of(
            &mut demuxer,
            &behind_null_packets(&[section_packet(0x0000, 1, &pat)]),
        ));
        assert_eq!(
            events,
            [
                acquired(),
                pat_event(&pat),
                described(&Event::Error(Error::SyncByteMissing { found: 0x00 })),
                described(&Event::Unsynchronized {
                    bytes: &unsynchronized
                }),
                described(&Event::Error(Error::SyncLost)),
                described(&Event::Unsynchronized {
                    bytes: &unsynchronized
                }),
                acquired(),
            ]
        );
    }

    #[test]
    fn a_pmt_of_a_program_the_pat_does_not_name_is_reported_as_a_section() {
        let pat = pat_section(0, &[(1, 0x0100)]);
        let pmt = pmt_section(2, 0, &[(0x1B, 0x0101)]);
        let section = Section::read(&pmt).unwrap();
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &pat),
                section_packet(0x0100, 0, &pmt)
            ]),
            [
                acquired(),
                pat_event(&pat),
                described(&Event::Section {
                    pid: Pid::try_from(0x0100).unwrap(),
                    section
                })
            ]
        );
    }

    #[test]
    fn a_pmt_that_cannot_be_read_is_reported_once_per_version() {
        let pat = pat_section(0, &[(1, 0x0100)]);
        let pmt = table_section(0x02, 1, 0, &[0xE1, 0x01]);
        let section = Section::read(&pmt).unwrap();
        let error = TsProgramMapSection::try_from(&section).unwrap_err();
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &pat),
                section_packet(0x0100, 0, &pmt),
                section_packet(0x0100, 1, &pmt),
            ]),
            [
                acquired(),
                pat_event(&pat),
                described(&Event::Error(Error::TsProgramMapSection {
                    pid: Pid::try_from(0x0100).unwrap(),
                    error
                }))
            ]
        );
    }

    #[test]
    fn the_programs_of_a_version_stay_named_until_the_next_version_is_whole() {
        let first = pat_section(0, &[(1, 0x0100), (2, 0x0200)]);
        let next_without_program_1 = pat_section_of(1, (0, 1), &[(2, 0x0200)]);
        let next_with_program_1 = pat_section_of(1, (1, 1), &[(1, 0x0100)]);
        let pmt_0 = pmt_section(1, 0, &[(0x1B, 0x0101)]);
        let pmt_1 = pmt_section(1, 1, &[(0x1B, 0x0101)]);
        assert_eq!(
            demux(&[
                section_packet(0x0000, 0, &first),
                section_packet(0x0100, 0, &pmt_0),
                section_packet(0x0000, 1, &next_without_program_1),
                section_packet(0x0100, 1, &pmt_1),
                section_packet(0x0100, 2, &pmt_1),
                section_packet(0x0000, 2, &next_with_program_1),
                section_packet(0x0100, 3, &pmt_1),
            ]),
            [
                acquired(),
                pat_event(&first),
                pmt_event(0x0100, &pmt_0),
                pat_event(&next_without_program_1),
                pmt_event(0x0100, &pmt_1),
                pat_event(&next_with_program_1),
            ]
        );
    }

    #[test]
    fn a_section_a_whole_version_lacks_is_new_again_when_its_version_comes_back() {
        let two_sections = [
            pat_section_of(0, (0, 1), &[(1, 0x0100)]),
            pat_section_of(0, (1, 1), &[(2, 0x0200)]),
        ];
        let pmt = pmt_section(2, 0, &[(0x1B, 0x0201)]);
        let mut continuity_counters = (0_u8..16).cycle();
        let mut on_pat =
            |section: &[u8]| section_packet(0x0000, continuity_counters.next().unwrap(), section);
        let mut packets: Vec<Vec<u8>> =
            two_sections.iter().map(|section| on_pat(section)).collect();
        packets.push(section_packet(0x0200, 0, &pmt));
        for version_number in 1..32 {
            packets.push(on_pat(&pat_section(version_number, &[(1, 0x0100)])));
        }
        packets.extend(two_sections.iter().map(|section| on_pat(section)));
        packets.push(section_packet(0x0200, 1, &pmt));
        let pmts: Vec<String> = demux(&packets)
            .into_iter()
            .filter(|event| event.starts_with("TsProgramMapSection"))
            .collect();
        assert_eq!(pmts, [pmt_event(0x0200, &pmt), pmt_event(0x0200, &pmt)]);
    }

    /// The PID the PES tests subscribe to
    const PES_PID: u16 = 0x0100;

    /// A PES packet of `private_stream_2`, which has no optional header, with
    /// `PES_packet_length` as given and `body` after it
    fn pes(pes_packet_length: u16, body: &[u8]) -> Vec<u8> {
        let [high, low] = pes_packet_length.to_be_bytes();
        [&[0x00, 0x00, 0x01, 0xBF, high, low][..], body].concat()
    }

    /// The payload of a [`packet`] that carries `head`: `head`, then 0xFF to
    /// the end of the packet
    fn stuffed(head: &[u8]) -> Vec<u8> {
        let mut payload = head.to_vec();
        payload.resize(184, 0xFF);
        payload
    }

    /// A demuxer subscribed to [`PES_PID`], with the limit given
    fn subscribed(pes_buffer_limit: usize) -> Demuxer {
        let mut demuxer = Demuxer::new(DemuxerConfig {
            pes_buffer_limit,
            ..DemuxerConfig::default()
        });
        demuxer.subscribe_pes(Pid::try_from(PES_PID).unwrap());
        demuxer
    }

    /// The events of `packets`, behind null packets and before one more, then
    /// those of the end of input
    fn demux_to_the_end(demuxer: &mut Demuxer, packets: &[Vec<u8>]) -> Vec<String> {
        let mut stream = behind_null_packets(packets);
        stream.extend(packet(0x1FFF, false, 0, &[]));
        let mut events = events_of(demuxer, &stream);
        demuxer.finish();
        events.extend(drained(demuxer));
        events
    }

    /// Input offset of the first packet [`behind_null_packets`] puts after them
    const FIRST_PACKET_OFFSET: u64 = 5 * 188;

    #[test]
    fn a_subscribed_pid_reports_its_reassembled_pes_packet() {
        let bytes = pes(3, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(
            demux_to_the_end(&mut subscribed(4096), &[packet(PES_PID, true, 0, &bytes)]),
            [
                acquired(),
                described(&Event::PesPacket {
                    pid: Pid::try_from(PES_PID).unwrap(),
                    packet: PesPacket::read(&bytes).unwrap(),
                    start_offset: FIRST_PACKET_OFFSET,
                }),
            ]
        );
    }

    #[test]
    fn an_unsubscribed_pid_reports_no_pes_packet() {
        assert_eq!(
            demux_to_the_end(
                &mut Demuxer::new(DemuxerConfig::default()),
                &[packet(PES_PID, true, 0, &pes(3, &[0xAA, 0xBB, 0xCC]))]
            ),
            [acquired()]
        );
    }

    #[test]
    fn unsubscribing_reports_nothing_for_the_pid_it_left() {
        let mut demuxer = subscribed(4096);
        let mut events = events_of(
            &mut demuxer,
            &behind_null_packets(&[packet(PES_PID, true, 0, &pes(400, &[0x01, 0x02]))]),
        );
        demuxer.unsubscribe_pes(Pid::try_from(PES_PID).unwrap());
        events.extend(demux_to_the_end(
            &mut demuxer,
            &[
                packet(PES_PID, false, 1, &[0x04, 0x05]),
                packet(PES_PID, true, 2, &pes(3, &[0xAA, 0xBB, 0xCC])),
            ],
        ));
        assert_eq!(events, [acquired()]);
    }

    #[test]
    fn the_end_of_input_reports_a_pes_packet_cut_short_as_truncated() {
        let head = pes(400, &[0x01, 0x02, 0x03]);
        assert_eq!(
            demux_to_the_end(&mut subscribed(4096), &[packet(PES_PID, true, 0, &head)]),
            [
                acquired(),
                described(&Event::TruncatedPesPacket {
                    pid: Pid::try_from(PES_PID).unwrap(),
                    bytes: &stuffed(&head),
                    start_offset: FIRST_PACKET_OFFSET,
                }),
            ]
        );
    }

    #[test]
    fn a_continuity_break_reports_the_pes_packet_in_flight_as_truncated() {
        let head = pes(400, &[0x01, 0x02, 0x03]);
        let pid = Pid::try_from(PES_PID).unwrap();
        assert_eq!(
            demux_to_the_end(
                &mut subscribed(4096),
                &[
                    packet(PES_PID, true, 0, &head),
                    packet(PES_PID, false, 2, &[0x04, 0x05]),
                ]
            ),
            [
                acquired(),
                described(&Event::Error(Error::Discontinuity {
                    pid,
                    expected: crate::ts::ContinuityCounter::try_from(1).unwrap(),
                    found: crate::ts::ContinuityCounter::try_from(2).unwrap(),
                })),
                described(&Event::TruncatedPesPacket {
                    pid,
                    bytes: &stuffed(&head),
                    start_offset: FIRST_PACKET_OFFSET,
                }),
            ]
        );
    }

    #[test]
    fn losing_sync_drops_the_pes_packet_in_flight_without_a_word() {
        let unsynchronized = [0x00; 188];
        let mut stream = behind_null_packets(&[packet(PES_PID, true, 0, &pes(400, &[0x01]))]);
        stream.extend_from_slice(&unsynchronized);
        stream.extend_from_slice(&unsynchronized);
        let mut demuxer = subscribed(4096);
        let mut events = events_of(&mut demuxer, &stream);
        demuxer.finish();
        events.extend(drained(&mut demuxer));
        assert_eq!(
            events,
            [
                acquired(),
                described(&Event::Error(Error::SyncByteMissing { found: 0x00 })),
                described(&Event::Unsynchronized {
                    bytes: &unsynchronized
                }),
                described(&Event::Error(Error::SyncLost)),
                described(&Event::Unsynchronized {
                    bytes: &unsynchronized
                }),
            ]
        );
    }

    #[test]
    fn reassembled_bytes_that_do_not_read_as_a_pes_packet_are_reported_with_the_error() {
        let scrambled = [0xAA, 0xBB, 0xCC, 0xDD, 0x00, 0x00, 0x99];
        assert_eq!(
            demux_to_the_end(
                &mut subscribed(4096),
                &[packet(PES_PID, true, 0, &scrambled)]
            ),
            [
                acquired(),
                described(&Event::UnreadablePesPacket {
                    pid: Pid::try_from(PES_PID).unwrap(),
                    bytes: &stuffed(&scrambled),
                    error: pes::Error::PacketStartCodePrefix {
                        packet_start_code_prefix: 0xAA_BBCC,
                    },
                    start_offset: FIRST_PACKET_OFFSET,
                }),
            ]
        );
    }

    #[test]
    fn a_subscribed_pid_whose_buffer_grows_past_the_limit_reports_an_overrun() {
        assert_eq!(
            demux_to_the_end(
                &mut subscribed(300),
                &[
                    packet(PES_PID, true, 0, &pes(0, &[0x01, 0x02, 0x03])),
                    packet(PES_PID, false, 1, &[]),
                ]
            ),
            [
                acquired(),
                described(&Event::Error(Error::PesBufferOverrun {
                    pid: Pid::try_from(PES_PID).unwrap(),
                    discarded_bytes: 2 * 184,
                })),
            ]
        );
    }

    #[test]
    fn a_packet_with_transport_error_indicator_set_ends_the_pes_packet_in_flight_as_truncated() {
        let head = pes(0, &[0x01, 0x02, 0x03]);
        let mut errored = packet(PES_PID, false, 1, &[0x04, 0x05]);
        *errored.get_mut(1).unwrap() |= 0x80;
        let pid = Pid::try_from(PES_PID).unwrap();
        assert_eq!(
            demux_to_the_end(
                &mut subscribed(4096),
                &[
                    packet(PES_PID, true, 0, &head),
                    errored,
                    packet(PES_PID, false, 2, &[0x06, 0x07]),
                ]
            ),
            [
                acquired(),
                described(&Event::Error(Error::TransportErrorIndicator { pid })),
                described(&Event::TruncatedPesPacket {
                    pid,
                    bytes: &stuffed(&head),
                    start_offset: FIRST_PACKET_OFFSET,
                }),
                described(&Event::Error(Error::Discontinuity {
                    pid,
                    expected: crate::ts::ContinuityCounter::try_from(1).unwrap(),
                    found: crate::ts::ContinuityCounter::try_from(2).unwrap(),
                })),
            ]
        );
    }

    #[test]
    fn a_pes_packet_in_scrambled_transport_packets_is_gathered_to_the_next_unit_start_unread() {
        let scrambled_start = stuffed(&[0xAA, 0xBB, 0xCC, 0xDD, 0x00, 0x01]);
        let continuation = [0x5A; 184];
        let mut first = packet(PES_PID, true, 0, &scrambled_start);
        *first.get_mut(3).unwrap() |= 0xC0;
        let mut second = packet(PES_PID, false, 1, &continuation);
        *second.get_mut(3).unwrap() |= 0xC0;
        let next = pes(1, &[0x0A]);
        let pid = Pid::try_from(PES_PID).unwrap();
        assert_eq!(
            demux_to_the_end(
                &mut subscribed(4096),
                &[first, second, packet(PES_PID, true, 2, &next)]
            ),
            [
                acquired(),
                described(&Event::ScrambledPesPacket {
                    pid,
                    bytes: &[&scrambled_start[..], &continuation[..]].concat(),
                    transport_scrambling_control:
                        crate::ts::packet::TransportScramblingControl::try_from(0b11).unwrap(),
                    start_offset: FIRST_PACKET_OFFSET,
                }),
                described(&Event::PesPacket {
                    pid,
                    packet: PesPacket::read(&next).unwrap(),
                    start_offset: FIRST_PACKET_OFFSET + 2 * 188,
                }),
            ]
        );
    }
}
