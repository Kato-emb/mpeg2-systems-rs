//! Reassembly of the PES packets a subscribed PID carries across Transport
//! Packets

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::ops::Range;

use super::continuity::Verdict;
use crate::pes::{self, PesPacket, PesPacketLength};
use crate::ts::Pid;
use crate::ts::demux::{Error, Event, Lent, Pending};
use crate::ts::packet::{PACKET_LENGTH, TransportPacket, TransportScramblingControl};

/// Where a Transport Packet stands in its PID's sequence, as far as the PES
/// packet in flight cares
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sequence {
    /// The packet follows the one before, or carries no counter to check
    InOrder,
    /// The one duplicate §2.4.3.3 allows: its payload repeats bytes already
    /// gathered
    Duplicate,
    /// Packets were lost, or `discontinuity_indicator` excused a jump
    Gap,
    /// `transport_error_indicator` is set: nothing in the packet is reliable
    Errored,
}

impl From<Verdict> for Sequence {
    fn from(verdict: Verdict) -> Self {
        match verdict {
            Verdict::InOrder => Self::InOrder,
            Verdict::Duplicate => Self::Duplicate,
            Verdict::Broken { .. } | Verdict::Excused => Self::Gap,
        }
    }
}

/// What a Transport Packet means for the PES packet in flight on its PID
#[derive(Debug, Clone, Copy)]
enum Observation {
    /// A duplicate, whose payload is already gathered
    Repeated,
    /// A gap, or a packet that cannot be trusted, with no
    /// `payload_unit_start_indicator` after it to start a PES packet from
    Interrupted,
    /// A continuation with no PES packet in flight: nothing in the payload
    /// says where a PES packet starts
    Stray,
    /// `payload_unit_start_indicator` is set on a packet in order: a new PES
    /// packet starts in the payload
    Starts(Growth),
    /// `payload_unit_start_indicator` is set on a packet after a gap
    StartsAfterGap(Growth),
    /// The payload continues the PES packet in flight
    Continues(Growth),
}

/// What a payload does to the PES packet it belongs to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Growth {
    /// The PES packet ends this many bytes into the payload
    Ends { length: usize },
    /// The whole payload belongs to the PES packet, which goes on within
    /// the limit
    Gathers,
    /// The whole payload belongs to the PES packet, which it takes past the
    /// limit
    Overruns,
}

/// The PIDs subscribed to, and the PES packet in flight on each
#[derive(Debug, Clone)]
pub(super) struct PesReassembly {
    buffer_limit: usize,
    reassemblers: BTreeMap<Pid, PesReassembler>,
}

/// The PES packets of one subscribed PID
///
/// `buffer[..in_flight_start]` holds the PES packets the latest packet of
/// the PID ended, which stay lent until the next packet of the PID;
/// `buffer[in_flight_start..]` is the PES packet in flight.
#[derive(Debug, Clone)]
struct PesReassembler {
    buffer: Vec<u8>,
    in_flight_start: usize,
    /// Input offset of the unit of the Transport Packet the PES packet in
    /// flight started in
    start_offset: u64,
    /// `transport_scrambling_control` of the Transport Packet the PES packet
    /// in flight started in
    // Why not read it from the packets that follow: the packets of one PES
    // packet need not agree, and the one that starts it decides whether its
    // header can be read.
    transport_scrambling_control: TransportScramblingControl,
}

impl PesReassembly {
    pub(super) const fn new(buffer_limit: usize) -> Self {
        Self {
            buffer_limit,
            reassemblers: BTreeMap::new(),
        }
    }

    /// Reassembles the PES packets of `pid` from its next
    /// `payload_unit_start_indicator`, keeping the PES packet in flight of a
    /// PID already subscribed
    pub(super) fn subscribe(&mut self, pid: Pid) {
        self.reassemblers
            .entry(pid)
            .or_insert_with(|| PesReassembler {
                buffer: Vec::new(),
                in_flight_start: 0,
                start_offset: 0,
                transport_scrambling_control: TransportScramblingControl::NOT_SCRAMBLED,
            });
    }

