//! The program maps a PAT names, version by version

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::section_reassembly::SectionReassembler;
use super::version_gate::VersionGate;
use crate::psi::VersionNumber;
use crate::psi::pat::{Program, ProgramAssociationSection};
use crate::ts::Pid;

/// The PAT sections of the latest version, and the programs they name
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ProgramMapRoutes {
    gathering: Option<Gathering>,
    // Why not keep this in Gathering: the vector is cleared and reused from
    // version to version, so following a PAT allocates nothing once it has
    // grown.
    /// The `program_map_PID` and `program_number` of each program the
    /// sections gathered name, sorted
    programs: Vec<(Pid, u16)>,
}

/// Which PAT sections of one version are in
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Gathering {
    transport_stream_id: u16,
    version_number: VersionNumber,
    last_section_number: u8,
    // Why not a set of section numbers: a set allocates its nodes anew for
    // each version gathered.
    /// One bit per `section_number`
    received: [u64; 4],
}

/// How a PAT section relates to the sections gathered
enum Observation {
    /// A section of the version being gathered, already in
    Repeated,
    /// A section of the version being gathered, not yet in
    Joins,
    /// A section of another version, or the first section seen
    Starts,
}

impl ProgramMapRoutes {
    pub(super) const fn new() -> Self {
        Self {
            gathering: None,
            programs: Vec::new(),
        }
    }

    /// Returns whether a section of the latest version names `program_number`
    /// on `program_map_pid`
    pub(super) fn names(&self, program_map_pid: Pid, program_number: u16) -> bool {
        self.programs
            .binary_search(&(program_map_pid, program_number))
            .is_ok()
    }

    /// Takes a PAT section in force, and follows the program maps it names
    ///
    /// Each program map named gains a reassembler at once, and the programs
    /// of a version replace those of the previous one as its sections come
    /// in. Once every section of the version is in, a program map PID the
    /// version no longer names loses its reassembler, and a program it no
    /// longer names loses its version memory.
    pub(super) fn follow(
        &mut self,
        pat: &ProgramAssociationSection<'_>,
        reassemblers: &mut BTreeMap<Pid, SectionReassembler>,
        gate: &mut VersionGate,
    ) {
        let gathering = match (observe(self.gathering.as_ref(), pat), &mut self.gathering) {
            (Observation::Repeated, _) => return,
            (Observation::Joins, Some(gathering)) => gathering,
            (Observation::Joins | Observation::Starts, gathering) => {
                // Why forget the programs of two versions back here, not only
                // once a version is whole: a stream that never completes a
                // version would otherwise grow the version memory with every
                // version it starts.
                gate.retain(pat.transport_stream_id, |pid, program_number| {
                    self.programs.binary_search(&(pid, program_number)).is_ok()
                });
                self.programs.clear();
                gathering.insert(Gathering {
                    transport_stream_id: pat.transport_stream_id,
                    version_number: pat.version_number,
                    last_section_number: pat.last_section_number,
                    received: [0; 4],
                })
            }
        };
        if let Some(word) = gathering
            .received
            .get_mut(usize::from(pat.section_number / 64))
        {
            *word |= 1 << (pat.section_number % 64);
        }
        for program in pat.programs.iter() {
            // Why skip the PAT, CAT and null PIDs: a broken PAT can name
            // them, and the PAT and CAT are reassembled anyway while null
            // packets carry stuffing, not sections.
            if let Ok(Program::ProgramMap {
                program_number,
                program_map_pid,
            }) = program
                && ![Pid::PAT, Pid::CAT, Pid::NULL].contains(&program_map_pid)
            {
                reassemblers
                    .entry(program_map_pid)
                    .or_insert_with(SectionReassembler::new);
                self.programs.push((program_map_pid, program_number.get()));
            }
        }
        // Why sort and search rather than scan the programs for each key: a
        // PAT can name tens of thousands of programs, and the scans would
        // multiply with the PIDs followed and the PMT sections looked up.
        // Why not sort: a stable sort allocates a scratch buffer for a long
        // vector, and equal pairs need no order kept.
        self.programs.sort_unstable();
        self.programs.dedup();
        // Why compare the count with last_section_number: Section::read
        // refuses a section_number past last_section_number, so the bits set
        // are all within 0..=last_section_number, and the version is whole
        // once they outnumber it.
        let received: u32 = gathering
            .received
            .iter()
            .map(|word| word.count_ones())
            .sum();
        if received <= u32::from(gathering.last_section_number) {
            return;
        }
        reassemblers.retain(|pid, _| {
            [Pid::PAT, Pid::CAT].contains(pid)
                || self
                    .programs
                    .binary_search_by_key(pid, |&(named, _)| named)
                    .is_ok()
        });
        gate.retain(pat.transport_stream_id, |pid, program_number| {
            self.names(pid, program_number)
        });
    }
}

