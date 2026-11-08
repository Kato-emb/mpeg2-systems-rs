//! Analysis of each Transport Packet the framer cuts
//!
//! The events of one packet are queued in a fixed order: a packet that cannot
//! be read, or one with `transport_error_indicator` set, gives one error and
//! nothing else; any other gives a discontinuity, then its PCR, then the
//! sections it completes, each read or failed. A PAT, PMT or CAT section is
//! queued only at a version not reported before.

use alloc::collections::{BTreeMap, VecDeque};
use core::mem;
use core::ops::Range;

use super::{Error, Event, Lent, Pending};
use crate::psi::TableId;
use crate::psi::cat::CaSection;
use crate::psi::pat::ProgramAssociationSection;
use crate::psi::pmt::TsProgramMapSection;
use crate::psi::section::{PREFIX_LENGTH, SYNTAX_LENGTH, Section};
use crate::ts::Pid;
use crate::ts::packet::TransportPacket;

mod continuity;
mod program_map_routes;
mod section_reassembly;
mod version_gate;

use continuity::{ContinuityTracker, Verdict};
use program_map_routes::ProgramMapRoutes;
use section_reassembly::{Completed, SectionReassembler};
use version_gate::{Admission, TableSection, VersionGate};

/// Continuity, section reassembly and table version state of the PIDs a
/// demuxer follows
///
/// Sections are reassembled on the PAT and CAT PIDs, and on every PID the
/// PAT names for a program map.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct PacketAnalyzer {
    continuity: ContinuityTracker,
    reassemblers: BTreeMap<Pid, SectionReassembler>,
    routes: ProgramMapRoutes,
    gate: VersionGate,
}

impl PacketAnalyzer {
    pub(super) const fn new() -> Self {
        Self {
            continuity: ContinuityTracker::new(),
            reassemblers: BTreeMap::new(),
            routes: ProgramMapRoutes::new(),
            gate: VersionGate::new(),
        }
    }

    /// Drops the continuity history, every section and the PAT sections
    /// gathered, once no event is queued; the PIDs learned from a PAT and the
    /// versions reported stay
    pub(super) fn reset(&mut self) {
        self.continuity.reset();
        for reassembler in self.reassemblers.values_mut() {
            reassembler.clear();
        }
        // Why not forget the versions reported: after a resynchronization or
        // across an end of input, the tables on air are most likely the same,
        // and reporting them again would read as a change.
        self.routes.reset();
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
        let payload_start = packet.payload.map_or(packet_start, |payload| {
            packet_start.saturating_add(bytes.len().saturating_sub(payload.len()))
        });
        match pid {
            Pid::PAT => {
                // Why not keep the PAT's reassembler in the map while it
                // pushes: the program maps a PAT names are added to the map
                // as its sections are read. Putting it back with insert on the
                // key already there allocates nothing.
                let mut reassembler = mem::replace(
                    self.reassemblers
                        .entry(pid)
                        .or_insert_with(SectionReassembler::new),
                    SectionReassembler::new(),
                );
                reassemble(
                    &mut reassembler,
                    Some((&mut self.routes, &mut self.reassemblers)),
                    &mut self.gate,
                    verdict,
                    &packet,
                    payload_start,
                    pending,
                );
                self.reassemblers.insert(pid, reassembler);
            }
            _ => {
                let reassembler = if pid == Pid::CAT {
                    Some(
                        self.reassemblers
                            .entry(pid)
                            .or_insert_with(SectionReassembler::new),
                    )
                } else {
                    self.reassemblers.get_mut(&pid)
                };
                if let Some(reassembler) = reassembler {
                    reassemble(
                        reassembler,
                        None,
                        &mut self.gate,
                        verdict,
                        &packet,
                        payload_start,
                        pending,
                    );
                }
            }
        }
    }
}

