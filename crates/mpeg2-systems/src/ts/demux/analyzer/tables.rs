//! The PAT, PMTs and CAT a demuxer follows: which sections are reported
//! typed, and at which versions

use crate::psi::TableId;
use crate::psi::cat::CaSection;
use crate::psi::pat::ProgramAssociationSection;
use crate::psi::pmt::TsProgramMapSection;
use crate::psi::section::Section;
use crate::ts::Pid;
use crate::ts::demux::{Error, Event};

mod program_map_routes;
mod version_gate;

use program_map_routes::ProgramMapRoutes;
use version_gate::{Admission, TableSection, VersionGate};

/// What the analyzer knows of the PAT, PMTs and CAT on air
#[derive(Debug, Clone)]
pub(super) struct Tables {
    routes: ProgramMapRoutes,
    gate: VersionGate,
}

/// What following a PAT section did to the programs named
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Followed {
    /// The section was already in: nothing changed
    Repeated,
    /// The section's programs are named, and its version is not whole yet
    Gathering,
    /// The section made its version whole, whose programs replaced those of
    /// the previous one
    Whole,
}

/// A table whose sections are reported typed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typed {
    ProgramAssociation,
    TsProgramMap,
    ConditionalAccess,
}

impl Tables {
    pub(super) const fn new() -> Self {
        Self {
            routes: ProgramMapRoutes::new(),
            gate: VersionGate::new(),
        }
    }

    /// Returns whether `pid` carries the PSI the demuxer follows: the PAT and
    /// CAT PIDs, and a PID the PAT names for a program map
    pub(super) fn carries_psi(&self, pid: Pid) -> bool {
        [Pid::PAT, Pid::CAT].contains(&pid) || self.routes.follows(pid)
    }

    /// Returns the event of a section read whole on `pid`, or `None` if it
    /// is not reported: a section of a typed table that announces the next
    /// version, or one at the version last reported
    pub(super) fn admit<'a>(&mut self, pid: Pid, section: &Section<'a>) -> Option<Event<'a>> {
        let typed = self.typed(pid, section);
        if let (Some(typed), Some(syntax)) = (typed, section.syntax) {
            // Why not take a section with current_next_indicator '0' as a
            // change: it announces the next table, and the table in force
            // stays until that version comes with the indicator set
            // (§2.4.4.5).
            if !syntax.current_next_indicator {
                return None;
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
            // Why gate on the syntax before the typed conversion: a resend,
            // by far the most common section, then costs no conversion; a
            // section that fails the conversion is reported once per
            // version, like one that passes.
            if self.gate.admit(table_section, syntax.version_number) == Admission::Resent {
                return None;
            }
        }
        Some(event_of(pid, section, typed))
    }

    /// Returns the event of a section read whole on `pid`, typed as it was
    /// when admitted
    pub(super) fn event<'a>(&self, pid: Pid, section: &Section<'a>) -> Event<'a> {
        event_of(pid, section, self.typed(pid, section))
    }

    /// Takes a PAT section admitted, and follows the program maps it names
    ///
    /// Each program a section names is named at once. Once every section of
    /// the version is in, the version's programs replace those of the
    /// previous one, and a program the version no longer names, or a PAT
    /// section past its `last_section_number`, loses its version memory.
    pub(super) fn follow(&mut self, pat: &ProgramAssociationSection<'_>) -> Followed {
        // Why forget the programs of a version left unfinished when another
        // starts, not only once a version is whole: a stream that never
        // completes a version would otherwise grow the version memory with
        // every version it starts.
        if self.routes.starts_version(pat) {
            self.gate
                .retain(pat.transport_stream_id, None, |pid, program_number| {
                    self.routes.whole_version_names(pid, program_number)
                });
        }
        let followed = self.routes.follow(pat);
        if followed == Followed::Whole {
            self.gate.retain(
                pat.transport_stream_id,
                Some(pat.last_section_number),
                |pid, program_number| self.routes.whole_version_names(pid, program_number),
            );
        }
        followed
    }

    /// Returns the table a section read on `pid` is reported as, if typed
    fn typed(&self, pid: Pid, section: &Section<'_>) -> Option<Typed> {
        match (pid, section.table_id) {
            (Pid::PAT, TableId::PROGRAM_ASSOCIATION) => Some(Typed::ProgramAssociation),
            (Pid::CAT, TableId::CONDITIONAL_ACCESS) => Some(Typed::ConditionalAccess),
            (Pid::PAT | Pid::CAT, _) => None,
            // Why type a PMT section that lacks the syntax on a program map
            // PID: it then fails as a PMT section and is reported as an
            // error, rather than passing as a section of another table.
            (_, TableId::TS_PROGRAM_MAP) => (self.routes.follows(pid)
                && section
                    .syntax
                    .is_none_or(|syntax| self.routes.names(pid, syntax.table_id_extension)))
            .then_some(Typed::TsProgramMap),
            _ => None,
        }
    }
}

