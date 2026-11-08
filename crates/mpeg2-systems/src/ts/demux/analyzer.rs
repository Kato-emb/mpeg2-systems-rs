//! Analysis of each Transport Packet the framer cuts
//!
//! The events of one packet are queued in a fixed order: a packet that cannot
//! be read, or one with `transport_error_indicator` set, gives one error and
//! nothing else; any other gives a discontinuity, then its PCR, then the
//! sections it completes, each read or failed.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::ops::Range;

use super::{Error, Event, Pending};
use crate::psi::pat::{Program, ProgramAssociationSection};
use crate::psi::section::Section;
use crate::ts::Pid;
use crate::ts::packet::TransportPacket;

mod continuity;
mod section_reassembly;

use continuity::{ContinuityTracker, Verdict};
use section_reassembly::SectionReassembler;

/// Continuity and section reassembly state of the PIDs a demuxer follows
///
/// Sections are reassembled on the PAT and CAT PIDs, and on every PID a PAT
/// has named for a program map.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct PacketAnalyzer {
    continuity: ContinuityTracker,
    reassemblers: BTreeMap<Pid, SectionReassembler>,
    /// The bodies of the sections queued since [`PacketAnalyzer::release`],
    /// which the queued events lend
    bodies: Vec<u8>,
}

impl PacketAnalyzer {
    pub(super) const fn new() -> Self {
        Self {
            continuity: ContinuityTracker::new(),
            reassemblers: BTreeMap::new(),
            bodies: Vec::new(),
        }
    }

    /// Drops the continuity history and every section in flight; the PIDs
    /// learned from a PAT stay
    pub(super) fn reset(&mut self) {
        self.continuity.reset();
        for reassembler in self.reassemblers.values_mut() {
            reassembler.discard_in_flight();
        }
    }

    /// Drops the section bodies the queued events lent, once none is queued
    pub(super) fn release(&mut self) {
        self.bodies.clear();
    }

    /// Returns the body of a section [`PacketAnalyzer::analyze`] queued
    pub(super) fn body(&self, range: Range<usize>) -> &[u8] {
        self.bodies.get(range).unwrap_or_default()
    }

    /// Analyzes one packet and queues its events
    pub(super) fn analyze(&mut self, bytes: &[u8], pending: &mut VecDeque<Pending>) {
        let packet = match TransportPacket::read(bytes) {
            Ok(packet) => packet,
            Err(error) => {
                pending.push_back(Pending::Ready(Event::Error(Error::Packet { error })));
                return;
            }
        };
        let pid = packet.pid;
        // Why not report the PCR of such a packet: transport_error_indicator
        // marks its contents as unreliable, and a wrong clock value would
        // mislead clock recovery downstream.
        if packet.transport_error_indicator {
            pending.push_back(Pending::Ready(Event::Error(
                Error::TransportErrorIndicator { pid },
            )));
            return;
        }
        let verdict = self.continuity.check(&packet);
        if let Verdict::Broken { expected, found } = verdict {
            pending.push_back(Pending::Ready(Event::Error(Error::Discontinuity {
                pid,
                expected,
                found,
            })));
        }
        // Why report the PCR of a duplicate too: §2.4.3.3 lets a duplicate
        // differ from the original only in its PCR, which can be a valid,
        // restamped value.
        if let Some(program_clock_reference) = packet
            .adaptation_field
            .as_ref()
            .and_then(|field| field.program_clock_reference)
        {
            pending.push_back(Pending::Ready(Event::ProgramClockReference {
                pid,
                program_clock_reference,
            }));
        }
        let reassembler = if pid == Pid::PAT || pid == Pid::CAT {
            Some(
                self.reassemblers
                    .entry(pid)
                    .or_insert_with(SectionReassembler::new),
            )
        } else {
            self.reassemblers.get_mut(&pid)
        };
        let (Some(reassembler), Some(payload)) = (reassembler, packet.payload) else {
            return;
        };
        match verdict {
            Verdict::InOrder => {}
            // Why not reassemble a duplicate: its payload repeats bytes already
            // gathered, so the same section would complete twice.
            Verdict::Duplicate => return,
            // Why drop the section in flight: after lost bytes or a splice, the
            // bytes gathered and those that follow belong to different
            // sections.
            Verdict::Broken { .. } | Verdict::Excused => reassembler.discard_in_flight(),
        }
        let first_queued = pending.len();
        let bodies = &mut self.bodies;
        reassembler.push(packet.payload_unit_start_indicator, payload, &mut |bytes| {
            pending.push_back(match Section::read(bytes) {
                Ok(section) => {
                    let start = bodies.len();
                    bodies.extend_from_slice(section.body);
                    Pending::Section {
                        pid,
                        section: Section {
                            table_id: section.table_id,
                            private_indicator: section.private_indicator,
                            reserved_before_section_length: section.reserved_before_section_length,
                            syntax: section.syntax,
                            body: &[],
                        },
                        body: start..bodies.len(),
                    }
                }
                Err(error) => Pending::Ready(Event::Error(Error::Section { pid, error })),
            });
        });
        if pid != Pid::PAT {
            return;
        }
        for queued in pending.range(first_queued..) {
            let Pending::Section { section, body, .. } = queued else {
                continue;
            };
            let section = Section {
                body: self.bodies.get(body.clone()).unwrap_or_default(),
                ..section.clone()
            };
            let Ok(table) = ProgramAssociationSection::try_from(&section) else {
                continue;
            };
            for program in table.programs {
                // Why skip the null PID: a broken PAT can name it, and its
                // packets carry stuffing, not sections.
                if let Ok(Program::ProgramMap {
                    program_map_pid, ..
                }) = program
                    && program_map_pid != Pid::NULL
                {
                    self.reassemblers
                        .entry(program_map_pid)
                        .or_insert_with(SectionReassembler::new);
                }
            }
        }
    }
}
