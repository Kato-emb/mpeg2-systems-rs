//! The events a [`Demuxer`](super::Demuxer) reports, and the failures among them

use core::fmt;

use crate::pes::{self, PesPacket};
use crate::ps::{PackHeader, ProgramStreamMap, SystemHeader, pack_header, psm, system_header};

/// What a [`Demuxer`](super::Demuxer) observed in its input
///
/// An event borrows the bytes it carries from the demuxer, until the next call
/// on the demuxer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Event<'a> {
    /// The first pack header that reads was found, and elements are cut from
    /// it on
    ///
    /// The pack header comes next as an [`Event::PackHeader`] of its own.
    SyncAcquired,
    /// A pack header, each one the stream carries
    PackHeader(PackHeader),
    /// A system header, the first one and each one whose bytes differ from
    /// the one reported before
    ///
    /// A system header resent unchanged is not reported. The one reported is
    /// remembered across [`Demuxer::finish`](super::Demuxer::finish) and
    /// forgotten at [`Event::ProgramEnd`].
    SystemHeader(SystemHeader<'a>),
    /// A program stream map, the first one and each one at a version not
    /// reported before
    ///
    /// A map resent at the version last reported, and one with
    /// `current_next_indicator` `'0'`, which announces the next version, are
    /// not reported. The version reported is remembered across
    /// [`Demuxer::finish`](super::Demuxer::finish) and forgotten at
    /// [`Event::ProgramEnd`].
    #[doc(alias = "PSM")]
    ProgramStreamMap(ProgramStreamMap<'a>),
    /// A PES packet, each one the stream carries
    ///
    /// The demuxer does not read the elementary stream the packet carries;
    /// that is up to the caller.
    #[doc(alias = "PES")]
    PesPacket(PesPacket<'a>),
    /// `MPEG_program_end_code`, which ends the Program Stream
    ///
    /// The demuxer drops its synchronization, and searches for the first pack
    /// header of a stream that may follow.
    ProgramEnd,
    /// Input passed over while searching for a pack header
    ///
    /// Reported for the bytes before the first pack header that reads, those
    /// after an [`Event::ProgramEnd`] before the next one, and those
    /// [`Demuxer::finish`](super::Demuxer::finish) leaves before one. A long
    /// run arrives as several events, so the bytes the demuxer holds stay
    /// bounded.
    Unsynchronized {
        /// The bytes, in input order
        bytes: &'a [u8],
    },
    /// Input that could not be cut into an element while synchronized, up to
    /// the next start code
    ///
    /// Reported for a start code the demuxer does not cut, bytes that are no
    /// start code, an element that does not read (an [`Event::Error`] before
    /// this event says why), and a PES packet whose `PES_packet_length` is 0,
    /// which a Program Stream does not allow.
    Unframable {
        /// The bytes, in input order
        bytes: &'a [u8],
    },
    /// The bytes of an element that [`Demuxer::finish`](super::Demuxer::finish)
    /// ended short of its declared length
    Truncated {
        /// The bytes, in input order
        bytes: &'a [u8],
    },
    /// An element of the Program Stream did not read; demuxing goes on
    Error(Error),
}

/// Failure of an element a [`Demuxer`](super::Demuxer) cut, reported as an
/// [`Event::Error`] before the [`Event::Unframable`] that carries its bytes
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// A pack header could not be read, an ISO/IEC 11172-1 pack among them
    PackHeader {
        /// How the pack header failed
        error: pack_header::Error,
    },
    /// A system header could not be read
    SystemHeader {
        /// How the system header failed
        error: system_header::Error,
    },
    /// A program stream map could not be read, its `CRC_32` failing included
    ProgramStreamMap {
        /// How the program stream map failed
        error: psm::Error,
    },
    /// A PES packet could not be read
    PesPacket {
        /// How the PES packet failed
        error: pes::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PackHeader { .. } => write!(formatter, "a pack header could not be read"),
            Self::SystemHeader { .. } => write!(formatter, "a system header could not be read"),
            Self::ProgramStreamMap { .. } => {
                write!(formatter, "a program stream map could not be read")
            }
            Self::PesPacket { .. } => write!(formatter, "a PES packet could not be read"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::PackHeader { error } => Some(error),
            Self::SystemHeader { error } => Some(error),
            Self::ProgramStreamMap { error } => Some(error),
            Self::PesPacket { error } => Some(error),
        }
    }
}
