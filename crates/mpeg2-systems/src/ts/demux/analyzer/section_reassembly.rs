//! Reassembly of the sections one PID carries across Transport Packets

use alloc::vec::Vec;

use crate::psi::TableId;
use crate::psi::section::PREFIX_LENGTH;

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

/// The section in flight on one PID, gathered across Transport Packets
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct SectionReassembler {
    in_flight: Vec<u8>,
}

impl SectionReassembler {
    pub(super) const fn new() -> Self {
        Self {
            in_flight: Vec::new(),
        }
    }

    /// Drops the section in flight
    pub(super) fn discard_in_flight(&mut self) {
        self.in_flight.clear();
    }

    /// Takes the payload of a packet of the PID, and calls `completed` with
    /// each section it completes, in order
    pub(super) fn push(
        &mut self,
        payload_unit_start_indicator: bool,
        payload: &[u8],
        completed: &mut impl FnMut(&[u8]),
    ) {
        match unit_of(payload_unit_start_indicator, payload) {
            Unit::Continuation(bytes) => {
                // Why not start a section from a continuation when none is in
                // flight: nothing in it says where a section starts.
                if !self.in_flight.is_empty() {
                    self.consume(bytes, completed);
                }
            }
            Unit::Start { tail, sections } => {
                // Why close with one observation and drop the rest:
                // pointer_field counts exactly the bytes that end the section
                // in flight, so bytes past its end, or too few to end it, mean
                // the framing and the bytes gathered disagree.
                if !self.in_flight.is_empty()
                    && let Observation::Complete { end, .. } = observe(&self.in_flight, tail)
                {
                    self.complete(end, completed);
                }
                self.in_flight.clear();
                self.consume(sections, completed);
            }
            Unit::BrokenStart => self.in_flight.clear(),
        }
    }

    fn consume(&mut self, mut remaining: &[u8], completed: &mut impl FnMut(&[u8])) {
        loop {
            match observe(&self.in_flight, remaining) {
                Observation::Exhausted | Observation::Stuffing => return,
                Observation::Oversized => {
                    self.in_flight.clear();
                    return;
                }
                Observation::Fragment => {
                    self.in_flight.extend_from_slice(remaining);
                    return;
                }
                Observation::Complete { end, rest } => {
                    self.complete(end, completed);
                    remaining = rest;
                }
            }
        }
    }

    /// Appends the bytes that end the section in flight, and hands it over
    fn complete(&mut self, end: &[u8], completed: &mut impl FnMut(&[u8])) {
        self.in_flight.extend_from_slice(end);
        completed(&self.in_flight);
        self.in_flight.clear();
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