/// Returns the `program_map_PID` of each program map a PAT section names
pub(super) fn program_map_pids<'a>(
    pat: &'a ProgramAssociationSection<'_>,
) -> impl Iterator<Item = Pid> + 'a {
    program_map_routes::program_maps(pat).map(|(pid, _)| pid)
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
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::Tables;
    use super::version_gate::{Admission, TableSection};
    use crate::psi::VersionNumber;
    use crate::psi::pat::ProgramAssociationSection;
    use crate::psi::section::Section;
    use crate::ts::Pid;
    use crate::ts::demux::tests::{
        behind_null_packets, events_of, pat_section_of, pmt_section, section_packet,
    };
    use crate::ts::demux::{Demuxer, DemuxerConfig};

    fn version_0() -> VersionNumber {
        VersionNumber::try_from(0).unwrap()
    }

    fn pmt(pid: u16, program_number: u16) -> TableSection {
        TableSection::TsProgramMap {
            pid: Pid::try_from(pid).unwrap(),
            program_number,
            section_number: 0,
        }
    }

    /// Follows a PAT section of `version_number`, numbered as `numbers`
    /// (`section_number`, `last_section_number`), that names `programs`
    fn follow(tables: &mut Tables, version_number: u8, numbers: (u8, u8), programs: &[(u16, u16)]) {
        let bytes = pat_section_of(version_number, numbers, programs);
        let section = Section::read(&bytes).unwrap();
        tables.follow(&ProgramAssociationSection::try_from(&section).unwrap());
    }

    /// Reports version 0 of the PMT of each (`program_map_PID`,
    /// `program_number`)
    fn remember(tables: &mut Tables, programs: &[(u16, u16)]) {
        for &(pid, program_number) in programs {
            tables.gate.admit(pmt(pid, program_number), version_0());
        }
    }

    /// Whether the version memory holds each PMT asked about
    fn remembered(tables: &Tables, programs: &[(u16, u16)]) -> Vec<bool> {
        let mut gate = tables.gate.clone();
        programs
            .iter()
            .map(|&(pid, program_number)| {
                gate.admit(pmt(pid, program_number), version_0()) == Admission::Resent
            })
            .collect()
    }

    const PROGRAMS: [(u16, u16); 3] = [(0x0100, 1), (0x0200, 2), (0x0300, 3)];

    #[test]
    fn a_pat_of_two_sections_forgets_nothing_until_both_are_in() {
        let mut tables = Tables::new();
        follow(&mut tables, 0, (0, 0), &[(1, 0x0100), (2, 0x0200)]);
        remember(&mut tables, &PROGRAMS);
        follow(&mut tables, 1, (1, 1), &[(3, 0x0300)]);
        let with_section_1 = remembered(&tables, &PROGRAMS);
        follow(&mut tables, 1, (0, 1), &[(1, 0x0100)]);
        assert_eq!(
            (with_section_1, remembered(&tables, &PROGRAMS)),
            (vec![true, true, false], vec![true, false, false])
        );
    }

    #[test]
    fn a_new_version_forgets_the_programs_it_no_longer_names() {
        let mut tables = Tables::new();
        follow(&mut tables, 0, (0, 0), &[(1, 0x0100), (2, 0x0200)]);
        remember(&mut tables, &PROGRAMS);
        follow(&mut tables, 1, (0, 0), &[(2, 0x0200), (3, 0x0300)]);
        assert_eq!(remembered(&tables, &PROGRAMS), vec![false, true, false]);
    }

    #[test]
    fn a_program_dropped_from_a_shared_program_map_pid_is_forgotten_alone() {
        let shared = [(0x0100, 1), (0x0100, 2)];
        let mut tables = Tables::new();
        follow(&mut tables, 0, (0, 0), &[(1, 0x0100), (2, 0x0100)]);
        remember(&mut tables, &shared);
        follow(&mut tables, 1, (0, 0), &[(2, 0x0100)]);
        assert_eq!(remembered(&tables, &shared), vec![false, true]);
    }

    #[test]
    fn a_version_started_forgets_the_programs_only_an_unfinished_one_named() {
        let mut tables = Tables::new();
        follow(&mut tables, 0, (0, 0), &[(1, 0x0100)]);
        follow(&mut tables, 1, (0, 1), &[(2, 0x0200)]);
        remember(&mut tables, &PROGRAMS);
        follow(&mut tables, 2, (0, 1), &[(3, 0x0300)]);
        assert_eq!(remembered(&tables, &PROGRAMS), vec![true, false, false]);
    }

    // Why not test through poll_event: the version memory changes no event,
    // only the memory the demuxer holds.
    #[test]
    fn the_version_memory_holds_no_more_pmts_than_the_pat_names() {
        let pat = pat_section_of(0, (0, 0), &[(1, 0x0100)]);
        let mut packets = vec![section_packet(0x0000, 0, &pat)];
        for (program_number, continuity_counter) in (1_u16..=300).zip((0_u8..16).cycle()) {
            packets.push(section_packet(
                0x0100,
                continuity_counter,
                &pmt_section(program_number, 0, &[]),
            ));
        }
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        events_of(&mut demuxer, &behind_null_packets(&packets));
        assert_eq!(demuxer.analyzer.tables.gate.len(), 2);
    }
}
