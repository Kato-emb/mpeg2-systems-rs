//! Reassembly of the sections the PIDs followed carry across Transport
//! Packets, and the subscriptions to them

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::ops::Range;

use crate::psi::TableId;
use crate::psi::section::PREFIX_LENGTH;
use crate::ts::Pid;
use crate::ts::demux::{Error, Event, Pending};

/// How a packet's payload relates to sections, `pointer_field` included
enum Unit<'payload> {
    /// `payload_unit_start_indicator` is `'0'`: the bytes continue a section
    Continuation(&'payload [u8]),
    /// `payload_unit_start_indicator` is `'1'`
    Start {
        /// The bytes `pointer_field` counts, which end the section in flight
        tail: &'payload [u8],
        /// The bytes after them, where sections start
        sections: &'payload [u8],
    },
    /// `payload_unit_start_indicator` is `'1'`, but `pointer_field` is
    /// missing or points past the payload
    BrokenStart,
}

/// What the reassembler saw at the front of the payload bytes left
enum Observation<'payload> {
    /// No payload bytes are left
    Exhausted,
    /// No section is in flight, and the bytes left are stuffing
    Stuffing,
    /// The section's `section_length` is larger than its table allows
    Oversized,
    /// All the bytes left belong to the section in flight, which they do not
    /// complete
    Fragment,
    /// The bytes that complete the section in flight, and those after them
    Complete {
        end: &'payload [u8],
        rest: &'payload [u8],
    },
}

/// Where a completed section starts
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Completed {
    /// At this offset of the payload pushed: the section lay inside one
    /// packet
    InPayload(usize),
    /// At this offset of the reassembler's buffer: the section crossed
    /// packets
    Reassembled(usize),
}

/// The PIDs whose sections are reassembled, for the PSI or because the
/// caller subscribed to them, and the section in flight on each
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SectionReassembly {
    pids: BTreeMap<Pid, SectionPid>,
}

/// A PID whose sections are reassembled
#[derive(Debug, Clone, PartialEq, Eq)]
struct SectionPid {
    reassembler: SectionReassembler,
    /// Whether [`Demuxer::subscribe_sections`](crate::ts::demux::Demuxer::subscribe_sections)
    /// names the PID
    // Why not a set of the PIDs subscribed to beside the map: each of them
    // has an entry here, and the two would have to be kept in step wherever
    // an entry is added or dropped.
    subscribed: bool,
}

impl SectionReassembly {
    pub(super) const fn new() -> Self {
        Self {
            pids: BTreeMap::new(),
        }
    }

    /// Reassembles the sections of `pid` from its next
    /// `payload_unit_start_indicator` for the caller, keeping the section in
    /// flight of a PID already reassembled
    pub(super) fn subscribe(&mut self, pid: Pid) {
        self.entry(pid).subscribed = true;
    }

    /// Drops the caller's subscription to `pid`, if any; unless `carries_psi`,
    /// drops with it every byte the PID gathered and the queued events of
    /// its sections
    pub(super) fn unsubscribe(
        &mut self,
        pid: Pid,
        carries_psi: bool,
        pending: &mut VecDeque<Pending>,
    ) {
        let Some(section_pid) = self
            .pids
            .get_mut(&pid)
            .filter(|section_pid| section_pid.subscribed)
        else {
            return;
        };
        section_pid.subscribed = false;
        if carries_psi {
            return;
        }
        // Why drop the queued events too: a section that crossed packets
        // lends the buffer dropped here.
        self.pids.remove(&pid);
        pending.retain(|pending| match pending {
            Pending::Section { pid: queued, .. }
            | Pending::Ready(Event::Error(Error::Section { pid: queued, .. })) => *queued != pid,
            Pending::Ready(_)
            | Pending::Unsynchronized(_)
            | Pending::PesPacket { .. }
            | Pending::ScrambledPesPacket { .. }
            | Pending::TruncatedPesPacket { .. } => true,
        });
    }