    /// Drops the PID's subscription, every byte it gathered, and the queued
    /// events of its PES packets
    pub(super) fn unsubscribe(&mut self, pid: Pid, pending: &mut VecDeque<Pending>) {
        // Why no event for the PES packet in flight: the caller that left
        // the PID has no use for it, and reporting it would oblige the caller
        // to keep filtering events for a PID it has just left. Why drop the
        // queued ones too: they lend the buffer dropped here.
        self.reassemblers.remove(&pid);
        pending.retain(|pending| match pending {
            Pending::PesPacket { pid: queued, .. }
            | Pending::ScrambledPesPacket { pid: queued, .. }
            | Pending::TruncatedPesPacket { pid: queued, .. }
            | Pending::Ready(Event::Error(Error::PesBufferOverrun { pid: queued, .. })) => {
                *queued != pid
            }
            Pending::Ready(_) | Pending::Unsynchronized(_) | Pending::Section { .. } => true,
        });
    }

    /// Returns the bytes of a range the queue holds for `pid`
    pub(super) fn bytes(&self, pid: Pid, range: Range<usize>) -> &[u8] {
        self.reassemblers
            .get(&pid)
            .and_then(|reassembler| reassembler.buffer.get(range))
            .unwrap_or_default()
    }

    /// Drops the PES packet in flight on every PID, keeping the
    /// subscriptions
    pub(super) fn discard_in_flight(&mut self) {
        // Why not drop the subscriptions: a gap in the input ends the bytes
        // in flight, not the caller's interest in the PID. Why not report the
        // bytes as truncated, as a continuity break does: Error::SyncLost
        // already says every PID lost what was in flight.
        for reassembler in self.reassemblers.values_mut() {
            reassembler.buffer.truncate(reassembler.in_flight_start);
        }
    }

    /// Ends the PES packet in flight on every PID at the end of input, and
    /// queues it
    pub(super) fn finish(&mut self, pending: &mut VecDeque<Pending>) {
        for (&pid, reassembler) in &mut self.reassemblers {
            match length_of(reassembler.in_flight(), reassembler.is_scrambled()) {
                PesPacketLength::Unbounded => reassembler.close_ended(pid, pending),
                // Why not read a packet of a declared length: it ends the
                // moment its bytes are in, so one still in flight is short of
                // its length.
                PesPacketLength::Bounded(_) | PesPacketLength::NeedMoreData => {
                    reassembler.close_truncated(pid, pending);
                }
            }
        }
    }

    /// Takes a packet, which `sequence` places in its PID's sequence, whose
    /// payload starts at `payload_start` in the framer's buffer and whose
    /// unit starts at input `offset`, and queues the PES packets it ends, if
    /// its PID is subscribed
    pub(super) fn push(
        &mut self,
        packet: &TransportPacket<'_>,
        sequence: Sequence,
        payload_start: usize,
        offset: u64,
        pending: &mut VecDeque<Pending>,
    ) {
        let pid = packet.pid;
        let Some(reassembler) = self.reassemblers.get_mut(&pid) else {
            return;
        };
        let payload = packet.payload.unwrap_or_default();
        match observe(reassembler, sequence, packet, self.buffer_limit) {
            // Why not reassemble a duplicate, nor end what it would lend:
            // its payload repeats bytes already gathered.
            Observation::Repeated => {}
            // Why cut the PES packet in flight on a packet that cannot be
            // read: its payload may belong to it or not, so the bytes after
            // it would be stitched across a gap.
            Observation::Interrupted => {
                reassembler.lend_no_more();
                reassembler.close_truncated(pid, pending);
            }
            Observation::Stray => reassembler.lend_no_more(),
            Observation::Starts(growth) => {
                reassembler.lend_no_more();
                reassembler.close_ended(pid, pending);
                reassembler.start(pid, packet, growth, (payload_start, offset), pending);
            }
            Observation::StartsAfterGap(growth) => {
                reassembler.lend_no_more();
                reassembler.close_truncated(pid, pending);
                reassembler.start(pid, packet, growth, (payload_start, offset), pending);
            }
            Observation::Continues(growth) => {
                reassembler.lend_no_more();
                match growth {
                    // Why drop the bytes after the end: a PES packet starts
                    // only at a payload_unit_start_indicator, so the bytes
                    // after one that ends inside a Transport Packet are
                    // stuffing.
                    Growth::Ends { length } => {
                        reassembler
                            .buffer
                            .extend_from_slice(payload.get(..length).unwrap_or_default());
                        reassembler.close_ended(pid, pending);
                    }
                    Growth::Gathers => reassembler.buffer.extend_from_slice(payload),
                    Growth::Overruns => reassembler.overrun(pid, payload, pending),
                }
            }
        }
        debug_assert!(
            reassembler.buffer.len() <= self.buffer_limit.saturating_add(PACKET_LENGTH),
            "the PES reassembly buffer of a PID outgrew the limit by more than a packet"
        );
    }
}

