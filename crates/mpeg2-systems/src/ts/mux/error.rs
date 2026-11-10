//! How writing a Transport Stream, or registering or multiplexing a stream,
//! fails

use core::fmt;
use core::time::Duration;

use crate::pes::{self, StreamId};
use crate::psi::{cat, pat, pmt, section};
use crate::time::{ClockReference, Timestamp};
use crate::ts::Pid;
use crate::ts::packet;

/// Failure to write a PES packet or a section as Transport Packets, or to
/// register or multiplex a stream with a [`Muxer`](super::Muxer)
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The PID given cannot carry what is written: a PES packet only goes on
    /// a PID from 0x0010 to 0x1FFE, and nothing on the PID of null packets
    Pid {
        /// The PID given
        pid: Pid,
    },
    /// The `stream_id` given is one this entry point does not take
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
    /// The `pcr_interval` set is shorter than a tick of the 27 MHz system
    /// clock, or longer than the 0.1 s ISO/IEC 13818-1 §2.7.2 allows
    PcrInterval {
        /// The `pcr_interval` set
        pcr_interval: Duration,
    },
    /// The first PES packet supplied for a stream that carries the optional
    /// header has no PTS, which this muxer requires to initialize the
    /// stream's timeline, or the first PES packet supplied to the muxer has
    /// no time stamp to start the PCRs from
    MissingPts {
        /// The PID of the stream
        pid: Pid,
    },
    /// The DTS of a PES packet, or its PTS when it has none, is before that
    /// of the PES packet given before it on the same stream
    DecreasingTimestamp {
        /// The PID of the stream
        pid: Pid,
        /// The DTS, or the PTS, given
        timestamp: Timestamp,
        /// The DTS, or the PTS, of the PES packet before it
        previous: Timestamp,
    },
    /// The time stamp of the input leaps so far past the one before it that
    /// the PCRs, and the PATs and PMTs, to send in the leap do not fit
    /// [`MuxerConfig::buffer_limit`](super::MuxerConfig::buffer_limit) even with nothing held
    TimestampGap {
        /// The leap, in ticks of the 90 kHz clock of PTS and DTS
        gap: u64,
    },
    /// The input would grow the output held for
    /// [`Muxer::poll_output`](super::Muxer::poll_output) past [`MuxerConfig::buffer_limit`](super::MuxerConfig::buffer_limit)
    BufferFull {
        /// The bytes the output held would grow to
        length: usize,
    },
    /// The `transport_private_data` of
    /// [`TransportPacketFields`](super::TransportPacketFields) is longer than
    /// the adaptation field of the PES packet's first Transport Packet holds
    TransportPrivateDataLength {
        /// The bytes of `transport_private_data` given
        length: usize,
        /// The most bytes the adaptation field holds
        limit: usize,
    },
    /// `random_access_indicator` is set on the PCR PID, which takes a PCR,
    /// at a time no later than that of the PCR sent before
    RandomAccessIndicator {
        /// The PID of the PES packet, the PCR PID
        pid: Pid,
        /// The value of the PCR the packet would carry, that of the PCR sent
        /// before
        program_clock_reference: ClockReference,
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
                "stream_id {:#04x} is not taken here",
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
            Self::PcrInterval { pcr_interval } => write!(
                formatter,
                "pcr_interval {pcr_interval:?} is outside one tick of the system clock to 0.1 s"
            ),
            Self::MissingPts { pid } => write!(
                formatter,
                "the first PES packet on PID {:#06x} has no PTS",
                pid.get()
            ),
            Self::DecreasingTimestamp {
                pid,
                timestamp,
                previous,
            } => write!(
                formatter,
                "time stamp {} on PID {:#06x} is before {} of the PES packet before it",
                timestamp.get(),
                pid.get(),
                previous.get()
            ),
            Self::TimestampGap { gap } => write!(
                formatter,
                "a leap of {gap} ticks in the time stamps needs more PCRs and tables than buffer_limit holds"
            ),
            Self::BufferFull { length } => write!(
                formatter,
                "the output held would grow to {length} bytes, past buffer_limit"
            ),
            Self::RandomAccessIndicator {
                pid,
                program_clock_reference,
            } => write!(
                formatter,
                "random_access_indicator on PCR PID {:#06x} needs a PCR after the one of {} already sent",
                pid.get(),
                program_clock_reference.base()
            ),
            Self::TransportPrivateDataLength { length, limit } => write!(
                formatter,
                "transport_private_data of {length} bytes is longer than the {limit} bytes the adaptation field holds"
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
            | Self::PcrInterval { .. }
            | Self::MissingPts { .. }
            | Self::DecreasingTimestamp { .. }
            | Self::TimestampGap { .. }
            | Self::BufferFull { .. }
            | Self::TransportPrivateDataLength { .. }
            | Self::RandomAccessIndicator { .. } => None,
        }
    }
}
