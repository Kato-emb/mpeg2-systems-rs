//! Analysis of each Transport Packet the framer cuts
//!
//! The events of one packet are queued in a fixed order: a packet that cannot
//! be read, or one with `transport_error_indicator` set, gives one error and
//! nothing else; any other gives a discontinuity, then its PCR, then the
//! sections it completes, each read or failed.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use core::ops::Range;

use super::{Error, Event, Lent, Pending};
use crate::psi::pat::{Program, ProgramAssociationSection};
use crate::psi::section::{PREFIX_LENGTH, SYNTAX_LENGTH, Section};
use crate::ts::Pid;
use crate::ts::packet::TransportPacket;

mod continuity;
mod section_reassembly;

use continuity::{ContinuityTracker, Verdict};
use section_reassembly::{Completed, SectionReassembler};

/// Continuity and section reassembly state of the PIDs a demuxer follows
///
/// Sections are reassembled on the PAT and CAT PIDs, and on every PID a PAT
/// has named for a program map.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct PacketAnalyzer {
    continuity: ContinuityTracker,
    reassemblers: BTreeMap<Pid, SectionReassembler>,
    /// The PIDs PAT sections have named for a program map
    // Why not read these from the map's keys: the map lends the PAT's
    // reassembler while the PAT's section names them.
    program_map_pids: BTreeSet<Pid>,
    /// The PID whose reassembler holds sections completed since the last
    /// [`PacketAnalyzer::release`], read or not
    // Why not release every reassembler instead: that walks them all on
    // every packet, while at most one holds completed sections at a time.
    completed_on: Option<Pid>,
}

impl PacketAnalyzer {
    pub(super) const fn new() -> Self {
        Self {
            continuity: ContinuityTracker::new(),
            reassemblers: BTreeMap::new(),
            program_map_pids: BTreeSet::new(),
            completed_on: None,
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

    /// Drops the sections the queued events lent, once none is queued
    pub(super) fn release(&mut self) {
        if let Some(reassembler) = self
            .completed_on
            .take()
            .and_then(|pid| self.reassemblers.get_mut(&pid))
        {
            reassembler.release();
        }
    }

    /// Returns the bytes of a [`Lent::Reassembled`] range of `pid`
    pub(super) fn reassembled(&self, pid: Pid, range: Range<usize>) -> &[u8] {
        self.reassemblers
            .get(&pid)
            .map(|reassembler| reassembler.section(range))
            .unwrap_or_default()
    }

    /// Analyzes one packet, which starts at `packet_start` in the framer's
    /// buffer, and queues its events
    pub(super) fn analyze(
        &mut self,
        bytes: &[u8],
        packet_start: usize,
        pending: &mut VecDeque<Pending>,
    ) {
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
        // Why gather the PIDs a PAT names in the set, and only then add their
        // reassemblers: the map lends the PAT's reassembler while the section
        // is read, so it cannot take a new entry until then.
        let mut program_map_pids = (pid == Pid::PAT).then_some(&mut self.program_map_pids);
        let mut named_new_pid = false;
        let Some(reassembler) = reassembler else {
            return;
        };
        match verdict {
            Verdict::InOrder => {}
            // Why not reassemble a duplicate: its payload repeats bytes already
            // gathered, so the same section would complete twice.
            Verdict::Duplicate => return,
            // Why drop the section in flight, on a packet without payload too:
            // after lost bytes or a splice, the bytes gathered and those that
            // follow belong to different sections.
            Verdict::Broken { .. } | Verdict::Excused => reassembler.discard_in_flight(),
        }
        let Some(payload) = packet.payload else {
            return;
        };
        let payload_start = packet_start.saturating_add(bytes.len().saturating_sub(payload.len()));
        let completed_on = &mut self.completed_on;
        reassembler.push(
            packet.payload_unit_start_indicator,
            payload,
            &mut |completed, section_bytes| {
                // Why before the section is read: a section that fails to read
                // still fills the reassembler, which must be drained all the
                // same.
                if let Completed::Reassembled(_) = completed {
                    *completed_on = Some(pid);
                }
                let section = match Section::read(section_bytes) {
                    Ok(section) => section,
                    Err(error) => {
                        pending
                            .push_back(Pending::Ready(Event::Error(Error::Section { pid, error })));
                        return;
                    }
                };
                if let Some(program_map_pids) = program_map_pids.as_deref_mut()
                    && let Ok(table) = ProgramAssociationSection::try_from(&section)
                {
                    for program in table.programs {
                        // Why skip the PAT, CAT and null PIDs: a broken PAT can
                        // name them, and the PAT and CAT are reassembled
                        // anyway while null packets carry stuffing, not
                        // sections.
                        if let Ok(Program::ProgramMap {
                            program_map_pid, ..
                        }) = program
                            && ![Pid::PAT, Pid::CAT, Pid::NULL].contains(&program_map_pid)
                        {
                            named_new_pid |= program_map_pids.insert(program_map_pid);
                        }
                    }
                }
                let header_length = if section.syntax.is_some() {
                    PREFIX_LENGTH.saturating_add(SYNTAX_LENGTH)
                } else {
                    PREFIX_LENGTH
                };
                let body = |section_start: usize| {
                    let start = section_start.saturating_add(header_length);
                    start..start.saturating_add(section.body.len())
                };
                pending.push_back(Pending::Section {
                    pid,
                    body: match completed {
                        Completed::InPayload(start) => {
                            Lent::Packet(body(payload_start.saturating_add(start)))
                        }
                        Completed::Reassembled(start) => Lent::Reassembled(body(start)),
                    },
                    section: Section {
                        table_id: section.table_id,
                        private_indicator: section.private_indicator,
                        reserved_before_section_length: section.reserved_before_section_length,
                        syntax: section.syntax,
                        body: &[],
                    },
                });
            },
        );
        if named_new_pid {
            for &program_map_pid in &self.program_map_pids {
                self.reassemblers
                    .entry(program_map_pid)
                    .or_insert_with(SectionReassembler::new);
            }
        }
    }
}

#[cfg(test)]
mod tests;
