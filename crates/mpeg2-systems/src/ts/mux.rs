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
//! The crate does not write null packets, keep a constant bit rate, check
//! the T-STD, or put more than one section in a Transport Packet. The
//! example writes through [`PacketWriter`]; [`Muxer`] has its own.
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
//! let mut pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0)?);
//! pat.programs = ProgramLoop::from(&programs[..]);
//! writer.write_program_association_section(&pat, &mut stream)?;
//! // The PMT, its PCR on the video PID
//! let mut streams = Vec::new();
//! ElementaryStreamLoop::write_from(
//!     [ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video_pid)],
//!     &mut streams,
//! )?;
//! let mut pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0)?);
//! pmt.pcr_pid = Some(video_pid);
//! pmt.elementary_streams = ElementaryStreamLoop::from(&streams[..]);
//! writer.write_ts_program_map_section(pmt_pid, &pmt, &mut stream)?;
//! // An access unit, with the PCR of the program
//! let access_unit = vec![0xaa; 1_000];
//! let mut header = OptionalHeader::new();
//! header.pts = Some(Timestamp::try_from(90_000)?);
//! let mut packet = PesPacket::new(StreamId::from(0xe0), &access_unit);
//! packet.optional_header = Some(header);
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
use core::fmt;
use core::num::NonZeroU16;
use core::time::Duration;

use crate::pes::{self, PesPacket, StreamId};
use crate::psi::cat;
use crate::psi::pat;
use crate::psi::pmt;
use crate::psi::section;
use crate::stream_type::StreamType;
use crate::time::{ClockReference, unwrap_timestamp};
use crate::ts::Pid;
use crate::ts::adaptation_field::AdaptationField;
use crate::ts::packet::{self, PACKET_LENGTH};
use packet_writer::{Stuffing, packet_count, section_packets_length};
use program_definition::{ProgramDefinition, Stream};

mod packet_writer;
mod program_definition;

pub use packet_writer::PacketWriter;

/// Ticks of the 27 MHz system clock in a second
const SYSTEM_CLOCK_FREQUENCY: u128 = 27_000_000;

/// Ticks of the 27 MHz system clock in one tick of the 90 kHz clock of PTS
/// and DTS
const TICKS_PER_TIMESTAMP_TICK: u64 = 300;

/// Failure to write a PES packet or a section as Transport Packets, or to
/// register or multiplex a stream with a [`Muxer`]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The PID given cannot carry what is written: a PES packet only goes on
    /// a PID from 0x0010 to 0x1FFE, and nothing on the PID of null packets
    Pid {
        /// The PID given
        pid: Pid,
    },
    /// The `stream_id` given is not that of a PES packet
    StreamId {
        /// The `stream_id` given
        stream_id: StreamId,
    },
    /// The PES packet could not be written
    PesPacket {
        /// How the PES packet failed
        error: pes::Error,
    },
    /// The section could not be written
    Section {
        /// How the section failed
        error: section::Error,
    },
    /// The program association section could not be written
    ProgramAssociationSection {
        /// How the section failed
        error: pat::Error,
    },
    /// The TS program map section could not be written
    TsProgramMapSection {
        /// How the section failed
        error: pmt::Error,
    },
    /// The CA section could not be written
    CaSection {
        /// How the section failed
        error: cat::Error,
    },
    /// A Transport Packet could not be written
    Packet {
        /// How the Transport Packet failed
        error: packet::Error,
    },
    /// The `program_number` given is 0, which names the network PID in the
    /// PAT rather than a program
    ProgramNumber {
        /// The `program_number` given
        program_number: u16,
    },
    /// No program of the `program_number` given is registered
    UnregisteredProgram {
        /// The `program_number` given
        program_number: u16,
    },
    /// The PID given already carries a registered stream or the PMT
    PidInUse {
        /// The PID given
        pid: Pid,
    },
    /// No stream is registered on the PID given
    UnregisteredPid {
        /// The PID given
        pid: Pid,
    },
    /// The input would grow the output held for
    /// [`Muxer::poll_output`] past [`MuxerConfig::buffer_limit`]
    BufferFull {
        /// The bytes the output held would grow to
        length: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pid { pid } => write!(
                formatter,
                "PID {:#06x} cannot carry what is written",
                pid.get()
            ),
            Self::StreamId { stream_id } => write!(
                formatter,
                "stream_id {:#04x} is not that of a PES packet",
                stream_id.get()
            ),
            Self::PesPacket { .. } => write!(formatter, "a PES packet could not be written"),
            Self::Section { .. } => write!(formatter, "a section could not be written"),
            Self::ProgramAssociationSection { .. } => {
                write!(
                    formatter,
                    "a program association section could not be written"
                )
            }
            Self::TsProgramMapSection { .. } => {
                write!(formatter, "a TS program map section could not be written")
            }
            Self::CaSection { .. } => write!(formatter, "a CA section could not be written"),
            Self::Packet { .. } => write!(formatter, "a Transport Packet could not be written"),
            Self::ProgramNumber { program_number } => write!(
                formatter,
                "program_number {program_number} names the network PID, not a program"
            ),
            Self::UnregisteredProgram { program_number } => write!(
                formatter,
                "program_number {program_number} is not registered"
            ),
            Self::PidInUse { pid } => write!(
                formatter,
                "PID {:#06x} already carries a stream or the PMT",
                pid.get()
            ),
            Self::UnregisteredPid { pid } => write!(
                formatter,
                "PID {:#06x} carries no registered stream",
                pid.get()
            ),
            Self::BufferFull { length } => write!(
                formatter,
                "the output held would grow to {length} bytes, past buffer_limit"
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::PesPacket { error } => Some(error),
            Self::Section { error } => Some(error),
            Self::ProgramAssociationSection { error } => Some(error),
            Self::TsProgramMapSection { error } => Some(error),
            Self::CaSection { error } => Some(error),
            Self::Packet { error } => Some(error),
            Self::Pid { .. }
            | Self::StreamId { .. }
            | Self::ProgramNumber { .. }
            | Self::UnregisteredProgram { .. }
            | Self::PidInUse { .. }
            | Self::UnregisteredPid { .. }
            | Self::BufferFull { .. } => None,
        }
    }
}

