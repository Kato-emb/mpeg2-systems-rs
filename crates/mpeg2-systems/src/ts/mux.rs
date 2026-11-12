//! Writing a Transport Stream: PES packets multiplexed with the PAT, the PMT
//! and the PCRs, or PES packets and sections cut into Transport Packets
//!
//! A Transport Stream is written into a buffer the caller owns, through one
//! of two entry points:
//!
//! - [`Muxer`] takes the PES packets of the streams registered with it, and
//!   sends the PAT, the PMT and the PCRs the stream needs on its own.
//! - [`PacketWriter`] cuts each PES packet or section given into the
//!   Transport Packets of a PID, writes them in the order given, and keeps
//!   the `continuity_counter` of each PID. What to write, and when, is up to
//!   the caller: the PAT and the PMT built with their own types and sent
//!   again as often as the caller chooses, the PCR taken from the caller's
//!   clock, sections the crate does not interpret.
//!
//! The example writes through [`PacketWriter`]; [`Muxer`] has its own.
//!
//! ```
//! use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
//! use mpeg2_systems::psi::VersionNumber;
//! use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
//! use mpeg2_systems::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
//! use mpeg2_systems::stream_type::StreamType;
//! use mpeg2_systems::time::{ClockReference, Timestamp};
//! use mpeg2_systems::ts::Pid;
//! use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
//! use mpeg2_systems::ts::mux::PacketWriter;
//!
//! let (pmt_pid, video_pid) = (Pid::try_from(0x1000)?, Pid::try_from(0x0100)?);
//! let mut writer = PacketWriter::new();
//! let mut stream = Vec::new();
//!
//! // The PAT, naming program 1 and its PMT
//! let mut programs = Vec::new();
//! ProgramLoop::write_from(
//!     [Program::ProgramMap {
//!         program_number: 1.try_into()?,
//!         program_map_pid: pmt_pid,
//!     }],
//!     &mut programs,
//! )?;
//! let pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0)?)
//!     .with_programs(ProgramLoop::from(&programs[..]));
//! writer.write_program_association_section(&pat, &mut stream)?;
//! // The PMT, its PCR on the video PID
//! let mut streams = Vec::new();
//! ElementaryStreamLoop::write_from(
//!     [ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video_pid)],
//!     &mut streams,
//! )?;
//! let pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0)?)
//!     .with_pcr_pid(video_pid)
//!     .with_elementary_streams(ElementaryStreamLoop::from(&streams[..]));
//! writer.write_ts_program_map_section(pmt_pid, &pmt, &mut stream)?;
//! // An access unit, with the PCR of the program
//! let access_unit = vec![0xaa; 1_000];
//! let packet = PesPacket::new(StreamId::from(0xe0), &access_unit)
//!     .with_optional_header(OptionalHeader::new().with_pts(Timestamp::try_from(90_000)?));
//! let pcr = ClockReference::new(81_000, 0)?;
//! assert_eq!(writer.write_pes_packet(video_pid, &packet, Some(pcr), &mut stream)?, 6);
//!
//! // Read back
//! let mut demuxer = Demuxer::new(DemuxerConfig::default());
//! demuxer.subscribe_pes(video_pid);
//! demuxer.handle_input(&stream);
//! demuxer.finish();
//! let mut payloads = Vec::new();
//! while let Some(event) = demuxer.poll_event() {
//!     if let Event::PesPacket { packet, .. } = event {
//!         payloads.push(packet.payload.to_vec());
//!     }
//! }
//! assert_eq!(payloads, [access_unit]);
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```

use alloc::vec::Vec;

use crate::descriptor::DescriptorLoop;
use crate::pes::{PesPacket, StreamId};
use crate::psi::pmt;
use crate::time::{StreamTime, SystemTimeClock, stream_time};
use crate::ts::Pid;
use crate::ts::adaptation_field::AdaptationField;
use crate::ts::packet::PACKET_LENGTH;
use packet_writer::{packet_count, section_packets_length};
use program_definition::ProgramDefinition;
use schedule::{Plan, Schedule, Sending};

mod config;
mod error;
mod packet_writer;
mod program_definition;
mod schedule;
mod transport_packet_fields;

pub use config::MuxerConfig;
pub use error::Error;
pub use packet_writer::PacketWriter;
pub use transport_packet_fields::TransportPacketFields;

/// Multiplexer of PES packets into a Transport Stream, which sends the PAT,
/// the PMT and the PCRs the stream needs on its own
///
/// The program and its streams are registered first, then each PES packet is
/// given with [`handle_pes_packet`](Self::handle_pes_packet), in the order
/// of their DTS (their PTS when they have none) across the streams, and the
/// Transport Packets are taken with [`poll_output`](Self::poll_output). The
/// output may be held until `poll_output` is called; after
/// [`finish`](Self::finish), `poll_output` gives all that is held.
///
/// - The time of the stream is the DTS of each PES packet (its PTS when it
///   has none), held from going back, and counted on past the wrap of the
///   33 bits; a PES packet without either does not move it. The PTS and the
///   DTS are written as given.
/// - The PAT and the PMT, `version_number` 0, are made from what is
///   registered. They are sent before the first PES packet, and again each
///   [`MuxerConfig::psi_interval`] of the time after.
/// - The PCR is sent before the first PES packet, and again each
///   [`MuxerConfig::pcr_interval`] of the time after; its value is the time
///   less [`MuxerConfig::mux_delay`]. One that falls at the time of a PES
///   packet on the PCR PID goes in the adaptation field of that PES packet's
///   first Transport Packet, and any other in a Transport Packet of its own
///   on the PCR PID, before the PES packet. A leap of the time stamps is
///   filled with PCRs, and with PATs and PMTs, at their intervals.
/// - The PCR PID is the one [`Muxer::set_pcr_pid`] sets. Without one,
///   it is the PID of the first video stream registered, or of the first
///   stream when none is video. The muxer takes for video the streams of
///   `stream_type` 0x01, 0x02, 0x10, 0x1B, 0x21, 0x24, 0x32, 0x33 and 0x35,
///   the values Table 2-34 gives a video stream of each coding, and not
///   those it gives only an additional view, an enhancement or another
///   part of a video stream; the value does not tell whether the payload is
///   a sub-bitstream.
///
/// The muxer does not write null packets to keep a constant bit rate.
/// It carries one program only.
/// It does not guarantee that the stream fits the buffers of the T-STD.
/// It does not pace the output in real time; the caller sends it at the pace
/// its PCRs give.
/// It does not check the order of the PES packets across the streams; the
/// caller gives them in the order of their DTS.
///
/// ```
/// use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
/// use mpeg2_systems::psi::pmt::ElementaryStream;
/// use mpeg2_systems::stream_type::StreamType;
/// use mpeg2_systems::time::Timestamp;
/// use mpeg2_systems::ts::Pid;
/// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
/// use mpeg2_systems::ts::mux::{Muxer, MuxerConfig};
///
/// let (pmt_pid, video_pid) = (Pid::try_from(0x1000)?, Pid::try_from(0x0100)?);
/// let mut muxer = Muxer::new(MuxerConfig::new())?;
/// let mut stream = Vec::new();
///
/// // Register the program and its stream
/// muxer.add_program(1, pmt_pid)?;
/// muxer.add_stream(1, ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video_pid))?;
/// // Give an access unit a second in, and take the Transport Packets
/// let access_unit = vec![0xaa; 1_000];
/// let packet = PesPacket::new(StreamId::from(0xe0), &access_unit)
///     .with_optional_header(OptionalHeader::new().with_pts(Timestamp::try_from(90_000)?));
/// muxer.handle_pes_packet(video_pid, &packet)?;
/// muxer.poll_output(&mut stream);
/// // The end, and what is still held
/// muxer.finish();
/// muxer.poll_output(&mut stream);
///
/// // Read back, the demuxer subscribing to the PIDs the PMT names
/// let mut demuxer = Demuxer::new(DemuxerConfig::default().with_subscribe_elementary_pids(true));
/// demuxer.handle_input(&stream);
/// demuxer.finish();
/// let mut payloads = Vec::new();
/// while let Some(event) = demuxer.poll_event() {
///     if let Event::PesPacket { packet, .. } = event {
///         payloads.push(packet.payload.to_vec());
///     }
/// }
/// assert_eq!(payloads, [access_unit]);
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[doc(alias("multiplexer", "mux"))]
#[derive(Debug, Clone)]
pub struct Muxer {
    config: MuxerConfig,
    writer: PacketWriter,
    program: Option<ProgramDefinition>,
    /// When the PAT, the PMT and the PCR were last sent, `None` before the
    /// first PES packet
    // Why not an `Option` for each time, or times of 0 before the first PES
    // packet: one could be set without the others, and 0 could not be told
    // from the time 0.
    schedule: Option<Schedule>,
    /// The Transport Packets [`poll_output`](Self::poll_output) gives next
    // Why not write into the caller's buffer on input: the output may be
    // held for reordering once that is added, and only `handle_pes_packet`
    // can fail.
    pending: Vec<u8>,
    /// The bytes of the PES packet being written, cleared on each input
    /// once the bytes it writes are known to fit `buffer_limit`
    // Why not keep the PAT and the PMT encoded: they are made again from the
    // registration each time, and a reused buffer allocates nothing.
    scratch: Vec<u8>,
    /// The bodies of the PAT and the PMT being written, cleared on each
    /// input that sends them
    // Why not `write` the PAT and the PMT: it allocates a body of its own.
    // Why not `scratch`: the bodies are written before the bytes of an input
    // are known to fit, and they are bounded by what is registered, not by
    // the input.
    section_bodies: Vec<u8>,
    finished: bool,
}

impl Muxer {
    /// Returns a muxer of `config` with nothing registered
    ///
    /// # Errors
    ///
    /// * [`PcrInterval`](Error::PcrInterval):
    ///   [`MuxerConfig::pcr_interval`] is shorter than a tick of the 27 MHz
    ///   system clock, or longer than 0.1 s (§2.7.2, §2.4.2.3 equation 2-5).
    pub fn new(config: MuxerConfig) -> Result<Self, Error> {
        schedule::check_pcr_interval(config.pcr_interval)?;
        Ok(Self {
            config,
            writer: PacketWriter::new(),
            program: None,
            schedule: None,
            pending: Vec::new(),
            scratch: Vec::new(),
            section_bodies: Vec::new(),
            finished: false,
        })
    }