/// Classifies a PAT section against the sections gathered
fn observe(gathering: Option<&Gathering>, pat: &ProgramAssociationSection<'_>) -> Observation {
    let Some(gathering) = gathering.filter(|gathering| {
        (
            gathering.transport_stream_id,
            gathering.version_number,
            gathering.last_section_number,
        ) == (
            pat.transport_stream_id,
            pat.version_number,
            pat.last_section_number,
        )
    }) else {
        return Observation::Starts;
    };
    let received = gathering
        .received
        .get(usize::from(pat.section_number / 64))
        .is_some_and(|word| word & (1 << (pat.section_number % 64)) != 0);
    if received {
        Observation::Repeated
    } else {
        Observation::Joins
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::ProgramMapRoutes;
    use crate::psi::VersionNumber;
    use crate::psi::pat::ProgramAssociationSection;
    use crate::psi::section::Section;
    use crate::ts::Pid;
    use crate::ts::demux::analyzer::section_reassembly::SectionReassembler;
    use crate::ts::demux::analyzer::version_gate::{Admission, TableSection, VersionGate};

    /// A PAT section of `transport_stream_id` 1 naming `programs`, each a
    /// `program_number` and its PID
    fn pat_section(
        version_number: u8,
        section_number: u8,
        last_section_number: u8,
        programs: &[(u16, u16)],
    ) -> Vec<u8> {
        let mut program_loop = Vec::new();
        for (program_number, pid) in programs {
            program_loop.extend_from_slice(&program_number.to_be_bytes());
            program_loop.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        }
        let length = u16::try_from(program_loop.len().checked_add(9).unwrap()).unwrap();
        let [high, low] = length.to_be_bytes();
        let mut bytes = vec![
            0x00,
            0xB0 | high,
            low,
            0x00,
            0x01,
            0xC1 | (version_number << 1),
            section_number,
            last_section_number,
        ];
        bytes.extend_from_slice(&program_loop);
        bytes.extend_from_slice(&crate::crc::crc_32(&bytes).to_be_bytes());
        bytes
    }

    /// Program routes, the reassembler map of a demuxer that has seen the
    /// PAT PID only, and a version memory
    #[derive(Debug, Clone, PartialEq)]
    struct Fixture {
        routes: ProgramMapRoutes,
        reassemblers: BTreeMap<Pid, SectionReassembler>,
        gate: VersionGate,
    }

    /// The PIDs reassembled, the programs named (`program_map_PID`,
    /// `program_number`), and whether the version memory holds each PMT asked
    /// about
    type State = (Vec<u16>, Vec<(u16, u16)>, Vec<bool>);

    impl Fixture {
        fn new() -> Self {
            Self {
                routes: ProgramMapRoutes::new(),
                reassemblers: BTreeMap::from([(Pid::PAT, SectionReassembler::new())]),
                gate: VersionGate::new(),
            }
        }

        fn follow(&mut self, bytes: &[u8]) -> &mut Self {
            let section = Section::read(bytes).unwrap();
            let pat = ProgramAssociationSection::try_from(&section).unwrap();
            self.routes
                .follow(&pat, &mut self.reassemblers, &mut self.gate);
            self
        }

        /// Reports version 0 of the PMT of each (`program_map_PID`,
        /// `program_number`)
        fn remember(&mut self, programs: &[(u16, u16)]) -> &mut Self {
            for &(pid, program_number) in programs {
                self.gate.admit(pmt(pid, program_number), version_0());
            }
            self
        }

        fn state(&self, programs: &[(u16, u16)]) -> State {
            let pids = self.reassemblers.keys().map(|pid| pid.get()).collect();
            let named = self
                .routes
                .programs
                .iter()
                .map(|&(pid, program_number)| (pid.get(), program_number))
                .collect();
            let mut gate = self.gate.clone();
            let remembered = programs
                .iter()
                .map(|&(pid, program_number)| {
                    gate.admit(pmt(pid, program_number), version_0()) == Admission::Resent
                })
                .collect();
            (pids, named, remembered)
        }
    }

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

    const PROGRAMS: [(u16, u16); 3] = [(0x0100, 1), (0x0200, 2), (0x0300, 3)];

    #[test]
    fn a_pat_of_one_section_names_the_program_maps_followed() {
        let mut fixture = Fixture::new();
        fixture.follow(&pat_section(
            0,
            0,
            0,
            &[(0, 0x0010), (1, 0x0100), (2, 0x0200)],
        ));
        assert_eq!(
            fixture.state(&[]),
            (
                vec![0x0000, 0x0100, 0x0200],
                vec![(0x0100, 1), (0x0200, 2)],
                vec![]
            )
        );
    }

    #[test]
    fn a_pat_of_two_sections_drops_nothing_until_both_are_in() {
        let mut fixture = Fixture::new();
        fixture
            .follow(&pat_section(0, 0, 0, &[(1, 0x0100), (2, 0x0200)]))
            .remember(&PROGRAMS)
            .follow(&pat_section(1, 1, 1, &[(3, 0x0300)]));
        let with_section_1 = fixture.state(&PROGRAMS);
        fixture.follow(&pat_section(1, 0, 1, &[(1, 0x0100)]));
        assert_eq!(
            (with_section_1, fixture.state(&PROGRAMS)),
            (
                (
                    vec![0x0000, 0x0100, 0x0200, 0x0300],
                    vec![(0x0300, 3)],
                    vec![true, true, false]
                ),
                (
                    vec![0x0000, 0x0100, 0x0300],
                    vec![(0x0100, 1), (0x0300, 3)],
                    vec![true, false, false]
                ),
            )
        );
    }

    #[test]
    fn a_new_version_drops_the_programs_it_no_longer_names_and_adds_the_new_ones() {
        let mut fixture = Fixture::new();
        fixture
            .follow(&pat_section(0, 0, 0, &[(1, 0x0100), (2, 0x0200)]))
            .remember(&PROGRAMS)
            .follow(&pat_section(1, 0, 0, &[(2, 0x0200), (3, 0x0300)]));
        assert_eq!(
            fixture.state(&PROGRAMS),
            (
                vec![0x0000, 0x0200, 0x0300],
                vec![(0x0200, 2), (0x0300, 3)],
                vec![false, true, false]
            )
        );
    }

    #[test]
    fn a_program_map_pid_two_programs_share_stays_while_one_of_them_is_named() {
        let shared = [(0x0100, 1), (0x0100, 2)];
        let mut fixture = Fixture::new();
        fixture
            .follow(&pat_section(0, 0, 0, &[(1, 0x0100), (2, 0x0100)]))
            .remember(&shared)
            .follow(&pat_section(1, 0, 0, &[(2, 0x0100)]));
        assert_eq!(
            fixture.state(&shared),
            (vec![0x0000, 0x0100], vec![(0x0100, 2)], vec![false, true])
        );
    }

    #[test]
    fn a_resent_version_changes_nothing() {
        let mut fixture = Fixture::new();
        let section = pat_section(0, 0, 0, &[(1, 0x0100)]);
        fixture.follow(&section).remember(&PROGRAMS);
        let first = fixture.clone();
        fixture.follow(&section);
        assert_eq!(fixture, first);
    }
}