/// Settings of a [`Muxer`]
///
/// [`MuxerConfig::new`] gives the default of each setting; change the fields
/// of the value it returns.
///
/// ```
/// use core::time::Duration;
/// use mpeg2_systems::ts::mux::MuxerConfig;
///
/// let mut config = MuxerConfig::new();
/// config.transport_stream_id = 0x7fe0;
/// config.pcr_interval = Duration::from_millis(30);
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MuxerConfig {
    /// The `transport_stream_id` of the PAT (1 by default)
    pub transport_stream_id: u16,
    /// The time from one sending of the PAT and the PMT to the next
    /// (100 ms by default)
    pub psi_interval: Duration,
    /// The time from one PCR to the next (40 ms by default, the threshold
    /// of the `PCR_repetition_error` of ETSI TR 101 290, shorter than the
    /// 0.1 s ISO/IEC 13818-1 §2.7.2 allows)
    pub pcr_interval: Duration,
    /// How far the PCR runs behind the DTS of the PES packet it is sent with,
    /// or its PTS when it has no DTS (0.7 s by default)
    pub mux_delay: Duration,
    /// The most bytes the output held for [`Muxer::poll_output`] may grow to
    /// (4 MiB by default, the default `pes_buffer_limit` of the demuxer)
    ///
    /// An input that would grow the output held past it is refused, so the
    /// limit must be larger than the Transport Packets of the largest PES
    /// packet given, with the PAT, the PMT and a PCR before it.
    pub buffer_limit: usize,
}

impl MuxerConfig {
    /// Returns the default settings
    #[must_use]
    #[expect(
        clippy::new_without_default,
        reason = "the specification sets no default, which Default would claim"
    )]
    pub const fn new() -> Self {
        Self {
            transport_stream_id: 1,
            psi_interval: Duration::from_millis(100),
            pcr_interval: Duration::from_millis(40),
            mux_delay: Duration::from_millis(700),
            buffer_limit: 4 * 1024 * 1024,
        }
    }
}

