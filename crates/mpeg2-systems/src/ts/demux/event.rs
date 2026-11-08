//! The events a [`Demuxer`](super::Demuxer) reports, and the failures among them

use core::fmt;

use crate::psi::cat::{self, CaSection};
use crate::psi::pat::{self, ProgramAssociationSection};
use crate::psi::pmt::{self, TsProgramMapSection};
use crate::psi::section::{self, Section};
use crate::time::ClockReference;
use crate::ts::demux::PacketSize;
use crate::ts::{ContinuityCounter, Pid, packet};

/// What a [`Demuxer`](super::Demuxer) observed in its input
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
    /// A section of a PAT, the first time each of its sections is seen at a
    /// version
    ///
    /// Each section of the table comes as an event of its own; the demuxer
    /// does not join them into one table, so a caller gathers the sections of
    /// one `version_number` by `section_number` and `last_section_number`. A
    /// section resent at the version last reported, and one with
    /// `current_next_indicator` `'0'`, which announces the next version, are
    /// not reported. The versions reported are remembered across
    /// [`Error::SyncLost`] and [`Demuxer::finish`](super::Demuxer::finish).
    ///
    /// The demuxer follows the program maps the PAT names: once every
    /// section of a version is in, a program the version no longer names
    /// stops being followed, and its PMT is reported again if it comes back,
    /// even at the version last reported.
    #[doc(alias = "PAT")]
    ProgramAssociationSection(ProgramAssociationSection<'a>),
    /// A section of a PMT, on a PID a PAT names for a program map, the first
    /// time it is seen at a version
    ///
    /// Only the PMT of a program the PAT names on that PID is typed; one of
    /// another `program_number` comes as an [`Event::Section`]. Reported by
    /// version per program, as
    /// [`Event::ProgramAssociationSection`] is. No packet after the one
    /// that completes the section is analyzed before this event is polled.
    #[doc(alias = "PMT")]
    TsProgramMapSection {
        /// The PID the section was carried on
        pid: Pid,
        /// The section, borrowed from the demuxer
        section: TsProgramMapSection<'a>,
    },
    /// A section of the CAT, the first time each of its sections is seen at
    /// a version
    ///
    /// Reported by version, as [`Event::ProgramAssociationSection`] is.
    #[doc(alias = "CAT")]
    CaSection(CaSection<'a>),
    /// A section of another table, reassembled on a PID that carries PSI,
    /// its `CRC_32` verified
    ///
    /// Sections are reassembled on the PAT and CAT PIDs, and on each PID a
    /// PAT names for a program map; the PAT and CAT sections among them, and
    /// the PMT sections of the programs the PAT names, come as their own
    /// events. A section with `section_syntax_indicator`
    /// `'0'` has no `CRC_32` and is reported as read. The demuxer reports
    /// every such section, a repeated one included; reading a table from it
    /// is up to the caller.
    Section {
        /// The PID the section was carried on
        pid: Pid,
        /// The section, borrowed from the demuxer
        section: Section<'a>,
    },
    /// Input that was not cut into a Transport Packet
    ///
    /// Reported for the bytes before the grid is found, those the search
    /// walks past after [`Error::SyncLost`], and those
    /// [`Demuxer::finish`](super::Demuxer::finish) leaves that fill no packet. A long run arrives as several events, so
    /// the bytes the demuxer holds stay bounded. The bytes
    /// of these events and of the packets cut add up to every byte handed to
    /// [`Demuxer::handle_input`](super::Demuxer::handle_input).
    Unsynchronized {
        /// The bytes, in input order
        bytes: &'a [u8],
    },
    /// The input broke a rule of the Transport Stream; demuxing goes on
    Error(Error),
}

/// Failure of the Transport Stream a [`Demuxer`](super::Demuxer) reads,
/// reported as an [`Event::Error`]
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
    /// A section on the PAT PID with the PAT's `table_id` is not a PAT
    /// section
    ///
    /// The section is dropped.
    ProgramAssociationSection {
        /// How the section failed
        error: pat::Error,
    },
    /// A section with the PMT's `table_id`, on a PID a PAT names for a
    /// program map, is not a PMT section
    ///
    /// The section is dropped.
    TsProgramMapSection {
        /// The PID the section was carried on
        pid: Pid,
        /// How the section failed
        error: pmt::Error,
    },
    /// A section on the CAT PID with the CAT's `table_id` is not a CAT
    /// section
    ///
    /// The section is dropped.
    CaSection {
        /// How the section failed
        error: cat::Error,
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
            Self::ProgramAssociationSection { .. } => {
                write!(formatter, "a program association section could not be read")
            }
            Self::TsProgramMapSection { pid, .. } => write!(
                formatter,
                "a TS program map section on PID {:#06x} could not be read",
                pid.get()
            ),
            Self::CaSection { .. } => write!(formatter, "a CA section could not be read"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Packet { error } => Some(error),
            Self::Section { error, .. } => Some(error),
            Self::ProgramAssociationSection { error } => Some(error),
            Self::TsProgramMapSection { error, .. } => Some(error),
            Self::CaSection { error } => Some(error),
            Self::SyncByteMissing { .. }
            | Self::SyncLost
            | Self::TransportErrorIndicator { .. }
            | Self::Discontinuity { .. } => None,
        }
    }
}