    /// Returns the reassembler of `pid`, which carries the PSI, reassembling
    /// its sections from here on if they are not already
    pub(super) fn for_psi(&mut self, pid: Pid) -> &mut SectionReassembler {
        &mut self.entry(pid).reassembler
    }

    /// Stops reassembling the sections of each PID not subscribed to that
    /// `carries_psi` refuses
    pub(super) fn retain_psi(&mut self, mut carries_psi: impl FnMut(Pid) -> bool) {
        self.pids
            .retain(|&pid, section_pid| section_pid.subscribed || carries_psi(pid));
    }

    /// Returns the entry of `pid`, made with no section and no subscription
    /// if there is none
    fn entry(&mut self, pid: Pid) -> &mut SectionPid {
        self.pids.entry(pid).or_insert_with(|| SectionPid {
            reassembler: SectionReassembler::new(),
            subscribed: false,
        })
    }

    /// Returns the reassembler of `pid`, if its sections are reassembled
    pub(super) fn reassembler_mut(&mut self, pid: Pid) -> Option<&mut SectionReassembler> {
        self.pids
            .get_mut(&pid)
            .map(|section_pid| &mut section_pid.reassembler)
    }

    /// Returns the bytes of a [`Completed::Reassembled`] range of `pid`
    pub(super) fn bytes(&self, pid: Pid, range: Range<usize>) -> &[u8] {
        self.pids
            .get(&pid)
            .map(|section_pid| section_pid.reassembler.section(range))
            .unwrap_or_default()
    }

    /// Drops the section in flight on every PID, keeping the sections
    /// completed, which the queued events lend, the PIDs reassembled and the
    /// subscriptions
    pub(super) fn discard_in_flight(&mut self) {
        for section_pid in self.pids.values_mut() {
            section_pid.reassembler.discard_in_flight();
        }
    }

    /// The PIDs whose sections are reassembled
    #[cfg(test)]
    pub(super) fn pids(&self) -> impl Iterator<Item = Pid> + '_ {
        self.pids.keys().copied()
    }

    /// Bytes the reassembler of `pid` holds
    #[cfg(test)]
    pub(super) fn buffer_len(&self, pid: Pid) -> Option<usize> {
        self.pids
            .get(&pid)
            .map(|section_pid| section_pid.reassembler.buffer_len())
    }
}

/// The sections of one PID that cross Transport Packets
///
/// `buffer[..in_flight_start]` holds the sections the latest packet
/// completed, which stay lent until the next packet of the PID;
/// `buffer[in_flight_start..]` is the section in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SectionReassembler {
    buffer: Vec<u8>,
    in_flight_start: usize,
}

impl SectionReassembler {
    pub(super) const fn new() -> Self {
        Self {
            buffer: Vec::new(),
            in_flight_start: 0,
        }
    }

    /// Returns the bytes of a [`Completed::Reassembled`] section
    pub(super) fn section(&self, range: Range<usize>) -> &[u8] {
        self.buffer.get(range).unwrap_or_default()
    }

    #[cfg(test)]
    pub(super) fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// Drops the section in flight
    pub(super) fn discard_in_flight(&mut self) {
        self.buffer.truncate(self.in_flight_start);
    }

    /// Takes the payload of a packet of the PID, and calls `completed` with
    /// each section it completes, in order, and where it lies
    ///
    /// The sections the previous push completed are dropped first.
    pub(super) fn push(
        &mut self,
        payload_unit_start_indicator: bool,
        payload: &[u8],
        completed: &mut impl FnMut(Completed, &[u8]),
    ) {
        self.buffer.drain(..self.in_flight_start);
        self.in_flight_start = 0;
        match unit_of(payload_unit_start_indicator, payload) {
            Unit::Continuation(bytes) => {
                // Why not start a section from a continuation when none is in
                // flight: nothing in it says where a section starts.
                if !self.in_flight().is_empty() {
                    self.consume(payload.len(), bytes, completed);
                }
            }
            Unit::Start { tail, sections } => {
                // Why close with one observation and drop the rest:
                // pointer_field counts exactly the bytes that end the section
                // in flight, so bytes past its end, or too few to end it, mean
                // the framing and the bytes gathered disagree.
                if !self.in_flight().is_empty()
                    && let Observation::Complete { end, .. } = observe(self.in_flight(), tail)
                {
                    self.complete(end, completed);
                }
                self.discard_in_flight();
                self.consume(payload.len(), sections, completed);
            }
            Unit::BrokenStart => self.discard_in_flight(),
        }
    }