impl PesReassembler {
    fn in_flight(&self) -> &[u8] {
        self.buffer.get(self.in_flight_start..).unwrap_or_default()
    }

    /// Drops the PES packets the previous packet of the PID ended, which no
    /// queued event lends any more
    fn lend_no_more(&mut self) {
        self.buffer.drain(..self.in_flight_start);
        self.in_flight_start = 0;
    }

    /// Ends the PES packet in flight, and returns the range of the buffer it
    /// is lent from, or `None` if none is in flight
    fn close(&mut self) -> Option<Range<usize>> {
        let start = self.in_flight_start;
        self.in_flight_start = self.buffer.len();
        (start < self.in_flight_start).then_some(start..self.in_flight_start)
    }

    /// Starts a PES packet in the payload of `packet`, which starts at
    /// `payload_start` in the framer's buffer and whose unit starts at input
    /// `offset`
    fn start(
        &mut self,
        pid: Pid,
        packet: &TransportPacket<'_>,
        growth: Growth,
        (payload_start, offset): (usize, u64),
        pending: &mut VecDeque<Pending>,
    ) {
        let payload = packet.payload.unwrap_or_default();
        self.start_offset = offset;
        self.transport_scrambling_control = packet.transport_scrambling_control;
        match growth {
            // Why not copy a PES packet that lies inside one packet: it can
            // be lent from the packet, and is copied only to cross packets.
            Growth::Ends { length } => pending.push_back(Pending::PesPacket {
                pid,
                bytes: Lent::Packet(payload_start..payload_start.saturating_add(length)),
                start_offset: offset,
            }),
            Growth::Gathers => self.buffer.extend_from_slice(payload),
            Growth::Overruns => self.overrun(pid, payload, pending),
        }
    }

    fn is_scrambled(&self) -> bool {
        self.transport_scrambling_control != TransportScramblingControl::NOT_SCRAMBLED
    }

    /// Ends the PES packet in flight, if any, and queues it to be read, or
    /// as scrambled
    fn close_ended(&mut self, pid: Pid, pending: &mut VecDeque<Pending>) {
        let Some(bytes) = self.close() else {
            return;
        };
        pending.push_back(if self.is_scrambled() {
            Pending::ScrambledPesPacket {
                pid,
                bytes,
                transport_scrambling_control: self.transport_scrambling_control,
                start_offset: self.start_offset,
            }
        } else {
            Pending::PesPacket {
                pid,
                bytes: Lent::Reassembled(bytes),
                start_offset: self.start_offset,
            }
        });
    }

