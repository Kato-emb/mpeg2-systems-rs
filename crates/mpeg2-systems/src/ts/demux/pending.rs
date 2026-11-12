//! The events a demuxer queues before they are polled, holding the bytes
//! they lend as ranges

use core::ops::Range;

use super::Event;
use crate::psi::section::Section;
use crate::ts::Pid;
use crate::ts::packet::TransportScramblingControl;

/// An event queued for [`Demuxer::poll_event`](super::Demuxer::poll_event),
/// holding the ranges of the bytes it lends rather than the bytes
#[derive(Debug, Clone)]
pub(super) enum Pending {
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
pub(super) enum Lent {
    /// A range of the framer's buffer: the section or PES packet lay inside
    /// one packet
    Packet(Range<usize>),
    /// A range of the section, or PES, reassembly buffer of the PID: the
    /// section or PES packet crossed packets
    Reassembled(Range<usize>),
}