    /// Registers the program, its PMT on `program_map_pid`
    ///
    /// The program has no `program_info` and no `pcr_pid` until
    /// [`set_program_info`](Self::set_program_info) and
    /// [`set_pcr_pid`](Self::set_pcr_pid) set them.
    ///
    /// # Errors
    ///
    /// * [`ProgramNumber`](Error::ProgramNumber): `program_number` is 0.
    /// * [`Pid`](Error::Pid): `program_map_pid` is outside 0x0010 to 0x1FFE,
    ///   the PIDs Table 2-3 leaves free to assign.
    ///
    /// # Panics
    ///
    /// Panics if a program is registered already.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn add_program(&mut self, program_number: u16, program_map_pid: Pid) -> Result<(), Error> {
        assert!(self.program.is_none(), "a program is registered already");
        self.program = Some(ProgramDefinition::new(program_number, program_map_pid)?);
        Ok(())
    }

    /// Sets the PID the PCR of the program `program_number` goes on, in
    /// place of the one [`Muxer`] says
    ///
    /// A later call replaces the PID set before.
    ///
    /// # Errors
    ///
    /// * [`UnregisteredProgram`](Error::UnregisteredProgram): no program of
    ///   `program_number` is registered.
    /// * [`Pid`](Error::Pid): `pcr_pid` is outside 0x0010 to 0x1FFE.
    ///
    /// # Panics
    ///
    /// Panics if a PES packet has been given already.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn set_pcr_pid(&mut self, program_number: u16, pcr_pid: Pid) -> Result<(), Error> {
        assert!(
            self.schedule.is_none(),
            "the PCR PID is set after the first PES packet"
        );
        self.program_mut(program_number)?.set_pcr_pid(pcr_pid)
    }

    /// Sets `program_info`, the descriptors of the program `program_number`,
    /// which the muxer copies
    ///
    /// A later call replaces the descriptors set before. A call refused
    /// leaves the muxer as it was.
    ///
    /// # Errors
    ///
    /// * [`UnregisteredProgram`](Error::UnregisteredProgram): no program of
    ///   `program_number` is registered.
    /// * [`TsProgramMapSection`](Error::TsProgramMapSection):
    ///   [`TsProgramMapSection::write`](pmt::TsProgramMapSection::write) refuses the PMT with
    ///   `program_info` and the streams registered, as it does `program_info`
    ///   that is not whole descriptors, or a PMT past the 1021 bytes of
    ///   `section_length`, which is refused before `program_info` is copied.
    ///
    /// # Panics
    ///
    /// Panics if a PES packet has been given already.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn set_program_info(
        &mut self,
        program_number: u16,
        program_info: DescriptorLoop<'_>,
    ) -> Result<(), Error> {
        assert!(
            self.schedule.is_none(),
            "the program info is set after the first PES packet"
        );
        let transport_stream_id = self.config.transport_stream_id;
        self.program_mut(program_number)?
            .set_program_info(program_info.as_bytes(), transport_stream_id)
    }

    /// Registers `stream`, a stream of the program `program_number`, its PES
    /// packets on its `elementary_pid`
    ///
    /// The muxer copies the `es_info` of `stream`, the descriptors of the
    /// stream as written.
    ///
    /// # Errors
    ///
    /// * [`UnregisteredProgram`](Error::UnregisteredProgram): no program of
    ///   `program_number` is registered.
    /// * [`Pid`](Error::Pid): `elementary_pid` is outside 0x0010 to 0x1FFE.
    /// * [`PidInUse`](Error::PidInUse): `elementary_pid` is that of a stream
    ///   registered already, or of the PMT.
    /// * [`TsProgramMapSection`](Error::TsProgramMapSection):
    ///   [`TsProgramMapSection::write`](pmt::TsProgramMapSection::write) refuses the PMT with the stream, as
    ///   it does `es_info` that is not whole descriptors or is longer than
    ///   `ES_info_length` counts, or a PMT past the 1021 bytes of
    ///   `section_length`; a length past either is refused before `es_info`
    ///   is copied.
    ///
    /// # Panics
    ///
    /// Panics if a PES packet has been given already.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn add_stream(
        &mut self,
        program_number: u16,
        stream: pmt::ElementaryStream<'_>,
    ) -> Result<(), Error> {
        assert!(
            self.schedule.is_none(),
            "a stream is registered after the first PES packet"
        );
        let transport_stream_id = self.config.transport_stream_id;
        self.program_mut(program_number)?
            .add_stream(stream, transport_stream_id)
    }

    /// Returns the program of `program_number`, or
    /// [`UnregisteredProgram`](Error::UnregisteredProgram)
    fn program_mut(&mut self, program_number: u16) -> Result<&mut ProgramDefinition, Error> {
        self.program
            .as_mut()
            .filter(|program| program.program_number() == program_number)
            .ok_or(Error::UnregisteredProgram { program_number })
    }

    /// Takes `packet`, a PES packet of the stream on `pid`, with the PAT, the
    /// PMT and the PCR to send before it, for [`poll_output`](Self::poll_output)
    ///
    /// An input refused leaves the muxer as it was.
    ///
    /// # Errors
    ///
    /// * [`UnregisteredPid`](Error::UnregisteredPid): no stream is
    ///   registered on `pid`.
    /// * [`StreamId`](Error::StreamId): the `stream_id` is below 0xBD, which
    ///   this entry point does not take: 0xBC is the `program_stream_map`,
    ///   and those below it are not PES packets.
    /// * [`PesPacket`](Error::PesPacket): [`PesPacket::write`] refuses the
    ///   packet.
    /// * [`MissingPts`](Error::MissingPts): the PES packet has no PTS and is
    ///   the first of a stream whose `stream_id` carries the optional header
    ///   ([`StreamId::carries_optional_header`]), or has no PTS or DTS and is
    ///   the first given to the muxer. The first PES packet of any other
    ///   stream needs neither.
    /// * [`DecreasingTimestamp`](Error::DecreasingTimestamp): its DTS, or its
    ///   PTS when it has none, is before that of the PES packet given before
    ///   it on its stream.
    /// * [`TimestampGap`](Error::TimestampGap): the PCRs and the tables to
    ///   send since the PES packet before do not fit
    ///   [`MuxerConfig::buffer_limit`], however much `poll_output` takes.
    /// * [`BufferFull`](Error::BufferFull): the Transport Packets would grow
    ///   the output held past [`MuxerConfig::buffer_limit`]; once
    ///   `poll_output` has taken it, the same input may be given again.
    ///
    /// # Panics
    ///
    /// Panics if [`finish`](Self::finish) has been called.
    pub fn handle_pes_packet(&mut self, pid: Pid, packet: &PesPacket<'_>) -> Result<(), Error> {
        self.handle_pes_packet_with_transport_packet_fields(
            pid,
            packet,
            &TransportPacketFields::new(),
        )
    }

    /// Takes `packet`, a PES packet of the stream on `pid`, as
    /// [`handle_pes_packet`](Self::handle_pes_packet) does, its Transport
    /// Packets carrying `fields`
    ///
    /// With `random_access_indicator` set on the PCR PID, the muxer sends a
    /// PCR in the first Transport Packet, since ISO/IEC 13818-1 §2.4.3.5 sets
    /// the indicator on the PCR PID only in a packet that carries one; the
    /// next PCR is [`MuxerConfig::pcr_interval`] after it. A PCR due at the
    /// first Transport Packet that does not fit beside
    /// `transport_private_data` goes, at the same value, in a Transport
    /// Packet of its own on the PCR PID before it. With
    /// [`TransportPacketFields::new`], the output is that of
    /// `handle_pes_packet`.
    ///
    /// ```
    /// use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
    /// use mpeg2_systems::psi::pmt::ElementaryStream;
    /// use mpeg2_systems::stream_type::StreamType;
    /// use mpeg2_systems::time::Timestamp;
    /// use mpeg2_systems::ts::Pid;
    /// use mpeg2_systems::ts::mux::{Muxer, MuxerConfig, TransportPacketFields};
    /// use mpeg2_systems::ts::packet::TransportPacket;
    ///
    /// let (pmt_pid, video_pid) = (Pid::try_from(0x1000)?, Pid::try_from(0x0100)?);
    /// let mut muxer = Muxer::new(MuxerConfig::new())?;
    /// muxer.add_program(1, pmt_pid)?;
    /// muxer.add_stream(1, ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video_pid))?;
    ///
    /// // An access unit that starts a video sequence, a point to begin decoding at
    /// let access_unit = vec![0xaa; 1_000];
    /// let packet = PesPacket::new(StreamId::from(0xe0), &access_unit)
    ///     .with_optional_header(OptionalHeader::new().with_pts(Timestamp::try_from(90_000)?));
    /// let fields = TransportPacketFields::new().with_random_access_indicator(true);
    /// muxer.handle_pes_packet_with_transport_packet_fields(video_pid, &packet, &fields)?;
    /// let mut stream = Vec::new();
    /// muxer.poll_output(&mut stream);
    ///
    /// // The first Transport Packet of the PES packet, after the PAT and the PMT
    /// let first = TransportPacket::read(&stream[2 * 188..3 * 188])?;
    /// let field = first.adaptation_field.unwrap();
    /// assert!(field.random_access_indicator && field.program_clock_reference.is_some());
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// The errors of `handle_pes_packet`, and
    ///
    /// * [`TransportPrivateDataLength`](Error::TransportPrivateDataLength):
    ///   `transport_private_data` is longer than the 180 bytes the adaptation
    ///   field of a Transport Packet with a payload holds, or, with
    ///   `random_access_indicator` set on the PCR PID, than the 174 it holds
    ///   beside the PCR (§2.4.3.5).
    /// * [`RandomAccessIndicator`](Error::RandomAccessIndicator):
    ///   `random_access_indicator` is set on the PCR PID, where it takes a PCR
    ///   in the packet (§2.4.3.5), at no time after the PCR sent before. A PCR
    ///   of the same value would leave the transport rate between the two
    ///   undefined (§2.4.2.3 equation 2-5). The muxer takes the value of a PCR
    ///   from the time stamps (the time of the PES packet's DTS, its PTS
    ///   without one, or the latest given before when it has neither), so it
    ///   has no later value to give. To avoid it, give the PES packet of the
    ///   PCR PID first among those of one time.
    ///
    /// # Panics
    ///
    /// Panics if [`finish`](Self::finish) has been called.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn handle_pes_packet_with_transport_packet_fields(
        &mut self,
        pid: Pid,
        packet: &PesPacket<'_>,
        fields: &TransportPacketFields<'_>,
    ) -> Result<(), Error> {
        assert!(!self.finished, "a PES packet is given after finish");
        let Some((program, stream)) = self
            .program
            .as_ref()
            .and_then(|program| Some((program, program.stream(pid)?)))
        else {
            return Err(Error::UnregisteredPid { pid });
        };
        let last_timestamp = stream.last_timestamp();
        let stream_id = packet.stream_id;
        if !stream_id.is_pes_packet() || stream_id == StreamId::PROGRAM_STREAM_MAP {
            return Err(Error::StreamId { stream_id });
        }
        let Self {
            config,
            writer,
            schedule,
            pending,
            scratch,
            section_bodies,
            ..
        } = self;
        let pes_length = packet.encoded_len();

        let header = packet.optional_header.as_ref();
        let reached = program.latest_timestamp();
        let time = match stream_time(
            schedule.and(reached),
            last_timestamp,
            stream_id.carries_optional_header(),
            header.and_then(|header| header.pts),
            header.and_then(|header| header.dts),
        ) {
            StreamTime::At(time) => Some(time),
            StreamTime::Unchanged => None,
            StreamTime::MissingPts => return Err(Error::MissingPts { pid }),
            StreamTime::Decreasing { time, previous } => {
                return Err(Error::DecreasingTimestamp {
                    pid,
                    timestamp: time.to_timestamp(),
                    previous: previous.to_timestamp(),
                });
            }
        };
        let (clock, previous) = match (schedule.zip(reached), time) {
            (None, Some(clock)) => (clock, None),
            // Why not unreachable!: `stream_time` gives the first PES packet a
            // time, so the arm is never taken, and an Err keeps it panic free.
            (None, None) => return Err(Error::MissingPts { pid }),
            (Some((schedule, reached)), time) => (
                time.map_or(reached, |time| reached.max(time)),
                Some(schedule),
            ),
        };
        // Why not refuse a program without a PCR PID: a stream is registered
        // on `pid`, so the program has one.
        let pcr_pid = program.pcr_pid().unwrap_or(pid);
        let on_pcr_pid = pcr_pid == pid;
        let plan = Plan::new(
            previous,
            clock,
            SystemTimeClock::ticks_in(config.psi_interval),
            SystemTimeClock::ticks_in(config.pcr_interval),
            on_pcr_pid,
        );
        let pcr_at_clock = clock.to_clock_reference(config.mux_delay);
        let pcr_in_packet = fields.pcr_in_first_packet(
            pid,
            on_pcr_pid,
            plan.pcr_due(),
            pcr_at_clock,
            plan.after_last_pcr(),
        )?;
        let adaptation_field = fields.adaptation_field(pcr_in_packet.then_some(pcr_at_clock));
        let plan = plan.with_pcr_in_packet(pcr_in_packet);

        let sections = if plan.psi_count() > 0 {
            Some(program.psi_sections(config.transport_stream_id, section_bodies)?)
        } else {
            None
        };
        let psi_length = sections.as_ref().map_or(0, |(pat, pmt)| {
            section_packets_length(pat.encoded_len())
                .saturating_add(section_packets_length(pmt.encoded_len()))
        });
        let as_length = |count: u64| usize::try_from(count).unwrap_or(usize::MAX);
        let filling = psi_length
            .saturating_mul(as_length(plan.psi_count()))
            .saturating_add(PACKET_LENGTH.saturating_mul(as_length(plan.filling_pcrs())));
        if filling > config.buffer_limit {
            return Err(Error::TimestampGap {
                gap: clock.timestamp_ticks_since(reached.unwrap_or(clock)),
            });
        }
        let length = filling
            .saturating_add(if plan.pcr_split() { PACKET_LENGTH } else { 0 })
            .saturating_add(
                packet_count(
                    pes_length,
                    adaptation_field
                        .as_ref()
                        .map_or(0, AdaptationField::encoded_len),
                )
                .saturating_mul(PACKET_LENGTH),
            )
            .saturating_add(pending.len());
        if length > config.buffer_limit {
            return Err(Error::BufferFull { length });
        }
        scratch.clear();
        packet
            .write(scratch)
            .map_err(|error| Error::PesPacket { error })?;

        for sending in plan.sendings() {
            match (sending, &sections) {
                (Sending::Psi, Some((pat, pmt))) => {
                    writer.write_section(Pid::PAT, pat, pending)?;
                    writer.write_section(program.program_map_pid(), pmt, pending)?;
                }
                (Sending::Psi, None) => {}
                (Sending::Pcr(at), _) => {
                    writer.write_program_clock_reference(
                        pcr_pid,
                        at.to_clock_reference(config.mux_delay),
                        pending,
                    )?;
                }
            }
        }
        writer.write_pes_packet_with(
            pid,
            |out| {
                out.extend_from_slice(scratch);
                Ok(())
            },
            adaptation_field,
            fields.transport_priority,
            pending,
        )?;
        *schedule = Some(plan.next());
        if let (Some(time), Some(program)) = (time, self.program.as_mut()) {
            program.set_last_timestamp(pid, time);
        }
        Ok(())
    }

    /// Appends to `out` all the Transport Packets that can be given
    ///
    /// After [`finish`](Self::finish), a stream that carries one PCR only is
    /// given a second one, [`MuxerConfig::pcr_interval`] after the first or
    /// at the time of the last PES packet if that is later.
    pub fn poll_output(&mut self, out: &mut Vec<u8>) {
        if let (true, Some(schedule), Some(program)) =
            (self.finished, self.schedule.as_mut(), self.program.as_ref())
            && let (Some(pcr_pid), Some(reached)) = (program.pcr_pid(), program.latest_timestamp())
            && let Some(at) = schedule
                .closing_pcr_at(SystemTimeClock::ticks_in(self.config.pcr_interval), reached)
            && self
                .writer
                .write_program_clock_reference(
                    pcr_pid,
                    at.to_clock_reference(self.config.mux_delay),
                    &mut self.pending,
                )
                .is_ok()
        {
            schedule.closing_pcr_sent();
        }
        out.extend_from_slice(&self.pending);
        self.pending.clear();
    }

    /// Declares the end of the input, after which
    /// [`poll_output`](Self::poll_output) gives all that is held
    ///
    /// It writes nothing itself; a second call does nothing.
    pub const fn finish(&mut self) {
        self.finished = true;
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "time stamps and lengths are small constants well inside their range"
)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::num::NonZeroU16;
    use core::time::Duration;

    use super::*;
    use crate::descriptor::DescriptorLoop;
    use crate::pes::OptionalHeader;
    use crate::psi::VersionNumber;
    use crate::psi::pat::{Program as PatProgram, ProgramAssociationSection, ProgramLoop};
    use crate::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
    use crate::psi::section;
    use crate::stream_type::StreamType;
    use crate::time::{ClockReference, Timestamp};
    use crate::ts::packet::TransportPacket;

    const PMT_PID: u16 = 0x1000;
    const VIDEO_PID: u16 = 0x0100;
    const AUDIO_PID: u16 = 0x0101;

    fn pid(value: u16) -> Pid {
        Pid::try_from(value).unwrap()
    }

    /// A PES packet of `stream_id` carrying `payload`, with a DTS (and a PTS
    /// after it) of `dts` 90 kHz ticks, or no time stamp
    fn pes_packet(stream_id: u8, payload: &[u8], dts: Option<u64>) -> PesPacket<'_> {
        let mut header = OptionalHeader::new();
        if let Some(dts) = dts {
            header.pts = Some(Timestamp::try_from(dts + 3_600).unwrap());
            header.dts = Some(Timestamp::try_from(dts).unwrap());
        }
        let mut packet = PesPacket::new(StreamId::from(stream_id), payload);
        packet.optional_header = Some(header);
        packet
    }

    /// A muxer of `config` with program 1 and a video and an audio stream
    fn muxer_of(config: MuxerConfig) -> Muxer {
        let mut muxer = Muxer::new(config).unwrap();
        muxer.add_program(1, pid(PMT_PID)).unwrap();
        muxer
            .add_stream(
                1,
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(VIDEO_PID)),
            )
            .unwrap();
        muxer
            .add_stream(
                1,
                ElementaryStream::new(StreamType::ISO_IEC_13818_3_AUDIO, pid(AUDIO_PID)),
            )
            .unwrap();
        muxer
    }

    /// The 90 kHz ticks the default `mux_delay` comes to
    const DELAY: u64 = 63_000;

    /// The PAT and the PMT [`muxer_of`] sends, written with `writer`
    fn psi(writer: &mut PacketWriter, out: &mut Vec<u8>) {
        let mut programs = Vec::new();
        ProgramLoop::write_from(
            [PatProgram::ProgramMap {
                program_number: NonZeroU16::new(1).unwrap(),
                program_map_pid: pid(PMT_PID),
            }],
            &mut programs,
        )
        .unwrap();
        let mut pat = ProgramAssociationSection::new(1, VersionNumber::from_5_bits(0));
        pat.programs = ProgramLoop::from(programs.as_slice());
        writer.write_program_association_section(&pat, out).unwrap();
        let mut streams = Vec::new();
        ElementaryStreamLoop::write_from(
            [
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(VIDEO_PID)),
                ElementaryStream::new(StreamType::ISO_IEC_13818_3_AUDIO, pid(AUDIO_PID)),
            ],
            &mut streams,
        )
        .unwrap();
        let mut pmt = TsProgramMapSection::new(1, VersionNumber::from_5_bits(0));
        pmt.pcr_pid = Some(pid(VIDEO_PID));
        pmt.elementary_streams = ElementaryStreamLoop::from(streams.as_slice());
        writer
            .write_ts_program_map_section(pid(PMT_PID), &pmt, out)
            .unwrap();
    }

    fn pcr(ticks: u64) -> ClockReference {
        ClockReference::from_fields(ticks / 300, u16::try_from(ticks % 300).unwrap())
    }

    /// What one input put in the output: the PID of each Transport Packet, and
    /// whether it carries a PCR
    fn summary(bytes: &[u8]) -> Vec<(u16, bool)> {
        bytes
            .chunks(188)
            .map(|chunk| {
                let packet = TransportPacket::read(chunk).unwrap();
                let carries_pcr = packet
                    .adaptation_field
                    .is_some_and(|field| field.program_clock_reference.is_some());
                (packet.pid.get(), carries_pcr)
            })
            .collect()
    }

    #[test]
    fn the_first_pes_packet_on_the_pcr_pid_follows_the_pat_and_the_pmt_with_the_pcr_in_its_first_packet()
     {
        let mut muxer = muxer_of(MuxerConfig::new());
        let payload = vec![0xaa; 500];
        let packet = pes_packet(0xe0, &payload, Some(90_000));
        muxer.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);

        let mut writer = PacketWriter::new();
        let mut expected = Vec::new();
        psi(&mut writer, &mut expected);
        writer
            .write_pes_packet(
                pid(VIDEO_PID),
                &packet,
                Some(pcr((90_000 - DELAY) * 300)),
                &mut expected,
            )
            .unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn the_first_pes_packet_off_the_pcr_pid_follows_a_packet_of_the_pcr_alone() {
        let mut muxer = muxer_of(MuxerConfig::new());
        let payload = vec![0xbb; 100];
        let packet = pes_packet(0xc0, &payload, Some(90_000));
        muxer.handle_pes_packet(pid(AUDIO_PID), &packet).unwrap();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);

        let mut writer = PacketWriter::new();
        let mut expected = Vec::new();
        psi(&mut writer, &mut expected);
        writer
            .write_program_clock_reference(
                pid(VIDEO_PID),
                pcr((90_000 - DELAY) * 300),
                &mut expected,
            )
            .unwrap();
        writer
            .write_pes_packet(pid(AUDIO_PID), &packet, None, &mut expected)
            .unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn the_pat_and_the_pmt_and_the_pcr_are_sent_again_once_their_interval_has_passed() {
        let mut muxer = muxer_of(MuxerConfig::new());
        let payload = [0xcc; 10];
        let mut inputs = Vec::new();
        for index in 0..8 {
            let dts = 90_000 + index * 1_800;
            let mut out = Vec::new();
            muxer
                .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &payload, Some(dts)))
                .unwrap();
            muxer.poll_output(&mut out);
            inputs.push(summary(&out));
            out.clear();
            muxer
                .handle_pes_packet(pid(AUDIO_PID), &pes_packet(0xc0, &payload, Some(dts + 900)))
                .unwrap();
            muxer
                .handle_pes_packet(pid(AUDIO_PID), &pes_packet(0xc0, &payload, None))
                .unwrap();
            muxer.poll_output(&mut out);
            inputs.push(summary(&out));
        }

        let (pat, pmt) = ((0, false), (PMT_PID, false));
        let video = |pcr| vec![(VIDEO_PID, pcr)];
        let audio = vec![(AUDIO_PID, false); 2];
        let with_psi = |mut packets: Vec<(u16, bool)>| {
            packets.splice(0..0, [pat, pmt]);
            packets
        };
        assert_eq!(
            inputs,
            [
                with_psi(video(true)),
                audio.clone(),
                video(false),
                audio.clone(),
                video(true),
                audio.clone(),
                video(false),
                audio.clone(),
                video(true),
                audio.clone(),
                with_psi(video(false)),
                audio.clone(),
                video(true),
                audio.clone(),
                video(false),
                audio.clone(),
            ]
        );
    }

    #[test]
    fn without_a_stream_on_the_pcr_pid_the_pcr_goes_alone() {
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        muxer.add_program(1, pid(PMT_PID)).unwrap();
        muxer.set_pcr_pid(1, pid(0x01ff)).unwrap();
        muxer
            .add_stream(
                1,
                ElementaryStream::new(StreamType::AVC_VIDEO, pid(VIDEO_PID)),
            )
            .unwrap();
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[1, 2, 3], Some(90_000)))
            .unwrap();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        assert_eq!(
            summary(&out),
            [
                (0, false),
                (PMT_PID, false),
                (0x01ff, true),
                (VIDEO_PID, false)
            ]
        );
    }

    #[test]
    fn an_input_past_the_buffer_limit_is_refused_and_taken_once_the_output_is() {
        let payload = vec![0xdd; 1_000];
        let first = pes_packet(0xe0, &payload, Some(90_000));
        let second = pes_packet(0xe0, &payload, Some(93_600));
        let mut unbounded = muxer_of(MuxerConfig::new());
        unbounded.handle_pes_packet(pid(VIDEO_PID), &first).unwrap();
        let mut first_output = Vec::new();
        unbounded.poll_output(&mut first_output);

        let mut config = MuxerConfig::new();
        config.buffer_limit = first_output.len();
        let mut bounded = muxer_of(config);
        bounded.handle_pes_packet(pid(VIDEO_PID), &first).unwrap();
        let refused = bounded.handle_pes_packet(pid(VIDEO_PID), &second);
        let mut out = Vec::new();
        bounded.poll_output(&mut out);
        bounded.handle_pes_packet(pid(VIDEO_PID), &second).unwrap();
        bounded.poll_output(&mut out);

        unbounded
            .handle_pes_packet(pid(VIDEO_PID), &second)
            .unwrap();
        let mut expected = first_output.clone();
        unbounded.poll_output(&mut expected);
        assert_eq!(
            (refused, out),
            (
                Err(Error::BufferFull {
                    length: first_output.len() + 6 * 188
                }),
                expected
            )
        );
    }

    #[test]
    fn an_input_alone_past_the_buffer_limit_is_refused() {
        let mut config = MuxerConfig::new();
        config.buffer_limit = 3 * 188;
        let mut muxer = muxer_of(config);
        let payload = vec![0xee; 200];
        let refused =
            muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &payload, Some(90_000)));
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        assert_eq!(
            (refused, out),
            (Err(Error::BufferFull { length: 4 * 188 }), Vec::new())
        );
    }

    #[test]
    fn a_refused_pes_packet_leaves_the_muxer_as_it_was() {
        let mut muxer = muxer_of(MuxerConfig::new());
        let payload = vec![0x11; 70_000];
        let too_long = pes_packet(0xc0, &payload, Some(90_000));
        let too_long_error = too_long.write(&mut Vec::new()).unwrap_err();
        let results = [
            muxer.handle_pes_packet(pid(0x0200), &pes_packet(0xe0, &[], Some(90_000))),
            muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xbb, &[], Some(90_000))),
            muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xbc, &[], Some(90_000))),
            muxer.handle_pes_packet(pid(AUDIO_PID), &too_long),
        ];
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        let mut fresh = muxer_of(MuxerConfig::new());
        let packet = pes_packet(0xe0, &[], Some(90_000));
        muxer.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
        fresh.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
        let (mut written, mut expected) = (Vec::new(), Vec::new());
        muxer.poll_output(&mut written);
        fresh.poll_output(&mut expected);
        assert_eq!(
            (results, out, written),
            (
                [
                    Err(Error::UnregisteredPid { pid: pid(0x0200) }),
                    Err(Error::StreamId {
                        stream_id: StreamId::from(0xbb)
                    }),
                    Err(Error::StreamId {
                        stream_id: StreamId::from(0xbc)
                    }),
                    Err(Error::PesPacket {
                        error: too_long_error
                    }),
                ],
                Vec::new(),
                expected
            )
        );
    }

    #[test]
    fn a_program_is_refused_for_program_number_0_or_a_pid_not_free_to_assign() {
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        let results = [
            muxer.add_program(0, pid(PMT_PID)),
            muxer.add_program(1, pid(0x000f)),
            muxer.add_program(1, pid(PMT_PID)),
        ];
        assert_eq!(
            results,
            [
                Err(Error::ProgramNumber { program_number: 0 }),
                Err(Error::Pid { pid: pid(0x000f) }),
                Ok(()),
            ]
        );
    }

    #[test]
    fn the_pcr_pid_is_refused_for_a_program_not_registered_or_a_pid_not_free_to_assign() {
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        let before_program = muxer.set_pcr_pid(1, pid(VIDEO_PID));
        muxer.add_program(1, pid(PMT_PID)).unwrap();
        assert_eq!(
            [
                before_program,
                muxer.set_pcr_pid(2, pid(VIDEO_PID)),
                muxer.set_pcr_pid(1, Pid::NULL),
            ],
            [
                Err(Error::UnregisteredProgram { program_number: 1 }),
                Err(Error::UnregisteredProgram { program_number: 2 }),
                Err(Error::Pid { pid: Pid::NULL }),
            ]
        );
    }

    #[test]
    fn the_pcr_pid_set_last_is_the_one_the_pcr_goes_on() {
        let output = |pcr_pids: &[u16]| {
            let mut muxer = muxer_of(MuxerConfig::new());
            for &pcr_pid in pcr_pids {
                muxer.set_pcr_pid(1, pid(pcr_pid)).unwrap();
            }
            muxer
                .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[1, 2, 3], Some(90_000)))
                .unwrap();
            let mut out = Vec::new();
            muxer.poll_output(&mut out);
            out
        };
        assert_eq!(output(&[0x01fe, AUDIO_PID]), output(&[AUDIO_PID]));
    }

    #[test]
    fn program_info_is_written_in_the_pmt() {
        let descriptors = [0x05, 0x04, 0x48, 0x44, 0x4d, 0x56];
        let mut muxer = muxer_of(MuxerConfig::new());
        muxer
            .set_program_info(1, DescriptorLoop::from(&[0x05, 0x00][..]))
            .unwrap();
        muxer
            .set_program_info(1, DescriptorLoop::from(descriptors.as_slice()))
            .unwrap();
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[1, 2, 3], Some(90_000)))
            .unwrap();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        let pmt_packet = TransportPacket::read(out.get(188..376).unwrap()).unwrap();
        let section_bytes = pmt_packet.payload.unwrap().get(1..).unwrap();
        let section_length = section::encoded_len(section_bytes).unwrap();
        let section = section::Section::read(section_bytes.get(..section_length).unwrap()).unwrap();
        assert_eq!(
            TsProgramMapSection::try_from(&section)
                .unwrap()
                .program_info,
            DescriptorLoop::from(descriptors.as_slice())
        );
    }

    #[test]
    fn program_info_refused_leaves_the_muxer_as_it_was() {
        let broken = [0x05, 0x04, 0x00];
        let mut pmt = TsProgramMapSection::new(1, VersionNumber::from_5_bits(0));
        pmt.program_info = broken.as_slice().into();
        let broken_error = pmt.write(&mut Vec::new()).unwrap_err();
        let past_the_limit = descriptors(1_009);
        let output = |refused: &[&[u8]]| {
            let mut muxer = muxer_of(MuxerConfig::new());
            let errors: Vec<_> = refused
                .iter()
                .map(|program_info| muxer.set_program_info(1, DescriptorLoop::from(*program_info)))
                .collect();
            muxer
                .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[1, 2, 3], Some(90_000)))
                .unwrap();
            let mut out = Vec::new();
            muxer.poll_output(&mut out);
            (errors, out)
        };
        let (errors, out) = output(&[&broken, &past_the_limit]);
        assert_eq!(
            (
                errors,
                Muxer::new(MuxerConfig::new())
                    .unwrap()
                    .set_program_info(1, DescriptorLoop::from(&[][..])),
                out
            ),
            (
                vec![
                    Err(Error::TsProgramMapSection {
                        error: broken_error
                    }),
                    Err(Error::TsProgramMapSection {
                        error: pmt::Error::Section(section::Error::SectionLength {
                            section_length: 1_032,
                            limit: 1_021
                        })
                    }),
                ],
                Err(Error::UnregisteredProgram { program_number: 1 }),
                output(&[]).1
            )
        );
    }

    #[test]
    fn a_stream_is_refused_off_the_program_on_a_pid_in_use_or_for_its_descriptors() {
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        let before_the_program = muxer.add_stream(
            1,
            ElementaryStream::new(StreamType::AVC_VIDEO, pid(VIDEO_PID)),
        );
        muxer.add_program(1, pid(PMT_PID)).unwrap();
        muxer
            .add_stream(
                1,
                ElementaryStream::new(StreamType::AVC_VIDEO, pid(VIDEO_PID)),
            )
            .unwrap();
        let too_long = vec![0u8; 1_024];
        let results = [
            before_the_program,
            muxer.add_stream(
                2,
                ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(AUDIO_PID)),
            ),
            muxer.add_stream(
                1,
                ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, Pid::NULL),
            ),
            muxer.add_stream(
                1,
                ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(VIDEO_PID)),
            ),
            muxer.add_stream(
                1,
                ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(PMT_PID)),
            ),
            muxer.add_stream(
                1,
                ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(AUDIO_PID))
                    .with_es_info(DescriptorLoop::from(too_long.as_slice())),
            ),
            muxer.add_stream(
                1,
                ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(AUDIO_PID)),
            ),
        ];
        assert_eq!(
            results,
            [
                Err(Error::UnregisteredProgram { program_number: 1 }),
                Err(Error::UnregisteredProgram { program_number: 2 }),
                Err(Error::Pid { pid: Pid::NULL }),
                Err(Error::PidInUse {
                    pid: pid(VIDEO_PID)
                }),
                Err(Error::PidInUse { pid: pid(PMT_PID) }),
                Err(Error::TsProgramMapSection {
                    error: pmt::Error::EsInfoLength { length: 1_024 }
                }),
                Ok(()),
            ]
        );
    }

    #[test]
    #[should_panic(expected = "a program is registered already")]
    fn a_second_program_panics() {
        let mut muxer = muxer_of(MuxerConfig::new());
        let _ = muxer.add_program(2, pid(0x1001));
    }

    #[test]
    #[should_panic(expected = "a stream is registered after the first PES packet")]
    fn a_stream_after_the_first_pes_packet_panics() {
        let mut muxer = muxer_of(MuxerConfig::new());
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[], Some(90_000)))
            .unwrap();
        let _ = muxer.add_stream(1, ElementaryStream::new(StreamType::AVC_VIDEO, pid(0x0102)));
    }

    #[test]
    #[should_panic(expected = "the PCR PID is set after the first PES packet")]
    fn the_pcr_pid_after_the_first_pes_packet_panics() {
        let mut muxer = muxer_of(MuxerConfig::new());
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[], Some(90_000)))
            .unwrap();
        let _ = muxer.set_pcr_pid(1, pid(AUDIO_PID));
    }

    #[test]
    #[should_panic(expected = "the program info is set after the first PES packet")]
    fn program_info_after_the_first_pes_packet_panics() {
        let mut muxer = muxer_of(MuxerConfig::new());
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[], Some(90_000)))
            .unwrap();
        let _ = muxer.set_program_info(1, DescriptorLoop::from(&[][..]));
    }

    #[test]
    #[should_panic(expected = "a PES packet is given after finish")]
    fn a_pes_packet_after_finish_panics() {
        let mut muxer = muxer_of(MuxerConfig::new());
        muxer.finish();
        let _ = muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[], Some(90_000)));
    }

    #[test]
    fn after_finish_poll_output_gives_what_is_held_and_a_second_finish_does_nothing() {
        let mut muxer = muxer_of(MuxerConfig::new());
        let packet = pes_packet(0xe0, &[1], Some(90_000));
        muxer.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
        muxer.finish();
        muxer.finish();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        let mut fresh = muxer_of(MuxerConfig::new());
        fresh.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
        fresh.finish();
        let mut expected = Vec::new();
        fresh.poll_output(&mut expected);
        assert_eq!(out, expected);
    }

    #[test]
    fn the_pcr_is_the_dts_less_the_mux_delay_and_never_goes_back() {
        let mut config = MuxerConfig::new();
        config.pcr_interval = Duration::from_millis(100);
        let mut muxer = muxer_of(config);
        let mut pcrs = Vec::new();
        for (stream_pid, dts) in [
            (VIDEO_PID, 90_000),
            (VIDEO_PID, 99_000),
            (AUDIO_PID, 95_000),
            (VIDEO_PID, 108_000),
        ] {
            muxer
                .handle_pes_packet(pid(stream_pid), &pes_packet(0xe0, &[0], Some(dts)))
                .unwrap();
            let mut out = Vec::new();
            muxer.poll_output(&mut out);
            pcrs.extend(out.chunks(188).filter_map(|chunk| {
                TransportPacket::read(chunk)
                    .unwrap()
                    .adaptation_field
                    .and_then(|field| field.program_clock_reference)
                    .map(ClockReference::ticks)
            }));
        }
        assert_eq!(pcrs, [27_000 * 300, 36_000 * 300, 45_000 * 300]);
    }

    /// A PES packet of `stream_id` whose PTS and DTS are both `timestamp`, taken
    /// round the 33-bit circle
    fn stamped(stream_id: u8, timestamp: u64) -> PesPacket<'static> {
        let timestamp = Timestamp::try_from(timestamp % Timestamp::MODULUS).unwrap();
        let mut header = OptionalHeader::new();
        header.pts = Some(timestamp);
        header.dts = Some(timestamp);
        let mut packet = PesPacket::new(StreamId::from(stream_id), &[0x5a; 10]);
        packet.optional_header = Some(header);
        packet
    }

    /// How many PCRs `stream` carries, the longest time from one to the next
    /// round the circle, and how many PATs it carries
    fn timing_of(stream: &[u8]) -> (usize, u64, usize) {
        let packets: Vec<TransportPacket<'_>> = stream
            .chunks(188)
            .map(|chunk| TransportPacket::read(chunk).unwrap())
            .collect();
        let pcrs: Vec<u64> = packets
            .iter()
            .filter_map(|packet| {
                packet
                    .adaptation_field
                    .as_ref()
                    .and_then(|field| field.program_clock_reference)
                    .map(ClockReference::ticks)
            })
            .collect();
        let longest = pcrs
            .windows(2)
            .map(|pair| match pair {
                [earlier, later] => {
                    (later + ClockReference::MODULUS - earlier) % ClockReference::MODULUS
                }
                _ => 0,
            })
            .max()
            .unwrap_or(0);
        let pats = packets
            .iter()
            .filter(|packet| packet.pid == Pid::PAT)
            .count();
        (pcrs.len(), longest, pats)
    }

    /// The stream a muxer of the default settings writes of one video stream
    /// whose PTS and DTS are each of `timestamps`
    fn multiplexed(timestamps: impl IntoIterator<Item = u64>) -> Vec<u8> {
        let mut muxer = muxer_of(MuxerConfig::new());
        let mut out = Vec::new();
        for timestamp in timestamps {
            muxer
                .handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, timestamp))
                .unwrap();
            muxer.poll_output(&mut out);
        }
        out
    }

    /// 27 MHz ticks of 40 ms, the default `pcr_interval`
    const PCR_INTERVAL: u64 = 40 * 27_000;

    #[test]
    fn time_stamps_below_the_mux_delay_keep_the_pcr_and_the_pat_and_the_pmt_coming() {
        let stream = multiplexed((0..=100).map(|index| index * 1_800));
        assert_eq!(timing_of(&stream), (51, PCR_INTERVAL, 21));
    }

    #[test]
    fn time_stamps_across_the_wrap_of_33_bits_keep_the_pcr_and_the_pat_and_the_pmt_coming() {
        let stream =
            multiplexed((0..=100).map(|index| Timestamp::MODULUS - 45_000 + index * 1_800));
        assert_eq!(timing_of(&stream), (51, PCR_INTERVAL, 21));
    }

    #[test]
    fn a_pes_packet_past_the_buffer_limit_is_refused_before_it_is_copied() {
        let mut config = MuxerConfig::new();
        config.buffer_limit = 3 * 188;
        let mut muxer = muxer_of(config);
        let payload = vec![0x77; 10_000];
        let refused =
            muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &payload, Some(90_000)));
        assert_eq!((refused.is_err(), muxer.scratch.capacity()), (true, 0));
    }

    #[test]
    fn a_leap_of_the_time_stamps_is_filled_with_pcrs_and_the_pat_and_the_pmt_at_their_intervals() {
        let stream = multiplexed([90_000, 180_000]);
        assert_eq!(timing_of(&stream), (26, PCR_INTERVAL, 11));
    }

    #[test]
    fn a_leap_too_long_to_fill_within_the_buffer_limit_is_refused_however_much_is_taken() {
        let mut config = MuxerConfig::new();
        config.buffer_limit = 100 * 188;
        let mut muxer = muxer_of(config);
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 90_000))
            .unwrap();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        let leap = stamped(0xe0, 90_000 + 10 * 90_000);
        let refused = muxer.handle_pes_packet(pid(VIDEO_PID), &leap);
        muxer.poll_output(&mut out);
        let refused_again = muxer.handle_pes_packet(pid(VIDEO_PID), &leap);
        let next = stamped(0xe0, 91_800);
        muxer.handle_pes_packet(pid(VIDEO_PID), &next).unwrap();
        muxer.poll_output(&mut out);

        let mut expected = Vec::new();
        let mut fresh = muxer_of(config);
        for packet in [stamped(0xe0, 90_000), next] {
            fresh.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
            fresh.poll_output(&mut expected);
        }
        let gap = Err(Error::TimestampGap { gap: 10 * 90_000 });
        assert_eq!((refused, refused_again, out), (gap, gap, expected));
    }

    /// A PES packet of `stream_id` without a PTS or a DTS
    fn unstamped(stream_id: u8) -> PesPacket<'static> {
        let mut packet = PesPacket::new(StreamId::from(stream_id), &[0x5a; 10]);
        packet.optional_header = Some(OptionalHeader::new());
        packet
    }

    #[test]
    fn a_stream_whose_first_pes_packet_has_no_pts_or_whose_dts_goes_back_is_refused() {
        let mut muxer = muxer_of(MuxerConfig::new());
        let results = [
            muxer.handle_pes_packet(pid(VIDEO_PID), &unstamped(0xe0)),
            muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 180_000)),
            muxer.handle_pes_packet(pid(AUDIO_PID), &unstamped(0xc0)),
            muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 90_000)),
            muxer.handle_pes_packet(pid(AUDIO_PID), &stamped(0xc0, 90_000)),
            muxer.handle_pes_packet(pid(VIDEO_PID), &unstamped(0xe0)),
            muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 181_800)),
        ];
        let mut out = Vec::new();
        muxer.poll_output(&mut out);

        let mut fresh = muxer_of(MuxerConfig::new());
        let mut expected = Vec::new();
        for (stream_pid, packet) in [
            (VIDEO_PID, stamped(0xe0, 180_000)),
            (AUDIO_PID, stamped(0xc0, 90_000)),
            (VIDEO_PID, unstamped(0xe0)),
            (VIDEO_PID, stamped(0xe0, 181_800)),
        ] {
            fresh.handle_pes_packet(pid(stream_pid), &packet).unwrap();
        }
        fresh.poll_output(&mut expected);
        let timestamp = |value| Timestamp::try_from(value).unwrap();
        assert_eq!(
            (results, out),
            (
                [
                    Err(Error::MissingPts {
                        pid: pid(VIDEO_PID)
                    }),
                    Ok(()),
                    Err(Error::MissingPts {
                        pid: pid(AUDIO_PID)
                    }),
                    Err(Error::DecreasingTimestamp {
                        pid: pid(VIDEO_PID),
                        timestamp: timestamp(90_000),
                        previous: timestamp(180_000),
                    }),
                    Ok(()),
                    Ok(()),
                    Ok(()),
                ],
                expected
            )
        );
    }

    #[test]
    fn a_dts_past_the_wrap_of_33_bits_does_not_go_back() {
        let mut muxer = muxer_of(MuxerConfig::new());
        let results = [Timestamp::MODULUS - 1_800, Timestamp::MODULUS + 1_800]
            .map(|timestamp| muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, timestamp)));
        assert_eq!(results, [Ok(()), Ok(())]);
    }

    #[test]
    fn a_stream_of_one_pcr_ends_with_a_second_one_pcr_interval_on_and_a_longer_one_with_none_added()
    {
        let finished = |timestamps: &[u64]| {
            let mut muxer = muxer_of(MuxerConfig::new());
            let mut out = Vec::new();
            for &timestamp in timestamps {
                muxer
                    .handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, timestamp))
                    .unwrap();
            }
            muxer.poll_output(&mut out);
            let before = timing_of(&out).0;
            muxer.finish();
            muxer.poll_output(&mut out);
            muxer.poll_output(&mut out);
            let pcrs: Vec<u64> = out
                .chunks(188)
                .filter_map(|chunk| {
                    TransportPacket::read(chunk)
                        .unwrap()
                        .adaptation_field
                        .and_then(|field| field.program_clock_reference)
                        .map(ClockReference::ticks)
                })
                .collect();
            (before, pcrs.len(), pcrs.last().copied())
        };
        let first = (90_000 - DELAY) * 300;
        assert_eq!(
            [
                finished(&[90_000]),
                finished(&[90_000, 91_800, 93_600, 95_400])
            ],
            [
                (1, 2, Some(first + PCR_INTERVAL)),
                (2, 2, Some(first + PCR_INTERVAL))
            ]
        );
    }

    #[test]
    fn a_mux_delay_longer_than_the_time_stamp_gives_the_pcr_round_the_circle() {
        let mut config = MuxerConfig::new();
        config.mux_delay = Duration::from_secs(96_000);
        let mut muxer = muxer_of(config);
        let mut header = OptionalHeader::new();
        header.pts = Some(Timestamp::try_from(0).unwrap());
        let mut packet = PesPacket::new(StreamId::from(0xe0), &[1]);
        packet.optional_header = Some(header);
        muxer.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        let pcrs: Vec<u64> = out
            .chunks(188)
            .filter_map(|chunk| {
                TransportPacket::read(chunk)
                    .unwrap()
                    .adaptation_field
                    .and_then(|field| field.program_clock_reference)
                    .map(ClockReference::ticks)
            })
            .collect();
        let delay = 96_000 * 27_000_000 % ClockReference::MODULUS;
        assert_eq!(pcrs, [ClockReference::MODULUS - delay]);
    }

    /// The shortest `pcr_interval` a muxer takes: one tick of the 27 MHz system
    /// clock, rounded up to whole nanoseconds
    const ONE_TICK: Duration = Duration::from_nanos(38);

    #[test]
    fn a_pcr_interval_of_no_tick_of_the_stc_or_past_0_1_s_is_refused() {
        let with = |pcr_interval| {
            let mut config = MuxerConfig::new();
            config.pcr_interval = pcr_interval;
            Muxer::new(config).map(|_| ())
        };
        let refused = |pcr_interval| Err(Error::PcrInterval { pcr_interval });
        let longest = Duration::from_millis(100);
        let past_longest = longest + Duration::from_nanos(1);
        assert_eq!(
            [
                Duration::ZERO,
                Duration::from_nanos(1),
                past_longest,
                longest,
                ONE_TICK
            ]
            .map(with),
            [
                refused(Duration::ZERO),
                refused(Duration::from_nanos(1)),
                refused(past_longest),
                Ok(()),
                Ok(())
            ]
        );
    }

    #[test]
    fn the_shortest_pcr_interval_never_sends_two_pcrs_of_one_value() {
        let mut config = MuxerConfig::new();
        config.pcr_interval = ONE_TICK;
        let mut muxer = muxer_of(config);
        let mut out = Vec::new();
        for (stream_pid, stream_id, timestamp) in [
            (VIDEO_PID, 0xe0, 90_000),
            (AUDIO_PID, 0xc0, 90_000),
            (VIDEO_PID, 0xe0, 90_001),
        ] {
            muxer
                .handle_pes_packet(pid(stream_pid), &stamped(stream_id, timestamp))
                .unwrap();
            muxer.poll_output(&mut out);
        }
        muxer.finish();
        muxer.poll_output(&mut out);
        let pcrs: Vec<u64> = out
            .chunks(188)
            .filter_map(|chunk| {
                TransportPacket::read(chunk)
                    .unwrap()
                    .adaptation_field
                    .and_then(|field| field.program_clock_reference)
                    .map(ClockReference::ticks)
            })
            .collect();
        let rising = pcrs.windows(2).all(|pair| match pair {
            [earlier, later] => later > earlier,
            _ => false,
        });
        assert_eq!((pcrs.len(), rising), (301, true));
    }

    /// For each input given a muxer of `config`, how many PATs and PMTs its
    /// output carries, and whether they all come before the PES packet
    fn psi_of_each(config: MuxerConfig, inputs: &[(u16, u8, u64)]) -> Vec<(usize, usize, bool)> {
        let mut muxer = muxer_of(config);
        inputs
            .iter()
            .map(|&(stream_pid, stream_id, timestamp)| {
                muxer
                    .handle_pes_packet(pid(stream_pid), &stamped(stream_id, timestamp))
                    .unwrap();
                let mut out = Vec::new();
                muxer.poll_output(&mut out);
                let packets: Vec<TransportPacket<'_>> = out
                    .chunks(188)
                    .map(|chunk| TransportPacket::read(chunk).unwrap())
                    .collect();
                let is_psi = |packet: &TransportPacket<'_>| {
                    packet.pid == Pid::PAT || packet.pid == pid(PMT_PID)
                };
                let pes_start = packets
                    .iter()
                    .position(|packet| {
                        packet.pid == pid(stream_pid) && packet.payload_unit_start_indicator
                    })
                    .unwrap();
                (
                    packets
                        .iter()
                        .filter(|packet| packet.pid == Pid::PAT)
                        .count(),
                    packets
                        .iter()
                        .filter(|packet| packet.pid == pid(PMT_PID))
                        .count(),
                    packets.iter().skip(pes_start).all(|packet| !is_psi(packet)),
                )
            })
            .collect()
    }

    #[test]
    fn a_psi_interval_of_0_sends_the_pat_and_the_pmt_before_every_pes_packet() {
        let mut config = MuxerConfig::new();
        config.psi_interval = Duration::ZERO;
        let inputs = [
            (VIDEO_PID, 0xe0, 90_000),
            (AUDIO_PID, 0xc0, 90_000),
            (VIDEO_PID, 0xe0, 91_800),
            (VIDEO_PID, 0xe0, 181_800),
        ];
        assert_eq!(psi_of_each(config, &inputs), vec![(1, 1, true); 4]);
    }

    #[test]
    fn a_psi_interval_of_duration_max_sends_the_pat_and_the_pmt_before_the_first_pes_packet_only() {
        let mut config = MuxerConfig::new();
        config.psi_interval = Duration::MAX;
        let inputs: Vec<(u16, u8, u64)> = (0..1_000)
            .map(|index| (VIDEO_PID, 0xe0, 90_000 + index * 1_800))
            .chain([(VIDEO_PID, 0xe0, 90_000 + 999 * 1_800 + 10 * 90_000)])
            .collect();
        let expected: Vec<(usize, usize, bool)> = core::iter::once((1, 1, true))
            .chain(core::iter::repeat_n((0, 0, true), inputs.len() - 1))
            .collect();
        assert_eq!(psi_of_each(config, &inputs), expected);
    }

    /// Whole descriptors of tag 0x80 that come to `length` bytes, which is not 1
    fn descriptors(length: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut rest = length;
        while rest > 0 {
            let body = (rest - 2).min(255);
            bytes.extend([0x80, u8::try_from(body).unwrap()]);
            bytes.extend(core::iter::repeat_n(0xaa, body));
            rest -= 2 + body;
        }
        bytes
    }

    #[test]
    fn program_info_is_taken_that_fills_the_pmt_and_refused_one_byte_past_it() {
        // A PMT without streams: 13 bytes of `section_length` besides its
        // descriptors leave 1008 of the 1021
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        muxer.add_program(1, pid(PMT_PID)).unwrap();
        assert_eq!(
            [
                muxer.set_program_info(1, DescriptorLoop::from(descriptors(1_009).as_slice())),
                muxer.set_program_info(1, DescriptorLoop::from(descriptors(1_008).as_slice())),
            ],
            [
                Err(Error::TsProgramMapSection {
                    error: pmt::Error::Section(section::Error::SectionLength {
                        section_length: 1_022,
                        limit: 1_021
                    })
                }),
                Ok(()),
            ]
        );
    }

    #[test]
    fn a_stream_is_taken_whose_descriptors_fill_the_pmt_and_refused_one_byte_past_it() {
        // A PMT of video and audio: 13 bytes besides its loops, two entries of
        // 5, and 5 for the stream added, leave 993 bytes of descriptors
        let mut muxer = muxer_of(MuxerConfig::new());
        let mut add = |es_info: &[u8]| {
            muxer.add_stream(
                1,
                ElementaryStream::new(StreamType::AVC_VIDEO, pid(0x0102))
                    .with_es_info(DescriptorLoop::from(es_info)),
            )
        };
        assert_eq!(
            [add(&descriptors(994)), add(&descriptors(993))],
            [
                Err(Error::TsProgramMapSection {
                    error: pmt::Error::Section(section::Error::SectionLength {
                        section_length: 1_022,
                        limit: 1_021
                    })
                }),
                Ok(()),
            ]
        );
    }

    const PRIVATE_PID: u16 = 0x0102;

    /// A muxer of the default settings with program 1, a video stream and a
    /// stream of `private_stream_2`
    fn muxer_with_private_stream_2() -> Muxer {
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        muxer.add_program(1, pid(PMT_PID)).unwrap();
        muxer
            .add_stream(
                1,
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(VIDEO_PID)),
            )
            .unwrap();
        muxer
            .add_stream(
                1,
                ElementaryStream::new(
                    StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                    pid(PRIVATE_PID),
                ),
            )
            .unwrap();
        muxer
    }

    #[test]
    fn a_stream_without_an_optional_header_is_taken_without_a_pts_after_the_first_pes_packet() {
        let mut muxer = muxer_with_private_stream_2();
        let private = PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0xcc; 10]);
        let results = [
            muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 90_000)),
            muxer.handle_pes_packet(pid(PRIVATE_PID), &private),
            muxer.handle_pes_packet(pid(PRIVATE_PID), &private),
        ];
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        let private_packets = out
            .chunks(188)
            .filter(|chunk| TransportPacket::read(chunk).unwrap().pid == pid(PRIVATE_PID))
            .count();
        assert_eq!((results, private_packets), ([Ok(()), Ok(()), Ok(())], 2));
    }

    #[test]
    fn a_first_pes_packet_without_a_time_stamp_is_refused_on_a_stream_without_an_optional_header() {
        let mut muxer = muxer_with_private_stream_2();
        assert_eq!(
            muxer.handle_pes_packet(
                pid(PRIVATE_PID),
                &PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0xcc])
            ),
            Err(Error::MissingPts {
                pid: pid(PRIVATE_PID)
            })
        );
    }

    mod transport_packet_fields {
        use alloc::vec;
        use alloc::vec::Vec;

        use super::*;
        use crate::ts::demux::{Demuxer, DemuxerConfig, Event};

        /// What a Transport Packet carries of what a [`TransportPacketFields`] sets, and
        /// of what the muxer sets beside it
        #[derive(Debug, Clone, PartialEq, Eq)]
        struct Carried {
            pid: u16,
            payload_unit_start_indicator: bool,
            transport_priority: bool,
            random_access_indicator: bool,
            elementary_stream_priority_indicator: bool,
            program_clock_reference: Option<ClockReference>,
            transport_private_data: Option<Vec<u8>>,
        }

        fn carried(bytes: &[u8]) -> Vec<Carried> {
            bytes
                .chunks(188)
                .map(|chunk| {
                    let packet = TransportPacket::read(chunk).unwrap();
                    let field = packet.adaptation_field.as_ref();
                    Carried {
                        pid: packet.pid.get(),
                        payload_unit_start_indicator: packet.payload_unit_start_indicator,
                        transport_priority: packet.transport_priority,
                        random_access_indicator: field
                            .is_some_and(|field| field.random_access_indicator),
                        elementary_stream_priority_indicator: field
                            .is_some_and(|field| field.elementary_stream_priority_indicator),
                        program_clock_reference: field
                            .and_then(|field| field.program_clock_reference),
                        transport_private_data: field
                            .and_then(|field| field.transport_private_data)
                            .map(<[u8]>::to_vec),
                    }
                })
                .collect()
        }

        /// A Transport Packet of `pid` that carries none of the fields set
        fn plain(pid: u16, payload_unit_start_indicator: bool) -> Carried {
            Carried {
                pid,
                payload_unit_start_indicator,
                transport_priority: false,
                random_access_indicator: false,
                elementary_stream_priority_indicator: false,
                program_clock_reference: None,
                transport_private_data: None,
            }
        }

        /// What the muxer of the default settings makes of the PES packets given
        /// in turn, each `(pid, dts, fields)`: the result of each, and the output
        fn multiplexed(
            inputs: &[(u16, u64, TransportPacketFields<'_>)],
        ) -> (Vec<Result<(), Error>>, Vec<u8>) {
            let mut muxer = muxer_of(MuxerConfig::new());
            let payload = vec![0xaa; 400];
            let results = inputs
                .iter()
                .map(|(pid_value, dts, fields)| {
                    let stream_id = if *pid_value == VIDEO_PID { 0xe0 } else { 0xc0 };
                    muxer.handle_pes_packet_with_transport_packet_fields(
                        pid(*pid_value),
                        &pes_packet(stream_id, &payload, Some(*dts)),
                        fields,
                    )
                })
                .collect();
            let mut out = Vec::new();
            muxer.poll_output(&mut out);
            (results, out)
        }

        /// The output of [`multiplexed`], each PES packet taken
        fn output_of(inputs: &[(u16, u64, TransportPacketFields<'_>)]) -> Vec<u8> {
            let (results, out) = multiplexed(inputs);
            assert!(results.iter().all(Result::is_ok), "{results:?}");
            out
        }

        /// The Transport Packets of `pid` in `out`
        fn of_pid(out: &[u8], pid_value: u16) -> Vec<Carried> {
            carried(out)
                .into_iter()
                .filter(|packet| packet.pid == pid_value)
                .collect()
        }

        /// The `sync_byte` to `adaptation_field_control` of a Transport Packet of
        /// `pid`, its `continuity_counter` 0 (§2.4.3.2)
        fn header(
            pid_value: u16,
            payload_unit_start_indicator: bool,
            transport_priority: bool,
            adaptation_field_control: u8,
        ) -> Vec<u8> {
            let [high, low] = pid_value.to_be_bytes();
            vec![
                0x47,
                u8::from(payload_unit_start_indicator) << 6
                    | u8::from(transport_priority) << 5
                    | high,
                low,
                adaptation_field_control << 4,
            ]
        }

        /// `program_clock_reference_base` and `_extension` of a PCR of `base`, its
        /// extension 0: 33 bits of base, 6 reserved bits of `'1'`, 9 bits of
        /// extension (§2.4.3.4)
        fn pcr_field(base: u64) -> Vec<u8> {
            let field = base << 15 | 0x3f << 9;
            field.to_be_bytes().get(2..).unwrap().to_vec()
        }

        /// The header and the adaptation field of the Transport Packet that starts
        /// the last PES packet on `pid` in `out`, its `continuity_counter` cleared
        fn first_packet_head(out: &[u8], pid_value: u16) -> Vec<u8> {
            let chunk = out
                .chunks(188)
                .rfind(|chunk| {
                    let packet = TransportPacket::read(chunk).unwrap();
                    packet.pid.get() == pid_value && packet.payload_unit_start_indicator
                })
                .unwrap();
            let adaptation_field_control = chunk.get(3).unwrap() >> 4;
            let adaptation_field_length = if adaptation_field_control & 0b10 == 0 {
                0
            } else {
                1 + usize::from(*chunk.get(4).unwrap())
            };
            let mut head = chunk.get(..4 + adaptation_field_length).unwrap().to_vec();
            if let Some(byte) = head.get_mut(3) {
                *byte &= 0xf0;
            }
            head
        }

        #[test]
        fn the_fields_set_go_in_the_adaptation_field_of_the_first_transport_packet_only() {
            let private_data = [0x01, 0x02, 0x03];
            let fields = TransportPacketFields::new()
                .with_random_access_indicator(true)
                .with_elementary_stream_priority_indicator(true)
                .with_transport_private_data(&private_data);
            let audio = of_pid(&output_of(&[(AUDIO_PID, 90_000, fields)]), AUDIO_PID);
            assert_eq!(
                audio,
                [
                    Carried {
                        random_access_indicator: true,
                        elementary_stream_priority_indicator: true,
                        transport_private_data: Some(private_data.to_vec()),
                        ..plain(AUDIO_PID, true)
                    },
                    plain(AUDIO_PID, false),
                    plain(AUDIO_PID, false),
                ]
            );
        }
        #[test]
        fn transport_priority_is_set_on_every_transport_packet_of_the_pes_packet_only() {
            let fields = TransportPacketFields::new().with_transport_priority(true);
            let out = output_of(&[
                (VIDEO_PID, 90_000, TransportPacketFields::new()),
                (AUDIO_PID, 93_600, fields),
            ]);
            let others_with_priority: Vec<Carried> = carried(&out)
                .into_iter()
                .filter(|packet| packet.pid != AUDIO_PID && packet.transport_priority)
                .collect();
            let with_priority = |payload_unit_start_indicator| Carried {
                transport_priority: true,
                ..plain(AUDIO_PID, payload_unit_start_indicator)
            };
            assert_eq!(
                (of_pid(&out, AUDIO_PID), others_with_priority),
                (
                    vec![
                        with_priority(true),
                        with_priority(false),
                        with_priority(false)
                    ],
                    Vec::new()
                )
            );
        }

        #[test]
        fn transport_priority_is_not_set_on_a_pcr_split_from_the_pes_packet_on_its_pid() {
            let private_data: Vec<u8> = (0..180).collect();
            let fields = TransportPacketFields::new()
                .with_transport_priority(true)
                .with_transport_private_data(&private_data);
            let out = output_of(&[
                (VIDEO_PID, 90_000, TransportPacketFields::new()),
                (VIDEO_PID, 93_600, fields),
            ]);
            let video = of_pid(&out, VIDEO_PID);
            let with_priority = |payload_unit_start_indicator| Carried {
                transport_priority: true,
                ..plain(VIDEO_PID, payload_unit_start_indicator)
            };
            // adaptation_field_length 182: the byte of flags with
            // transport_private_data_flag, transport_private_data_length 180, the
            // data (§2.4.3.4)
            let mut head = header(VIDEO_PID, true, true, 0b11);
            head.extend([182, 0x02, 180]);
            head.extend(&private_data);
            assert_eq!(
                (
                    video.get(3..).map(<[Carried]>::to_vec),
                    first_packet_head(&out, VIDEO_PID)
                ),
                (
                    Some(vec![
                        Carried {
                            program_clock_reference: Some(pcr((93_600 - DELAY) * 300)),
                            ..plain(VIDEO_PID, false)
                        },
                        Carried {
                            transport_private_data: Some(private_data.clone()),
                            ..with_priority(true)
                        },
                        with_priority(false),
                        with_priority(false),
                        with_priority(false),
                    ]),
                    head
                )
            );
        }
        #[test]
        fn the_fields_set_go_beside_a_pcr_in_the_same_packet() {
            let private_data = [0x0a; 8];
            let fields = TransportPacketFields::new()
                .with_random_access_indicator(true)
                .with_transport_private_data(&private_data);
            let video = of_pid(&output_of(&[(VIDEO_PID, 90_000, fields)]), VIDEO_PID);
            assert_eq!(
                video.first().cloned(),
                Some(Carried {
                    random_access_indicator: true,
                    program_clock_reference: Some(pcr((90_000 - DELAY) * 300)),
                    transport_private_data: Some(private_data.to_vec()),
                    ..plain(VIDEO_PID, true)
                })
            );
        }

        fn pcrs_of(out: &[u8]) -> Vec<(u16, ClockReference)> {
            carried(out)
                .iter()
                .filter_map(|packet| {
                    packet
                        .program_clock_reference
                        .map(|value| (packet.pid, value))
                })
                .collect()
        }

        #[test]
        fn random_access_on_the_pcr_pid_puts_a_pcr_in_the_packet_and_starts_the_pcr_interval_there()
        {
            let random_access = TransportPacketFields::new().with_random_access_indicator(true);
            let out = output_of(&[
                (VIDEO_PID, 90_000, TransportPacketFields::new()),
                (VIDEO_PID, 90_900, random_access),
                (VIDEO_PID, 93_600, TransportPacketFields::new()),
                (VIDEO_PID, 94_500, TransportPacketFields::new()),
            ]);
            let at = |dts: u64| (VIDEO_PID, pcr((dts - DELAY) * 300));
            assert_eq!(pcrs_of(&out), [at(90_000), at(90_900), at(94_500)]);
        }

        #[test]
        fn random_access_off_the_pcr_pid_adds_no_pcr() {
            let random_access = TransportPacketFields::new().with_random_access_indicator(true);
            let out = output_of(&[
                (VIDEO_PID, 90_000, TransportPacketFields::new()),
                (AUDIO_PID, 90_900, random_access),
            ]);
            assert_eq!(pcrs_of(&out), [(VIDEO_PID, pcr((90_000 - DELAY) * 300))]);
        }

        #[test]
        fn fields_that_set_nothing_add_no_adaptation_field_where_no_pcr_is_due() {
            let out = output_of(&[
                (VIDEO_PID, 90_000, TransportPacketFields::new()),
                (AUDIO_PID, 90_900, TransportPacketFields::new()),
            ]);
            // adaptation_field_control '01': a payload alone (§2.4.3.2)
            assert_eq!(
                first_packet_head(&out, AUDIO_PID),
                header(AUDIO_PID, true, false, 0b01)
            );
        }
        /// The result of giving a PES packet on `pid` at `dts`, after a first one on
        /// the video PID at 90 000, carrying `private_data`, random access as given,
        /// and the output then
        fn with_private_data(
            pid_value: u16,
            dts: u64,
            random_access_indicator: bool,
            private_data: &[u8],
        ) -> (Result<(), Error>, Vec<u8>) {
            let (results, out) = multiplexed(&[
                (VIDEO_PID, 90_000, TransportPacketFields::new()),
                (
                    pid_value,
                    dts,
                    TransportPacketFields::new()
                        .with_random_access_indicator(random_access_indicator)
                        .with_transport_private_data(private_data),
                ),
            ]);
            (*results.last().unwrap(), out)
        }

        #[test]
        fn transport_private_data_past_the_adaptation_field_is_refused_and_leaves_the_muxer_as_it_was()
         {
            let next = (AUDIO_PID, 93_600, TransportPacketFields::new());
            let (results, after_refusal) = multiplexed(&[
                (VIDEO_PID, 90_000, TransportPacketFields::new()),
                (
                    AUDIO_PID,
                    90_900,
                    TransportPacketFields::new().with_transport_private_data(&[0x0b; 181]),
                ),
                next,
            ]);
            let (_, without_refusal) =
                multiplexed(&[(VIDEO_PID, 90_000, TransportPacketFields::new()), next]);
            let (taken, _) = with_private_data(AUDIO_PID, 90_900, false, &[0x0b; 180]);
            let (taken_off_the_pcr_pid, _) =
                with_private_data(AUDIO_PID, 90_900, true, &[0x0b; 180]);
            assert_eq!(
                (results, after_refusal, taken, taken_off_the_pcr_pid),
                (
                    vec![
                        Ok(()),
                        Err(Error::TransportPrivateDataLength {
                            length: 181,
                            limit: 180
                        }),
                        Ok(())
                    ],
                    without_refusal,
                    Ok(()),
                    Ok(())
                )
            );
        }
        #[test]
        fn a_pcr_due_with_transport_private_data_too_long_to_share_its_packet_goes_alone_before_it()
        {
            let private_data = [0x0c; 180];
            let (result, out) = with_private_data(VIDEO_PID, 93_600, false, &private_data);
            let mut head = header(VIDEO_PID, true, false, 0b11);
            head.extend([182, 0x02, 180]);
            head.extend(private_data);
            assert_eq!(
                (
                    result,
                    of_pid(&out, VIDEO_PID).get(3..5).map(<[Carried]>::to_vec),
                    first_packet_head(&out, VIDEO_PID)
                ),
                (
                    Ok(()),
                    Some(vec![
                        Carried {
                            program_clock_reference: Some(pcr((93_600 - DELAY) * 300)),
                            ..plain(VIDEO_PID, false)
                        },
                        Carried {
                            transport_private_data: Some(private_data.to_vec()),
                            ..plain(VIDEO_PID, true)
                        },
                    ]),
                    head
                )
            );
        }
        #[test]
        fn random_access_on_the_pcr_pid_takes_as_much_transport_private_data_as_fits_beside_the_pcr()
         {
            let (taken, out) = with_private_data(VIDEO_PID, 90_900, true, &[0x0d; 174]);
            let (refused, _) = with_private_data(VIDEO_PID, 90_900, true, &[0x0d; 175]);
            // adaptation_field_length 182: random_access_indicator, PCR_flag and
            // transport_private_data_flag, the PCR, transport_private_data_length
            // 174, the data (§2.4.3.4)
            let mut head = header(VIDEO_PID, true, false, 0b11);
            head.extend([182, 0x52]);
            head.extend(pcr_field(90_900 - DELAY));
            head.push(174);
            head.extend([0x0d; 174]);
            assert_eq!(
                (taken, refused, first_packet_head(&out, VIDEO_PID)),
                (
                    Ok(()),
                    Err(Error::TransportPrivateDataLength {
                        length: 175,
                        limit: 174
                    }),
                    head
                )
            );
        }
        #[test]
        fn a_pes_packet_after_an_adaptation_field_that_leaves_one_byte_of_payload_reads_back_whole()
        {
            let payload: Vec<u8> = (0..=255).collect();
            let mut muxer = muxer_of(MuxerConfig::new());
            let packet = pes_packet(0xc0, &payload, Some(90_000));
            muxer
                .handle_pes_packet_with_transport_packet_fields(
                    pid(AUDIO_PID),
                    &packet,
                    &TransportPacketFields::new().with_transport_private_data(&[0x0e; 180]),
                )
                .unwrap();
            muxer.finish();
            let mut out = Vec::new();
            muxer.poll_output(&mut out);
            let mut demuxer = Demuxer::new(DemuxerConfig::default());
            demuxer.subscribe_pes(pid(AUDIO_PID));
            demuxer.handle_input(&out);
            demuxer.finish();
            let mut payloads = Vec::new();
            while let Some(event) = demuxer.poll_event() {
                if let Event::PesPacket { packet, .. } = event {
                    payloads.push(packet.payload.to_vec());
                }
            }
            let mut head = header(AUDIO_PID, true, false, 0b11);
            head.extend([182, 0x02, 180]);
            head.extend([0x0e; 180]);
            assert_eq!(
                (first_packet_head(&out, AUDIO_PID), payloads),
                (head, vec![payload])
            );
        }

        #[test]
        fn a_pcr_split_from_its_pes_packet_counts_with_the_pes_packet_against_the_buffer_limit() {
            let config = MuxerConfig::new()
                .with_psi_interval(Duration::ZERO)
                .with_pcr_interval(Duration::from_millis(10))
                .with_buffer_limit(600);
            let mut muxer = muxer_of(config);
            let payload = [0xaa; 10];
            muxer
                .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &payload, Some(90_000)))
                .unwrap();
            muxer.poll_output(&mut Vec::new());
            // Why 91 800: two PCRs are then due, the second at the PES packet, where
            // 180 bytes of private data leave it no room
            let refused = muxer.handle_pes_packet_with_transport_packet_fields(
                pid(VIDEO_PID),
                &pes_packet(0xe0, &payload, Some(91_800)),
                &TransportPacketFields::new().with_transport_private_data(&[0x0f; 180]),
            );
            assert_eq!(refused, Err(Error::BufferFull { length: 188 * 6 }));
        }

        /// The PCRs of an output, each the PID it goes on and its value
        type Pcrs = Vec<(u16, ClockReference)>;

        /// What the muxer of the default settings makes of `inputs` and the end of
        /// input, each `(pid, stream_id, dts, fields)`: the result of each, and the
        /// PCRs of the output
        fn finished(
            inputs: &[(u16, u8, Option<u64>, TransportPacketFields<'_>)],
        ) -> (Vec<Result<(), Error>>, Pcrs) {
            let mut muxer = muxer_of(MuxerConfig::new());
            let payload = [0xaa; 100];
            let results = inputs
                .iter()
                .map(|(pid_value, stream_id, dts, fields)| {
                    muxer.handle_pes_packet_with_transport_packet_fields(
                        pid(*pid_value),
                        &pes_packet(*stream_id, &payload, *dts),
                        fields,
                    )
                })
                .collect();
            muxer.finish();
            let mut out = Vec::new();
            muxer.poll_output(&mut out);
            (results, pcrs_of(&out))
        }

        /// The PCR of the time `dts` on the PCR PID, the default `mux_delay` taken off
        fn video_pcr(dts: u64) -> (u16, ClockReference) {
            (VIDEO_PID, pcr((dts - DELAY) * 300))
        }

        #[test]
        fn random_access_on_the_pcr_pid_at_the_time_of_the_pcr_before_is_refused() {
            let random_access = TransportPacketFields::new().with_random_access_indicator(true);
            let audio = (AUDIO_PID, 0xc0, Some(90_000), TransportPacketFields::new());
            let (results, pcrs) =
                finished(&[audio, (VIDEO_PID, 0xe0, Some(90_000), random_access)]);
            let (_, without_the_refused) = finished(&[audio]);
            assert_eq!(
                (results, pcrs.clone(), without_the_refused),
                (
                    vec![
                        Ok(()),
                        Err(Error::RandomAccessIndicator {
                            pid: pid(VIDEO_PID),
                            program_clock_reference: pcr((90_000 - DELAY) * 300),
                        })
                    ],
                    vec![video_pcr(90_000), video_pcr(93_600)],
                    pcrs
                )
            );
        }

        #[test]
        fn random_access_on_the_pcr_pid_without_a_time_stamp_after_the_pcr_is_refused() {
            let random_access = TransportPacketFields::new().with_random_access_indicator(true);
            let video = (VIDEO_PID, 0xe0, Some(90_000), TransportPacketFields::new());
            let (results, pcrs) = finished(&[video, (VIDEO_PID, 0xe0, None, random_access)]);
            assert_eq!(
                (results, pcrs),
                (
                    vec![
                        Ok(()),
                        Err(Error::RandomAccessIndicator {
                            pid: pid(VIDEO_PID),
                            program_clock_reference: pcr((90_000 - DELAY) * 300),
                        })
                    ],
                    vec![video_pcr(90_000), video_pcr(93_600)]
                )
            );
        }

        #[test]
        fn random_access_on_the_pcr_pid_given_first_among_pes_packets_of_one_time_is_taken() {
            let random_access = TransportPacketFields::new().with_random_access_indicator(true);
            let (results, pcrs) = finished(&[
                (VIDEO_PID, 0xe0, Some(90_000), random_access),
                (AUDIO_PID, 0xc0, Some(90_000), TransportPacketFields::new()),
            ]);
            assert_eq!(
                (results, pcrs),
                (
                    vec![Ok(()), Ok(())],
                    vec![video_pcr(90_000), video_pcr(93_600)]
                )
            );
        }

        #[test]
        fn random_access_on_the_pcr_pid_without_a_time_stamp_after_the_time_advanced_is_taken() {
            let random_access = TransportPacketFields::new().with_random_access_indicator(true);
            let (results, pcrs) = finished(&[
                (VIDEO_PID, 0xe0, Some(90_000), TransportPacketFields::new()),
                (AUDIO_PID, 0xc0, Some(90_900), TransportPacketFields::new()),
                (VIDEO_PID, 0xe0, None, random_access),
            ]);
            assert_eq!(
                (results, pcrs),
                (
                    vec![Ok(()), Ok(()), Ok(())],
                    vec![video_pcr(90_000), video_pcr(90_900)]
                )
            );
        }
    }
}
