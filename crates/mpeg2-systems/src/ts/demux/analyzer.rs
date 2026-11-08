//! Analysis of each Transport Packet the framer cuts
//!
//! The events of one packet are queued in a fixed order: a packet that cannot
//! be read, or one with `transport_error_indicator` set, gives one error and
//! nothing else; any other gives a discontinuity, then its PCR, then the
//! sections it completes, each read or failed, then the PES packets it ends
//! on a subscribed PID. A PAT, PMT or CAT section is queued only at a version
//! not reported before.

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
mod pes_reassembly;
mod program_map_routes;
mod section_reassembly;
mod version_gate;

use continuity::{ContinuityTracker, Verdict};
use pes_reassembly::PesReassembly;
use program_map_routes::ProgramMapRoutes;
use section_reassembly::{Completed, SectionReassembler};
use version_gate::{Admission, TableSection, VersionGate};

/// Continuity, section reassembly and table version state of the PIDs a
/// demuxer follows
///
/// Sections are reassembled on the PAT and CAT PIDs, and on every PID the
/// PAT names for a program map; PES packets on the PIDs subscribed to.
#[derive(Debug, Clone)]
pub(super) struct PacketAnalyzer {
    continuity: ContinuityTracker,
    reassemblers: BTreeMap<Pid, SectionReassembler>,
    tables: Tables,
    pes: PesReassembly,
}

/// What the analyzer knows of the PAT, PMTs and CAT on air
#[derive(Debug, Clone)]
struct Tables {
    routes: ProgramMapRoutes,
    gate: VersionGate,
}

/// A table whose sections are reported typed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typed {
    ProgramAssociation,
    TsProgramMap,
    ConditionalAccess,
}

impl Tables {
    /// Returns the table a section read on `pid` is reported as, if typed
    fn typed(&self, pid: Pid, section: &Section<'_>) -> Option<Typed> {
        match (pid, section.table_id) {
            (Pid::PAT, TableId::PROGRAM_ASSOCIATION) => Some(Typed::ProgramAssociation),
            (Pid::CAT, TableId::CONDITIONAL_ACCESS) => Some(Typed::ConditionalAccess),
            (Pid::PAT | Pid::CAT, _) => None,
            // Why type a PMT section that lacks the syntax: it then fails as
            // a PMT section and is reported as an error, rather than passing
            // as a section of another table.
            (_, TableId::TS_PROGRAM_MAP) => section
                .syntax
                .is_none_or(|syntax| self.routes.names(pid, syntax.table_id_extension))
                .then_some(Typed::TsProgramMap),
            _ => None,
        }
    }
}

impl PacketAnalyzer {
    pub(super) const fn new(pes_buffer_limit: usize) -> Self {
        Self {
            continuity: ContinuityTracker::new(),
            reassemblers: BTreeMap::new(),
            tables: Tables {
                routes: ProgramMapRoutes::new(),
                gate: VersionGate::new(),
            },
            pes: PesReassembly::new(pes_buffer_limit),
        }
    }

    pub(super) fn subscribe_pes(&mut self, pid: Pid) {
        self.pes.subscribe(pid);
    }

    pub(super) fn unsubscribe_pes(&mut self, pid: Pid, pending: &mut VecDeque<Pending>) {
        self.pes.unsubscribe(pid, pending);
    }

    /// Returns the bytes of a [`Lent::Reassembled`] range of a PES packet on
    /// `pid`
    pub(super) fn pes_bytes(&self, pid: Pid, range: Range<usize>) -> &[u8] {
        self.pes.bytes(pid, range)
    }

    /// Queues the PES packets in flight, ended by the end of input, then
    /// resets as [`PacketAnalyzer::reset`] does
    pub(super) fn finish(&mut self, pending: &mut VecDeque<Pending>) {
        // Why end the PES packets before the reset: the reset drops what is
        // in flight, and ending them moves their bytes out of flight.
        self.pes.finish(pending);
        self.reset();
    }

