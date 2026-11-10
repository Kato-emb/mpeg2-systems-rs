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
use core::mem;
use core::num::NonZeroU16;
use core::time::Duration;

use crate::descriptor::DescriptorLoop;
use crate::pes::{PesPacket, StreamId};
use crate::psi::pmt;
use crate::time::{ClockReference, SystemTimeClock};
use crate::ts::Pid;
use crate::ts::adaptation_field::AdaptationField;
use crate::ts::packet::PACKET_LENGTH;
use packet_writer::{Stuffing, packet_count, section_packets_length};
use program_definition::{ProgramDefinition, Stream};
use schedule::{Schedule, Times};

/// The longest time from one PCR to the next, ISO/IEC 13818-1 §2.7.2
const LONGEST_PCR_INTERVAL: Duration = Duration::from_millis(100);

mod carriage;
mod config;
mod error;
mod packet_writer;
mod program_definition;
mod schedule;

pub use carriage::PesPacketCarriage;
pub use config::MuxerConfig;
pub use error::Error;
pub use packet_writer::PacketWriter;

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
        let pcr_interval = config.pcr_interval;
        if SystemTimeClock::ticks_in(pcr_interval) == 0 || pcr_interval > LONGEST_PCR_INTERVAL {
            return Err(Error::PcrInterval { pcr_interval });
        }
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
        let Some(program_number) = NonZeroU16::new(program_number) else {
            return Err(Error::ProgramNumber { program_number });
        };
        if !program_map_pid.is_assignable() {
            return Err(Error::Pid {
                pid: program_map_pid,
            });
        }
        self.program = Some(ProgramDefinition {
            program_number,
            program_map_pid,
            pcr_pid: None,
            program_info: Vec::new(),
            streams: Vec::new(),
        });
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
        let program = self.program_mut(program_number)?;
        if !pcr_pid.is_assignable() {
            return Err(Error::Pid { pid: pcr_pid });
        }
        program.pcr_pid = Some(pcr_pid);
        Ok(())
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
        let program = self.program_mut(program_number)?;
        program_definition::check_pmt_length(
            program_info.as_bytes(),
            program.elementary_streams(),
        )?;
        let previous = mem::replace(&mut program.program_info, program_info.as_bytes().to_vec());
        program.check(transport_stream_id).inspect_err(|_| {
            program.program_info = previous;
        })
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
        let program = self.program_mut(program_number)?;
        let pid = stream.elementary_pid;
        if !pid.is_assignable() {
            return Err(Error::Pid { pid });
        }
        if pid == program.program_map_pid || program.stream(pid).is_some() {
            return Err(Error::PidInUse { pid });
        }
        program_definition::check_pmt_length(
            &program.program_info,
            program.elementary_streams().chain(core::iter::once(stream)),
        )?;
        program.streams.push(Stream {
            pid,
            stream_type: stream.stream_type,
            es_info: stream.es_info.as_bytes().to_vec(),
            last_timestamp: None,
        });
        program.check(transport_stream_id).inspect_err(|_| {
            program.streams.pop();
        })
    }

    /// Returns the program of `program_number`, or
    /// [`UnregisteredProgram`](Error::UnregisteredProgram)
    fn program_mut(&mut self, program_number: u16) -> Result<&mut ProgramDefinition, Error> {
        self.program
            .as_mut()
            .filter(|program| program.program_number.get() == program_number)
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
        self.handle_pes_packet_with_carriage(pid, packet, &PesPacketCarriage::new())
    }

    /// Takes `packet`, a PES packet of the stream on `pid`, as
    /// [`handle_pes_packet`](Self::handle_pes_packet) does, carried as
    /// `carriage` says
    ///
    /// With `random_access_indicator` set on the PCR PID, the muxer sends a
    /// PCR in the first Transport Packet, since ISO/IEC 13818-1 §2.4.3.5 sets
    /// the indicator on the PCR PID only in a packet that carries one; the
    /// next PCR is [`MuxerConfig::pcr_interval`] after it. A PCR due at the
    /// first Transport Packet that does not fit beside
    /// `transport_private_data` goes, at the same value, in a Transport
    /// Packet of its own on the PCR PID before it. With
    /// [`PesPacketCarriage::new`], the output is that of `handle_pes_packet`.
    ///
    /// ```
    /// use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
    /// use mpeg2_systems::psi::pmt::ElementaryStream;
    /// use mpeg2_systems::stream_type::StreamType;
    /// use mpeg2_systems::time::Timestamp;
    /// use mpeg2_systems::ts::Pid;
    /// use mpeg2_systems::ts::mux::{Muxer, MuxerConfig, PesPacketCarriage};
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
    /// let carriage = PesPacketCarriage::new().with_random_access_indicator(true);
    /// muxer.handle_pes_packet_with_carriage(video_pid, &packet, &carriage)?;
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
    ///
    /// # Panics
    ///
    /// Panics if [`finish`](Self::finish) has been called.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn handle_pes_packet_with_carriage(
        &mut self,
        pid: Pid,
        packet: &PesPacket<'_>,
        carriage: &PesPacketCarriage<'_>,
    ) -> Result<(), Error> {
        assert!(!self.finished, "a PES packet is given after finish");
        let Some((program, stream_index)) = self.program.as_ref().and_then(|program| {
            let index = program
                .streams
                .iter()
                .position(|stream| stream.pid == pid)?;
            Some((program, index))
        }) else {
            return Err(Error::UnregisteredPid { pid });
        };
        let last_timestamp = program
            .streams
            .get(stream_index)
            .and_then(|stream| stream.last_timestamp);
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
        // Why not ask a PTS of the first PES packet of every stream: the PTS
        // of the first access unit §2.7.5 asks for sits in the optional
        // header, which the PES packets of some streams do not carry.
        if last_timestamp.is_none()
            && stream_id.carries_optional_header()
            && header.and_then(|header| header.pts).is_none()
        {
            return Err(Error::MissingPts { pid });
        }
        let timestamp = header.and_then(|header| header.dts.or(header.pts));
        let reached = program.latest_timestamp();
        let (clock, time, psi_times, pcr_times, mut next) = match (schedule.zip(reached), timestamp)
        {
            (None, Some(timestamp)) => {
                let clock = SystemTimeClock::first(timestamp);
                let next = Schedule {
                    psi_sent_at: clock,
                    pcr_sent_at: clock,
                    several_pcrs: false,
                };
                (
                    clock,
                    Some(clock),
                    Times::once(clock),
                    Times::once(clock),
                    next,
                )
            }
            // Why not a time of its own: the PCR and the PAT and the PMT
            // start at the time of the first PES packet, and this one has
            // none.
            (None, None) => return Err(Error::MissingPts { pid }),
            (Some((schedule, reached)), timestamp) => {
                let time = timestamp.map(|timestamp| reached.expand(timestamp));
                if let (Some(time), Some(timestamp), Some(previous)) =
                    (time, timestamp, last_timestamp)
                    && time < previous
                {
                    return Err(Error::DecreasingTimestamp {
                        pid,
                        timestamp,
                        previous: previous.to_timestamp(),
                    });
                }
                let clock = time.map_or(reached, |time| reached.max(time));
                let interval = |duration| SystemTimeClock::ticks_in(duration);
                let psi_times =
                    Times::due(schedule.psi_sent_at, clock, interval(config.psi_interval));
                let pcr_times =
                    Times::due(schedule.pcr_sent_at, clock, interval(config.pcr_interval));
                let next = Schedule {
                    psi_sent_at: psi_times.last().unwrap_or(schedule.psi_sent_at),
                    pcr_sent_at: pcr_times.last().unwrap_or(schedule.pcr_sent_at),
                    several_pcrs: schedule.several_pcrs || pcr_times.count > 0,
                };
                (clock, time, psi_times, pcr_times, next)
            }
        };
        // Why not refuse a program without a PCR PID: a stream is registered
        // on `pid`, so the program has one.
        let pcr_pid = program.pcr_pid().unwrap_or(pid);
        let on_pcr_pid = pcr_pid == pid;
        let pcr_due = on_pcr_pid && pcr_times.last() == Some(clock);
        let pcr_in_packet = carriage.pcr_in_first_packet(on_pcr_pid, pcr_due)?;
        let adaptation_field =
            carriage.adaptation_field(pcr_in_packet.then(|| pcr_at(config, clock)));
        let pcrs_alone = pcr_times
            .count
            .saturating_sub(u64::from(pcr_due && pcr_in_packet));
        // Why not count the PCR due at the PES packet, split from it, with the
        // PCRs that fill a leap: it is sent at the PES packet's time, and is
        // part of what the PES packet adds to the output.
        let pcr_split = pcr_due && !pcr_in_packet;
        if pcr_in_packet && !pcr_due {
            next = next.with_pcr_sent_at(clock);
        }

        section_bodies.clear();
        let sections = if psi_times.count > 0 {
            let pat_body_length = program.write_bodies(section_bodies)?;
            let (pat_body, pmt_body) = section_bodies.split_at(pat_body_length);
            Some(program.sections_of(config.transport_stream_id, pat_body, pmt_body))
        } else {
            None
        };
        let psi_length = sections.as_ref().map_or(0, |(pat, pmt)| {
            section_packets_length(pat.encoded_len())
                .saturating_add(section_packets_length(pmt.encoded_len()))
        });
        let as_length = |count: u64| usize::try_from(count).unwrap_or(usize::MAX);
        let filling = psi_length
            .saturating_mul(as_length(psi_times.count))
            .saturating_add(
                PACKET_LENGTH
                    .saturating_mul(as_length(pcrs_alone.saturating_sub(u64::from(pcr_split)))),
            );
        if filling > config.buffer_limit {
            return Err(Error::TimestampGap {
                gap: clock.timestamp_ticks_since(reached.unwrap_or(clock)),
            });
        }
        let length = filling
            .saturating_add(if pcr_split { PACKET_LENGTH } else { 0 })
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

        // Why not all the PSI and then all the PCRs: each goes at its time,
        // the PSI first where the two fall together, so that a PCR stays
        // the time its bytes arrive at.
        let mut psi_at = psi_times.iter().zip(sections.iter().cycle()).peekable();
        let mut pcrs_at = pcr_times.iter().take(as_length(pcrs_alone)).peekable();
        loop {
            let psi_first = match (psi_at.peek(), pcrs_at.peek()) {
                (Some((psi, _)), Some(pcr)) => psi <= pcr,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if psi_first {
                if let Some((_, (pat, pmt))) = psi_at.next() {
                    writer.write_section(Pid::PAT, pat, pending)?;
                    writer.write_section(program.program_map_pid, pmt, pending)?;
                }
            } else if let Some(at) = pcrs_at.next() {
                writer.write_pcr_packet(pcr_pid, pcr_at(config, at), pending)?;
            }
        }
        writer.write_payload_unit(
            pid,
            |out| {
                out.extend_from_slice(scratch);
                Ok(())
            },
            adaptation_field,
            carriage.transport_priority,
            Stuffing::AdaptationField,
            pending,
        )?;
        *schedule = Some(next);
        if let (Some(time), Some(stream)) = (
            time,
            self.program
                .as_mut()
                .and_then(|program| program.streams.get_mut(stream_index)),
        ) {
            stream.last_timestamp = Some(time);
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
            && !schedule.several_pcrs
        {
            let at = schedule
                .pcr_sent_at
                .add_ticks(SystemTimeClock::ticks_in(self.config.pcr_interval))
                .max(reached);
            if self
                .writer
                .write_pcr_packet(pcr_pid, pcr_at(&self.config, at), &mut self.pending)
                .is_ok()
            {
                schedule.several_pcrs = true;
            }
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

/// Returns the PCR of `at`, less [`MuxerConfig::mux_delay`]
fn pcr_at(config: &MuxerConfig, at: SystemTimeClock) -> ClockReference {
    at.to_clock_reference(config.mux_delay)
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "time stamps and lengths are small constants well inside their range"
)]
mod tests;
