//! The program maps a PAT names, version by version

use alloc::vec::Vec;
use core::mem;

use super::Followed;
use crate::psi::VersionNumber;
use crate::psi::pat::{Program, ProgramAssociationSection};
use crate::ts::Pid;

/// The PAT sections of the latest version, and the programs the PAT names
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProgramMapRoutes {
    gathering: Option<Gathering>,
    // Why not keep these in Gathering: the vectors are swapped, cleared and
    // reused from version to version, so following a PAT allocates nothing
    // once they have grown.
    /// The `program_map_PID` and `program_number` of each program the latest
    /// whole version names, sorted
    named: Vec<(Pid, u16)>,
    /// The same, for each program the sections of the version being gathered
    /// name, sorted
    gathered: Vec<(Pid, u16)>,
}

/// Which PAT sections of one version are in
#[derive(Debug, Clone, PartialEq, Eq)]
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
            named: Vec::new(),
            gathered: Vec::new(),
        }
    }

    /// Returns whether the latest whole version, or a section of the version
    /// being gathered, names `program_number` on `program_map_pid`
    pub(super) fn names(&self, program_map_pid: Pid, program_number: u16) -> bool {
        [&self.named, &self.gathered].into_iter().any(|programs| {
            programs
                .binary_search(&(program_map_pid, program_number))
                .is_ok()
        })
    }

    /// Returns whether the latest whole version, or a section of the version
    /// being gathered, names a program map on `pid`
    pub(super) fn follows(&self, pid: Pid) -> bool {
        [&self.named, &self.gathered].into_iter().any(|programs| {
            programs
                .binary_search_by_key(&pid, |&(named, _)| named)
                .is_ok()
        })
    }

    /// Returns whether the latest whole version names `program_number` on
    /// `program_map_pid`
    pub(super) fn whole_version_names(&self, program_map_pid: Pid, program_number: u16) -> bool {
        self.named
            .binary_search(&(program_map_pid, program_number))
            .is_ok()
    }

    /// Returns whether `pat` is of a version other than the one being
    /// gathered, whose programs [`follow`](Self::follow) would then drop
    pub(super) fn starts_version(&self, pat: &ProgramAssociationSection<'_>) -> bool {
        matches!(observe(self.gathering.as_ref(), pat), Observation::Starts)
    }

    /// Takes a PAT section in force, and names the programs it names
    ///
    /// Each program a section names is named at once. A section of another
    /// version than the one being gathered drops the programs gathered so
    /// far. Once every section of the version is in, the version's programs
    /// replace those of the previous one.
    pub(super) fn follow(&mut self, pat: &ProgramAssociationSection<'_>) -> Followed {
        let gathering = match (observe(self.gathering.as_ref(), pat), &mut self.gathering) {
            (Observation::Repeated, _) => return Followed::Repeated,
            (Observation::Joins, Some(gathering)) => gathering,
            (Observation::Joins | Observation::Starts, gathering) => {
                self.gathered.clear();
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
        self.gathered.extend(program_maps(pat));
        // Why sort and search rather than scan the programs for each key: a
        // PAT can name tens of thousands of programs, and the scans would
        // multiply with the PIDs followed and the PMT sections looked up.
        // Why not sort: a stable sort allocates a scratch buffer for a long
        // vector, and equal pairs need no order kept.
        self.gathered.sort_unstable();
        self.gathered.dedup();
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
            return Followed::Gathering;
        }
        mem::swap(&mut self.named, &mut self.gathered);
        self.gathered.clear();
        Followed::Whole
    }
}