    /// Drops the continuity history, every section and every PES packet in
    /// flight, once no event is queued; the PIDs learned from a PAT, the PAT
    /// sections gathered, the versions reported and the PES subscriptions
    /// stay
    pub(super) fn reset(&mut self) {
        // Why not forget the versions reported, nor the PAT sections
        // gathered: after a resynchronization or across an end of input, the
        // tables on air are most likely the same, so reporting them again
        // would read as a change, and a version names the same programs
        // before and after the gap.
        self.continuity.reset();
        for reassembler in self.reassemblers.values_mut() {
            reassembler.clear();
        }
        self.pes.discard_in_flight();
    }

    /// Returns the event of a section read whole on `pid`, typed as it was
    /// when analyzed
    pub(super) fn section_event<'a>(&self, pid: Pid, section: &Section<'a>) -> Event<'a> {
        event_of(pid, section, self.tables.typed(pid, section))
    }

    /// Returns the bytes of a [`Lent::Reassembled`] range of `pid`
    pub(super) fn reassembled(&self, pid: Pid, range: Range<usize>) -> &[u8] {
        self.reassemblers
            .get(&pid)
            .map(|reassembler| reassembler.section(range))
            .unwrap_or_default()
    }

    /// Analyzes one packet, which starts at `packet_start` in the framer's
    /// buffer and whose unit starts at input `offset`, and queues its events
    pub(super) fn analyze(
        &mut self,
        bytes: &[u8],
        packet_start: usize,
        offset: u64,
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
                    &mut self.tables,
                    Some(&mut self.reassemblers),
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
                        &mut self.tables,
                        None,
                        verdict,
                        &packet,
                        payload_start,
                        pending,
                    );
                }
            }
        }
        self.pes
            .push(&packet, verdict, payload_start, offset, pending);
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
    tables: &mut Tables,
    mut program_maps: Option<&mut BTreeMap<Pid, SectionReassembler>>,
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
            let typed = tables.typed(pid, &section);
            if let (Some(typed), Some(syntax)) = (typed, section.syntax) {
                // Why not take a section with current_next_indicator '0' as a
                // change: it announces the next table, and the table in force
                // stays until that version comes with the indicator set
                // (§2.4.4.5).
                if !syntax.current_next_indicator {
                    return;
                }
                let table_section = match typed {
                    Typed::ProgramAssociation => TableSection::ProgramAssociation {
                        transport_stream_id: syntax.table_id_extension,
                        section_number: syntax.section_number,
                    },
                    Typed::TsProgramMap => TableSection::TsProgramMap {
                        pid,
                        program_number: syntax.table_id_extension,
                        section_number: syntax.section_number,
                    },
                    Typed::ConditionalAccess => TableSection::ConditionalAccess {
                        section_number: syntax.section_number,
                    },
                };
                // Why gate on the syntax before the typed conversion: a
                // resend, by far the most common section, then costs no
                // conversion; a section that fails the conversion is
                // reported once per version, like one that passes.
                if tables.gate.admit(table_section, syntax.version_number) == Admission::Resent {
                    return;
                }
            }
            let event = event_of(pid, &section, typed);
            if let Event::Error(error) = event {
                pending.push_back(Pending::Ready(Event::Error(error)));
                return;
            }
            if let (Event::ProgramAssociationSection(pat), Some(program_maps)) =
                (&event, program_maps.as_deref_mut())
            {
                tables.routes.follow(pat, program_maps, &mut tables.gate);
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

/// Returns the event of a section read whole on `pid`, typed as `typed`
/// says, or failed if it cannot be
fn event_of<'a>(pid: Pid, section: &Section<'a>, typed: Option<Typed>) -> Event<'a> {
    match typed {
        Some(Typed::ProgramAssociation) => ProgramAssociationSection::try_from(section)
            .map_or_else(
                |error| Event::Error(Error::ProgramAssociationSection { error }),
                Event::ProgramAssociationSection,
            ),
        Some(Typed::TsProgramMap) => TsProgramMapSection::try_from(section).map_or_else(
            |error| Event::Error(Error::TsProgramMapSection { pid, error }),
            |section| Event::TsProgramMapSection { pid, section },
        ),
        Some(Typed::ConditionalAccess) => CaSection::try_from(section).map_or_else(
            |error| Event::Error(Error::CaSection { error }),
            Event::CaSection,
        ),
        None => Event::Section {
            pid,
            section: section.clone(),
        },
    }
}

#[cfg(test)]
mod tests;