/// Multiplexer of PES packets into a Transport Stream, which sends the PAT,
/// the PMT and the PCRs the stream needs on its own
///
/// The program and its streams are registered first, then each PES packet is
/// given with [`handle_pes_packet`](Self::handle_pes_packet), in the order
/// of their DTS (their PTS when they have none) across the streams, and the
/// Transport Packets are taken with [`poll_output`](Self::poll_output). The
/// output may be held until `poll_output` is called; after
/// [`finish`](Self::finish), `poll_output` gives all that is held. Every
/// Transport Packet is written through a [`PacketWriter`].
///
/// - The PAT and the PMT, `version_number` 0, are made from what is
///   registered. They are sent before the first PES packet, and again
///   before the first PES packet [`MuxerConfig::psi_interval`] or more after
///   the last sending.
/// - The PCR is sent before the first PES packet, and again before the first
///   PES packet [`MuxerConfig::pcr_interval`] or more after the last PCR: in
///   the adaptation field of that PES packet's first Transport Packet when
///   it is on the PCR PID, in a Transport Packet of its own on the PCR PID
///   otherwise.
/// - Its value is the DTS of the PES packet (its PTS when it has none) less
///   [`MuxerConfig::mux_delay`], held from going back; a PES packet without
///   either does not move it. The PTS and the DTS are written as given.
/// - The PCR PID is the `pcr_pid` registered with the program. Without one,
///   it is the PID of the first video stream registered, or of the first
///   stream when none is video. The video streams are those of `stream_type`
///   0x01, 0x02, 0x10, 0x1B, 0x21, 0x24, 0x32, 0x33 and 0x35, the base
///   video of each coding Table 2-34 lists, not a sub-bitstream, an
///   additional view or an enhancement.
///
/// The muxer does not write null packets to keep a constant bit rate.
/// It carries one program only.
/// It does not check that the stream fits the buffers of the T-STD.
/// It does not pace the output in real time; the caller sends it at the pace
/// its PCRs give.
/// It does not check the order of the PES packets across the streams; the
/// caller gives them in the order of their DTS.
///
/// ```
/// use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
/// use mpeg2_systems::stream_type::StreamType;
/// use mpeg2_systems::time::Timestamp;
/// use mpeg2_systems::ts::Pid;
/// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
/// use mpeg2_systems::ts::mux::{Muxer, MuxerConfig};
///
/// let (pmt_pid, video_pid) = (Pid::try_from(0x1000)?, Pid::try_from(0x0100)?);
/// let mut muxer = Muxer::new(MuxerConfig::new());
/// let mut stream = Vec::new();
///
/// // Register the program and its stream
/// muxer.add_program(1, pmt_pid, None, &[])?;
/// muxer.add_stream(1, video_pid, StreamType::ISO_IEC_13818_2_VIDEO, &[])?;
/// // Give an access unit a second in, and take the Transport Packets
/// let access_unit = vec![0xaa; 1_000];
/// let mut header = OptionalHeader::new();
/// header.pts = Some(Timestamp::try_from(90_000)?);
/// let mut packet = PesPacket::new(StreamId::from(0xe0), &access_unit);
/// packet.optional_header = Some(header);
/// muxer.handle_pes_packet(video_pid, &packet)?;
/// muxer.poll_output(&mut stream);
/// // The end, and what is still held
/// muxer.finish();
/// muxer.poll_output(&mut stream);
///
/// // Read back
/// let mut demuxer = Demuxer::new(DemuxerConfig::default());
/// demuxer.handle_input(&stream);
/// demuxer.finish();
/// let mut pids = Vec::new();
/// let mut payloads = Vec::new();
/// while let Some(event) = demuxer.poll_event() {
///     if let Event::TsProgramMapSection { section, .. } = &event {
///         pids.extend(section.elementary_streams.iter().flatten().map(|stream| stream.elementary_pid));
///     } else if let Event::PesPacket { packet, .. } = &event {
///         payloads.push(packet.payload.to_vec());
///     }
///     drop(event);
///     for pid in pids.drain(..) {
///         demuxer.subscribe_pes(pid);
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
    /// The clock and when the PAT, the PMT and the PCR were last sent, `None`
    /// before the first PES packet
    // Why not an `Option` for each time, or a clock of 0 before the first
    // PES packet: one could be set without the others, and 0 could not be
    // told from the time 0.
    schedule: Option<Schedule>,
    /// The Transport Packets [`poll_output`](Self::poll_output) gives next
    // Why not write into the caller's buffer on input: the output may be
    // held for reordering once that is added, and only `handle_pes_packet`
    // can fail.
    pending: Vec<u8>,
    /// The bytes of the PES packet, the program loop and the elementary
    /// stream loop being written, cleared on each input
    // Why not keep the PAT and the PMT encoded: they are made again from the
    // registration each time, and a reused buffer allocates nothing.
    scratch: Vec<u8>,
    /// The bodies of the PAT and the PMT being written, cleared on each
    /// input that sends them
    // Why not `write` the PAT and the PMT: it allocates a body of its own.
    section_bodies: Vec<u8>,
    finished: bool,
}

/// The time of the 27 MHz system clock the input has reached, and the times
/// the PAT and the PMT and the PCR were last sent, each on the line
/// [`unwrap_timestamp`] places the time stamps on, times 300, and before
/// [`MuxerConfig::mux_delay`] is taken off
#[derive(Debug, Clone, Copy)]
struct Schedule {
    // Why not take it from the PES packet each time: a PES packet without a
    // PTS or a DTS, or with an earlier DTS than one before it, must not move
    // it back.
    clock: u64,
    psi_sent_at: u64,
    // Why not psi_sent_at: the PSI and the PCR are sent at intervals of
    // their own.
    pcr_sent_at: u64,
}