    /// Drops the PES packet in flight, which `payload` would take past the
    /// limit, and queues the overrun
    fn overrun(&mut self, pid: Pid, payload: &[u8], pending: &mut VecDeque<Pending>) {
        let discarded_bytes = self.in_flight().len().saturating_add(payload.len());
        self.buffer.truncate(self.in_flight_start);
        pending.push_back(Pending::Ready(Event::Error(Error::PesBufferOverrun {
            pid,
            discarded_bytes,
        })));
    }

    /// Ends the PES packet in flight, if any, and queues it as cut short
    fn close_truncated(&mut self, pid: Pid, pending: &mut VecDeque<Pending>) {
        if let Some(bytes) = self.close() {
            pending.push_back(Pending::TruncatedPesPacket {
                pid,
                bytes,
                start_offset: self.start_offset,
            });
        }
    }
}

/// Classifies a packet of a subscribed PID against the PES packet in flight
fn observe(
    reassembler: &PesReassembler,
    sequence: Sequence,
    packet: &TransportPacket<'_>,
    buffer_limit: usize,
) -> Observation {
    let payload = packet.payload.unwrap_or_default();
    let start = || {
        growth(
            &[],
            packet.transport_scrambling_control != TransportScramblingControl::NOT_SCRAMBLED,
            payload,
            buffer_limit,
        )
    };
    match (sequence, packet.payload_unit_start_indicator) {
        (Sequence::Duplicate, _) => Observation::Repeated,
        (Sequence::Errored, _) | (Sequence::Gap, false) => Observation::Interrupted,
        (Sequence::InOrder, true) => Observation::Starts(start()),
        (Sequence::Gap, true) => Observation::StartsAfterGap(start()),
        (Sequence::InOrder, false) if reassembler.in_flight().is_empty() => Observation::Stray,
        (Sequence::InOrder, false) => Observation::Continues(growth(
            reassembler.in_flight(),
            reassembler.is_scrambled(),
            payload,
            buffer_limit,
        )),
    }
}

/// Classifies `payload` against `in_flight`, the PES packet it belongs to,
/// whose length is read unless it is `scrambled`
fn growth(in_flight: &[u8], scrambled: bool, payload: &[u8], buffer_limit: usize) -> Growth {
    // Why copy the prefix only while the packet in flight is shorter than
    // it: once its first bytes are gathered, they alone give the length.
    let mut joined = [0; pes::PREFIX_LENGTH];
    let prefix = if in_flight.len() >= pes::PREFIX_LENGTH {
        in_flight
    } else {
        let copied = joined
            .iter_mut()
            .zip(in_flight.iter().chain(payload))
            .map(|(slot, byte)| *slot = *byte)
            .count();
        joined.get(..copied).unwrap_or_default()
    };
    let gathered = in_flight.len().saturating_add(payload.len());
    match length_of(prefix, scrambled) {
        // Why end before checking the limit: a PES packet whose declared
        // length is all in hand is whole, and dropping it as an overrun would
        // lose it for nothing.
        PesPacketLength::Bounded(length) if length <= gathered => Growth::Ends {
            length: length.saturating_sub(in_flight.len()),
        },
        PesPacketLength::Bounded(_)
        | PesPacketLength::Unbounded
        | PesPacketLength::NeedMoreData
            if gathered > buffer_limit =>
        {
            Growth::Overruns
        }
        PesPacketLength::Bounded(_)
        | PesPacketLength::Unbounded
        | PesPacketLength::NeedMoreData => Growth::Gathers,
    }
}

/// Returns how long the PES packet that `prefix` starts is, unread when
/// its Transport Packets are `scrambled`
fn length_of(prefix: &[u8], scrambled: bool) -> PesPacketLength {
    // Why not read the length of a packet in scrambled Transport Packets:
    // PES_packet_length is scrambled with the rest, so the packet runs to
    // the next payload_unit_start_indicator, bounded by the limit.
    if scrambled {
        PesPacketLength::Unbounded
    } else {
        PesPacket::encoded_len_of(prefix)
    }
}

#[cfg(test)]
mod tests;