    fn in_flight(&self) -> &[u8] {
        self.buffer.get(self.in_flight_start..).unwrap_or_default()
    }

    /// Consumes `remaining`, the last bytes of a payload of `payload_length`
    fn consume(
        &mut self,
        payload_length: usize,
        mut remaining: &[u8],
        completed: &mut impl FnMut(Completed, &[u8]),
    ) {
        loop {
            match observe(self.in_flight(), remaining) {
                Observation::Exhausted | Observation::Stuffing => return,
                Observation::Oversized => {
                    self.discard_in_flight();
                    return;
                }
                Observation::Fragment => {
                    self.buffer.extend_from_slice(remaining);
                    return;
                }
                // Why not copy a section that lies inside one packet: it can be
                // lent from the packet, and is copied only to cross packets.
                Observation::Complete { end, rest } if self.in_flight().is_empty() => {
                    completed(
                        Completed::InPayload(payload_length.saturating_sub(remaining.len())),
                        end,
                    );
                    remaining = rest;
                }
                Observation::Complete { end, rest } => {
                    self.complete(end, completed);
                    remaining = rest;
                }
            }
        }
    }

    /// Appends the bytes that end the section in flight, and lends it from
    /// the buffer
    fn complete(&mut self, end: &[u8], completed: &mut impl FnMut(Completed, &[u8])) {
        self.buffer.extend_from_slice(end);
        let start = self.in_flight_start;
        self.in_flight_start = self.buffer.len();
        completed(
            Completed::Reassembled(start),
            self.section(start..self.in_flight_start),
        );
    }
}

/// Splits a payload by its `pointer_field`
fn unit_of(payload_unit_start_indicator: bool, payload: &[u8]) -> Unit<'_> {
    if !payload_unit_start_indicator {
        return Unit::Continuation(payload);
    }
    payload
        .split_first()
        .and_then(|(&pointer_field, after)| after.split_at_checked(usize::from(pointer_field)))
        .map_or(Unit::BrokenStart, |(tail, sections)| Unit::Start {
            tail,
            sections,
        })
}

/// Classifies the front of `remaining` against the section in flight
fn observe<'payload>(in_flight: &[u8], remaining: &'payload [u8]) -> Observation<'payload> {
    let Some(&first) = remaining.first() else {
        return Observation::Exhausted;
    };
    if in_flight.is_empty() && TableId::from(first) == TableId::FORBIDDEN {
        return Observation::Stuffing;
    }
    let mut prefix = in_flight.iter().chain(remaining).copied();
    let (Some(table_id), Some(high), Some(low)) = (prefix.next(), prefix.next(), prefix.next())
    else {
        return Observation::Fragment;
    };
    let section_length = u16::from_be_bytes([high & 0x0F, low]);
    // Why bound by the table, not by a setting: the syntax caps every
    // section_length (§2.4.4.5, §2.4.4.11), so a larger one is broken
    // framing, and the buffer never grows past the largest section.
    if section_length > TableId::from(table_id).max_section_length() {
        return Observation::Oversized;
    }
    let needed = PREFIX_LENGTH
        .saturating_add(usize::from(section_length))
        .saturating_sub(in_flight.len());
    remaining
        .split_at_checked(needed)
        .map_or(Observation::Fragment, |(end, rest)| Observation::Complete {
            end,
            rest,
        })
}

#[cfg(test)]
mod tests;