impl Muxer {
    /// Returns a muxer of `config` with nothing registered
    #[must_use]
    pub const fn new(config: MuxerConfig) -> Self {
        Self {
            config,
            writer: PacketWriter::new(),
            program: None,
            schedule: None,
            pending: Vec::new(),
            scratch: Vec::new(),
            section_bodies: Vec::new(),
            finished: false,
        }
    }

    /// Registers the program, its PMT on `program_map_pid`
    ///
    /// `program_info` is the descriptors of the program as written, which
    /// the muxer copies. The PCR goes on `pcr_pid`, or, when it is `None`,
    /// on the PID [`Muxer`] says.
    ///
    /// # Errors
    ///
    /// * [`ProgramNumber`](Error::ProgramNumber): `program_number` is 0.
    /// * [`Pid`](Error::Pid): `program_map_pid` or `pcr_pid` is outside
    ///   0x0010 to 0x1FFE, the PIDs Table 2-3 leaves free to assign.
    /// * [`TsProgramMapSection`](Error::TsProgramMapSection):
    ///   [`TsProgramMapSection::write`](pmt::TsProgramMapSection::write) refuses the PMT of the program, as it
    ///   does `program_info` that is not whole descriptors or is too long.
    ///
    /// # Panics
    ///
    /// Panics if a program is registered already.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn add_program(
        &mut self,
        program_number: u16,
        program_map_pid: Pid,
        pcr_pid: Option<Pid>,
        program_info: &[u8],
    ) -> Result<(), Error> {
        assert!(self.program.is_none(), "a program is registered already");
        let Some(program_number) = NonZeroU16::new(program_number) else {
            return Err(Error::ProgramNumber { program_number });
        };
        for pid in core::iter::once(program_map_pid).chain(pcr_pid) {
            if !pid.is_assignable() {
                return Err(Error::Pid { pid });
            }
        }
        let program = ProgramDefinition {
            program_number,
            program_map_pid,
            pcr_pid,
            program_info: program_info.to_vec(),
            streams: Vec::new(),
        };
        program.check(self.config.transport_stream_id)?;
        self.program = Some(program);
        Ok(())
    }

    /// Registers a stream of the program `program_number`, its PES packets
    /// on `pid`
    ///
    /// `es_info` is the descriptors of the stream as written, which the
    /// muxer copies.
    ///
    /// # Errors
    ///
    /// * [`UnregisteredProgram`](Error::UnregisteredProgram): no program of
    ///   `program_number` is registered.
    /// * [`Pid`](Error::Pid): `pid` is outside 0x0010 to 0x1FFE.
    /// * [`PidInUse`](Error::PidInUse): `pid` is that of a stream registered
    ///   already, or of the PMT.
    /// * [`TsProgramMapSection`](Error::TsProgramMapSection):
    ///   [`TsProgramMapSection::write`](pmt::TsProgramMapSection::write) refuses the PMT with the stream, as
    ///   it does `es_info` that is not whole descriptors or is too long, or
    ///   a PMT past the length of a section.
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
        pid: Pid,
        stream_type: StreamType,
        es_info: &[u8],
    ) -> Result<(), Error> {
        assert!(
            self.schedule.is_none(),
            "a stream is registered after the first PES packet"
        );
        let transport_stream_id = self.config.transport_stream_id;
        let Some(program) = self
            .program
            .as_mut()
            .filter(|program| program.program_number.get() == program_number)
        else {
            return Err(Error::UnregisteredProgram { program_number });
        };
        if !pid.is_assignable() {
            return Err(Error::Pid { pid });
        }
        if pid == program.program_map_pid || program.stream(pid).is_some() {
            return Err(Error::PidInUse { pid });
        }
        program.streams.push(Stream {
            pid,
            stream_type,
            es_info: es_info.to_vec(),
        });
        program.check(transport_stream_id).inspect_err(|_| {
            program.streams.pop();
        })
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
    /// * [`StreamId`](Error::StreamId): the `stream_id` is below 0xBD, not
    ///   that of a PES packet.
    /// * [`PesPacket`](Error::PesPacket): [`PesPacket::write`] refuses the
    ///   packet.
    /// * [`BufferFull`](Error::BufferFull): the Transport Packets would grow
    ///   the output held past [`MuxerConfig::buffer_limit`]; once
    ///   `poll_output` has taken it, the same input may be given again.
    ///
    /// # Panics
    ///
    /// Panics if [`finish`](Self::finish) has been called.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn handle_pes_packet(&mut self, pid: Pid, packet: &PesPacket<'_>) -> Result<(), Error> {
        assert!(!self.finished, "a PES packet is given after finish");
        let Some(program) = self
            .program
            .as_ref()
            .filter(|program| program.stream(pid).is_some())
        else {
            return Err(Error::UnregisteredPid { pid });
        };
        let stream_id = packet.stream_id;
        if !stream_id.is_pes_packet() {
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
        scratch.clear();
        packet
            .write(scratch)
            .map_err(|error| Error::PesPacket { error })?;
        let pes_length = scratch.len();

        let at = packet
            .optional_header
            .as_ref()
            .and_then(|header| header.dts.or(header.pts))
            .map(|timestamp| {
                unwrap_timestamp(
                    schedule.map(|schedule| schedule.clock / TICKS_PER_TIMESTAMP_TICK),
                    timestamp,
                )
                .saturating_mul(TICKS_PER_TIMESTAMP_TICK)
            });
        let (send_psi, send_pcr, next) = match *schedule {
            None => {
                let clock = at.unwrap_or(ClockReference::MODULUS);
                let next = Schedule {
                    clock,
                    psi_sent_at: clock,
                    pcr_sent_at: clock,
                };
                (true, true, next)
            }
            Some(schedule) => {
                let clock = at.map_or(schedule.clock, |at| schedule.clock.max(at));
                let due = |sent_at: u64, interval| clock.saturating_sub(sent_at) >= ticks(interval);
                let send_psi = due(schedule.psi_sent_at, config.psi_interval);
                let send_pcr = due(schedule.pcr_sent_at, config.pcr_interval);
                let next = Schedule {
                    clock,
                    psi_sent_at: if send_psi {
                        clock
                    } else {
                        schedule.psi_sent_at
                    },
                    pcr_sent_at: if send_pcr {
                        clock
                    } else {
                        schedule.pcr_sent_at
                    },
                };
                (send_psi, send_pcr, next)
            }
        };
        let pcr = ClockReference::from_ticks(next.clock.wrapping_sub(ticks(config.mux_delay)));
        let pcr_pid = program.pcr_pid().filter(|_| send_pcr);
        let adaptation_field = (pcr_pid == Some(pid)).then(|| {
            let mut adaptation_field = AdaptationField::new();
            adaptation_field.program_clock_reference = Some(pcr);
            adaptation_field
        });
        let pcr_packet_pid = pcr_pid.filter(|&pcr_pid| pcr_pid != pid);

        section_bodies.clear();
        let sections = if send_psi {
            let pat_loop_length = program.write_loops(scratch)?;
            let (pat_loop, pmt_loop) = scratch.split_at(pes_length).1.split_at(pat_loop_length);
            let (pat, pmt) = program.sections(config.transport_stream_id, pat_loop, pmt_loop);
            pat.write_body(section_bodies)
                .map_err(|error| Error::ProgramAssociationSection { error })?;
            let pat_body_length = section_bodies.len();
            pmt.write_body(section_bodies)
                .map_err(|error| Error::TsProgramMapSection { error })?;
            let (pat_body, pmt_body) = section_bodies.split_at(pat_body_length);
            Some((pat.section(pat_body), pmt.section(pmt_body)))
        } else {
            None
        };
        let pes_bytes = scratch.split_at(pes_length).0;

        let length = sections
            .as_ref()
            .map_or(0, |(pat, pmt)| {
                section_packets_length(pat.encoded_len())
                    .saturating_add(section_packets_length(pmt.encoded_len()))
            })
            .saturating_add(if pcr_packet_pid.is_some() {
                PACKET_LENGTH
            } else {
                0
            })
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

        if let Some((pat, pmt)) = &sections {
            writer.write_section(Pid::PAT, pat, pending)?;
            writer.write_section(program.program_map_pid, pmt, pending)?;
        }
        if let Some(pcr_packet_pid) = pcr_packet_pid {
            writer.write_pcr_packet(pcr_packet_pid, pcr, pending)?;
        }
        writer.write_payload_unit(
            pid,
            |out| {
                out.extend_from_slice(pes_bytes);
                Ok(())
            },
            adaptation_field,
            Stuffing::AdaptationField,
            pending,
        )?;
        *schedule = Some(next);
        Ok(())
    }

    /// Appends to `out` all the Transport Packets that can be given
    pub fn poll_output(&mut self, out: &mut Vec<u8>) {
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

/// Returns `duration` in ticks of the 27 MHz system clock
fn ticks(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos().saturating_mul(SYSTEM_CLOCK_FREQUENCY) / 1_000_000_000)
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "time stamps and lengths are small constants well inside their range"
)]
mod tests;