/// Feeds a packet's payload, which starts at `payload_start` in the framer's
/// buffer, to the reassembler of its PID, and queues the sections it
/// completes
///
/// With `program_maps`, the packet is on the PAT PID, and the routes follow
/// the program maps its sections name in the reassembler map.
fn reassemble(
    reassembler: &mut SectionReassembler,
    mut program_maps: Option<(
        &mut ProgramMapRoutes,
        &mut BTreeMap<Pid, SectionReassembler>,
    )>,
    gate: &mut VersionGate,
    verdict: Verdict,
    packet: &TransportPacket<'_>,
    payload_start: usize,
    pending: &mut VecDeque<Pending>,
) {
    let pid = packet.pid;
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
    reassembler.push(
        packet.payload_unit_start_indicator,
        payload,
        &mut |completed, section_bytes| {
            let section = match Section::read(section_bytes) {
                Ok(section) => section,
                Err(error) => {
                    pending.push_back(Pending::Ready(Event::Error(Error::Section { pid, error })));
                    return;
                }
            };
            let event = section_event(pid, &section);
            let tracked = match &event {
                Event::ProgramAssociationSection(pat) => Some((
                    TableSection::ProgramAssociation {
                        transport_stream_id: pat.transport_stream_id,
                        section_number: pat.section_number,
                    },
                    pat.version_number,
                    pat.current_next_indicator,
                )),
                Event::TsProgramMapSection { section: pmt, .. } => Some((
                    TableSection::TsProgramMap {
                        program_number: pmt.program_number,
                        section_number: pmt.section_number,
                    },
                    pmt.version_number,
                    pmt.current_next_indicator,
                )),
                Event::CaSection(cat) => Some((
                    TableSection::ConditionalAccess {
                        section_number: cat.section_number,
                    },
                    cat.version_number,
                    cat.current_next_indicator,
                )),
                Event::Error(error) => {
                    pending.push_back(Pending::Ready(Event::Error(*error)));
                    return;
                }
                Event::SyncAcquired { .. }
                | Event::ProgramClockReference { .. }
                | Event::Section { .. }
                | Event::Unsynchronized { .. } => None,
            };
            if let Some((table_section, version_number, current_next_indicator)) = tracked {
                // Why not take a section with current_next_indicator '0' as a
                // change: it announces the next table, and the table in force
                // stays until that version comes with the indicator set
                // (§2.4.4.5).
                if !current_next_indicator {
                    return;
                }
                if let (Some((routes, reassemblers)), Event::ProgramAssociationSection(pat)) =
                    (program_maps.as_mut(), &event)
                {
                    routes.follow(pat, reassemblers, gate);
                }
                if gate.admit(table_section, version_number) == Admission::Resent {
                    return;
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
                    body: &[],
                    ..section
                },
            });
        },
    );
}

/// Returns the event of a section read whole on `pid`
///
/// A section of the PAT, of the CAT, or of a PMT on any other PID, is typed,
/// or failed if it cannot be; any other section is reported as read.
pub(super) fn section_event<'a>(pid: Pid, section: &Section<'a>) -> Event<'a> {
    match (pid, section.table_id) {
        (Pid::PAT, TableId::PROGRAM_ASSOCIATION) => ProgramAssociationSection::try_from(section)
            .map_or_else(
                |error| Event::Error(Error::ProgramAssociationSection { error }),
                Event::ProgramAssociationSection,
            ),
        (Pid::CAT, TableId::CONDITIONAL_ACCESS) => CaSection::try_from(section).map_or_else(
            |error| Event::Error(Error::CaSection { error }),
            Event::CaSection,
        ),
        (Pid::PAT | Pid::CAT, _) => Event::Section {
            pid,
            section: section.clone(),
        },
        (_, TableId::TS_PROGRAM_MAP) => TsProgramMapSection::try_from(section).map_or_else(
            |error| Event::Error(Error::TsProgramMapSection { pid, error }),
            |section| Event::TsProgramMapSection { pid, section },
        ),
        _ => Event::Section {
            pid,
            section: section.clone(),
        },
    }
}

#[cfg(test)]
mod tests;