/// Returns the `program_map_PID` and `program_number` of each program map a
/// PAT section names
pub(super) fn program_maps<'a>(
    pat: &'a ProgramAssociationSection<'_>,
) -> impl Iterator<Item = (Pid, u16)> + 'a {
    pat.programs.iter().filter_map(|program| match program {
        // Why skip the PAT, CAT and null PIDs: a broken PAT can name them,
        // and the PAT and CAT are reassembled anyway while null packets carry
        // stuffing, not sections.
        Ok(Program::ProgramMap {
            program_number,
            program_map_pid,
        }) if ![Pid::PAT, Pid::CAT, Pid::NULL].contains(&program_map_pid) => {
            Some((program_map_pid, program_number.get()))
        }
        _ => None,
    })
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
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{Followed, ProgramMapRoutes};
    use crate::psi::pat::ProgramAssociationSection;
    use crate::psi::section::Section;
    use crate::ts::Pid;

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

    /// What following a PAT section did, and the programs of the latest
    /// whole version and of the version being gathered after it
    /// (`program_map_PID`, `program_number`)
    type State = (Followed, Vec<(u16, u16)>, Vec<(u16, u16)>);

    fn follow(routes: &mut ProgramMapRoutes, bytes: &[u8]) -> State {
        let section = Section::read(bytes).unwrap();
        let pat = ProgramAssociationSection::try_from(&section).unwrap();
        let followed = routes.follow(&pat);
        let listed = |programs: &[(Pid, u16)]| {
            programs
                .iter()
                .map(|&(pid, program_number)| (pid.get(), program_number))
                .collect()
        };
        (followed, listed(&routes.named), listed(&routes.gathered))
    }

    #[test]
    fn a_pat_of_one_section_names_the_program_maps_followed() {
        let mut routes = ProgramMapRoutes::new();
        assert_eq!(
            follow(
                &mut routes,
                &pat_section(0, 0, 0, &[(0, 0x0010), (1, 0x0100), (2, 0x0200)])
            ),
            (Followed::Whole, vec![(0x0100, 1), (0x0200, 2)], vec![])
        );
    }

    #[test]
    fn a_pat_of_two_sections_replaces_nothing_until_both_are_in() {
        let mut routes = ProgramMapRoutes::new();
        follow(
            &mut routes,
            &pat_section(0, 0, 0, &[(1, 0x0100), (2, 0x0200)]),
        );
        let with_section_1 = follow(&mut routes, &pat_section(1, 1, 1, &[(3, 0x0300)]));
        let with_both = follow(&mut routes, &pat_section(1, 0, 1, &[(1, 0x0100)]));
        assert_eq!(
            (with_section_1, with_both),
            (
                (
                    Followed::Gathering,
                    vec![(0x0100, 1), (0x0200, 2)],
                    vec![(0x0300, 3)]
                ),
                (Followed::Whole, vec![(0x0100, 1), (0x0300, 3)], vec![]),
            )
        );
    }

    #[test]
    fn a_new_version_drops_the_programs_it_no_longer_names_and_adds_the_new_ones() {
        let mut routes = ProgramMapRoutes::new();
        follow(
            &mut routes,
            &pat_section(0, 0, 0, &[(1, 0x0100), (2, 0x0200)]),
        );
        assert_eq!(
            follow(
                &mut routes,
                &pat_section(1, 0, 0, &[(2, 0x0200), (3, 0x0300)])
            ),
            (Followed::Whole, vec![(0x0200, 2), (0x0300, 3)], vec![])
        );
    }

    #[test]
    fn a_program_map_pid_two_programs_share_stays_while_one_of_them_is_named() {
        let mut routes = ProgramMapRoutes::new();
        follow(
            &mut routes,
            &pat_section(0, 0, 0, &[(1, 0x0100), (2, 0x0100)]),
        );
        follow(&mut routes, &pat_section(1, 0, 0, &[(2, 0x0100)]));
        assert!(routes.follows(Pid::try_from(0x0100).unwrap()));
    }

    #[test]
    fn a_resent_version_changes_nothing() {
        let mut routes = ProgramMapRoutes::new();
        let section = pat_section(0, 0, 0, &[(1, 0x0100)]);
        follow(&mut routes, &section);
        let first = routes.clone();
        assert_eq!(
            (follow(&mut routes, &section).0, routes),
            (Followed::Repeated, first)
        );
    }
}
