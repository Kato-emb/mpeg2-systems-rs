//! Reassembly of the PES packets a subscribed PID carries across Transport
//! Packets

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::ops::Range;

use super::continuity::Verdict;
use crate::pes::{self, PesPacketLength, pes_packet_length};
use crate::ts::Pid;
use crate::ts::demux::{Error, Event, Lent, Pending};
use crate::ts::packet::{PACKET_LENGTH, TransportPacket};

/// What a packet's payload brings to the PES packet in flight on its PID
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Observation {
    /// No PES packet is in flight, and the payload continues one: nothing in
    /// it says where a PES packet starts
    Stray,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct PesReassembly {
    buffer_limit: usize,
    reassemblers: BTreeMap<Pid, PesReassembler>,
}

/// The PES packets of one subscribed PID
///
/// `buffer[..in_flight_start]` holds the PES packets the latest packet of
/// the PID ended, which stay lent until the next packet of the PID;
/// `buffer[in_flight_start..]` is the PES packet in flight.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PesReassembler {
    buffer: Vec<u8>,
    in_flight_start: usize,
    /// Input offset of the unit of the Transport Packet the PES packet in
    /// flight started in
    start_offset: u64,
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
            });
    }

    /// Drops the PID's subscription and every byte it gathered
    pub(super) fn unsubscribe(&mut self, pid: Pid) {
        // Why no event for the PES packet in flight: the caller that left
        // the PID has no use for it, and reporting it would oblige the caller
        // to keep filtering events for a PID it has just left.
        self.reassemblers.remove(&pid);
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
        // in flight, not the caller's interest in the PID.
        for reassembler in self.reassemblers.values_mut() {
            reassembler.buffer.truncate(reassembler.in_flight_start);
        }
    }

    /// Ends the PES packet in flight on every PID at the end of input, and
    /// queues it
    pub(super) fn finish(&mut self, pending: &mut VecDeque<Pending>) {
        for (&pid, reassembler) in &mut self.reassemblers {
            let length = pes_packet_length(reassembler.in_flight());
            let Some(bytes) = reassembler.close() else {
                continue;
            };
            pending.push_back(match length {
                PesPacketLength::Unbounded => Pending::PesPacket {
                    pid,
                    bytes: Lent::Reassembled(bytes),
                    start_offset: reassembler.start_offset,
                },
                // Why not read a packet of a declared length: it ends the
                // moment its bytes are in, so one still in flight is short of
                // its length.
                PesPacketLength::Bounded(_) | PesPacketLength::NeedMoreData => {
                    Pending::TruncatedPesPacket { pid, bytes }
                }
            });
        }
    }

    /// Takes a packet, which `verdict` placed in its PID's sequence and
    /// whose payload starts at `payload_start` in the framer's buffer and
    /// whose unit starts at input `offset`, and queues the PES packets it
    /// ends, if its PID is subscribed
    pub(super) fn push(
        &mut self,
        packet: &TransportPacket<'_>,
        verdict: Verdict,
        payload_start: usize,
        offset: u64,
        pending: &mut VecDeque<Pending>,
    ) {
        let pid = packet.pid;
        let Some(reassembler) = self.reassemblers.get_mut(&pid) else {
            return;
        };
        match verdict {
            Verdict::InOrder => reassembler.lend_no_more(),
            // Why not reassemble a duplicate: its payload repeats bytes
            // already gathered.
            Verdict::Duplicate => return,
            Verdict::Broken { .. } | Verdict::Excused => {
                reassembler.lend_no_more();
                if let Some(bytes) = reassembler.close() {
                    pending.push_back(Pending::TruncatedPesPacket { pid, bytes });
                }
            }
        }
        let payload = packet.payload.unwrap_or_default();
        if packet.payload_unit_start_indicator {
            if let Some(bytes) = reassembler.close() {
                pending.push_back(Pending::PesPacket {
                    pid,
                    bytes: Lent::Reassembled(bytes),
                    start_offset: reassembler.start_offset,
                });
            }
            reassembler.start_offset = offset;
        }
        let in_flight = reassembler.in_flight();
        match observe(
            in_flight,
            packet.payload_unit_start_indicator,
            payload,
            self.buffer_limit,
        ) {
            Observation::Stray => {}
            // Why not copy a PES packet that lies inside one packet: it can
            // be lent from the packet, and is copied only to cross packets.
            Observation::Ends { length } if in_flight.is_empty() => {
                pending.push_back(Pending::PesPacket {
                    pid,
                    bytes: Lent::Packet(payload_start..payload_start.saturating_add(length)),
                    start_offset: offset,
                });
            }
            // Why drop the bytes after the end: a PES packet starts only at
            // a payload_unit_start_indicator, so the bytes after one that
            // ends inside a Transport Packet are stuffing.
            Observation::Ends { length } => {
                reassembler
                    .buffer
                    .extend_from_slice(payload.get(..length).unwrap_or_default());
                if let Some(bytes) = reassembler.close() {
                    pending.push_back(Pending::PesPacket {
                        pid,
                        bytes: Lent::Reassembled(bytes),
                        start_offset: reassembler.start_offset,
                    });
                }
            }
            Observation::Gathers => reassembler.buffer.extend_from_slice(payload),
            Observation::Overruns => {
                let discarded_bytes = in_flight.len().saturating_add(payload.len());
                reassembler.buffer.truncate(reassembler.in_flight_start);
                pending.push_back(Pending::Ready(Event::Error(Error::PesBufferOverrun {
                    pid,
                    discarded_bytes,
                })));
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
}

/// Classifies `payload` against `in_flight`, the PES packet in flight once a
/// `payload_unit_start_indicator` has ended the previous one
fn observe(
    in_flight: &[u8],
    payload_unit_start_indicator: bool,
    payload: &[u8],
    buffer_limit: usize,
) -> Observation {
    if !payload_unit_start_indicator && in_flight.is_empty() {
        return Observation::Stray;
    }
    let mut prefix = [0; pes::PREFIX_LENGTH];
    let prefix_length = prefix
        .iter_mut()
        .zip(in_flight.iter().chain(payload))
        .map(|(slot, byte)| *slot = *byte)
        .count();
    let gathered = in_flight.len().saturating_add(payload.len());
    match pes_packet_length(prefix.get(..prefix_length).unwrap_or_default()) {
        // Why end before checking the limit: a PES packet whose declared
        // length is all in hand is whole, and dropping it as an overrun would
        // lose it for nothing.
        PesPacketLength::Bounded(length) if length <= gathered => Observation::Ends {
            length: length.saturating_sub(in_flight.len()),
        },
        PesPacketLength::Bounded(_)
        | PesPacketLength::Unbounded
        | PesPacketLength::NeedMoreData => {
            if gathered > buffer_limit {
                Observation::Overruns
            } else {
                Observation::Gathers
            }
        }
    }
}

#[cfg(test)]
mod tests;
