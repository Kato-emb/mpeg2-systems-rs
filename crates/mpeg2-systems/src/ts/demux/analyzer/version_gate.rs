//! The versions of the PAT, PMT and CAT sections a demuxer reported

use alloc::collections::BTreeMap;

use crate::psi::VersionNumber;
use crate::ts::Pid;

/// A section of a table reported by version, told apart as §2.4.4.5 tells
/// the tables apart
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum TableSection {
    ProgramAssociation {
        transport_stream_id: u16,
        section_number: u8,
    },
    TsProgramMap {
        pid: Pid,
        program_number: u16,
        section_number: u8,
    },
    ConditionalAccess {
        section_number: u8,
    },
}

/// What the version memory makes of a section in force
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Admission {
    /// Seen for the first time, or at a version other than the one last
    /// reported
    New,
    /// At the version last reported
    Resent,
}

/// The version each table section in force was last reported at
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct VersionGate {
    reported: BTreeMap<TableSection, VersionNumber>,
}

impl VersionGate {
    pub(super) const fn new() -> Self {
        Self {
            reported: BTreeMap::new(),
        }
    }

    /// Classifies a section in force by its version, and remembers the
    /// version
    pub(super) fn admit(
        &mut self,
        section: TableSection,
        version_number: VersionNumber,
    ) -> Admission {
        match self.reported.insert(section, version_number) {
            Some(reported) if reported == version_number => Admission::Resent,
            _ => Admission::New,
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.reported.len()
    }

    /// Forgets the PAT sections of a Transport Stream other than
    /// `transport_stream_id` or past `last_section_number`, if given, and the
    /// PMT sections of each `program_map_PID` and `program_number` that
    /// `named` refuses
    pub(super) fn retain(
        &mut self,
        transport_stream_id: u16,
        last_section_number: Option<u8>,
        mut named: impl FnMut(Pid, u16) -> bool,
    ) {
        // Why forget another transport_stream_id: the memory would otherwise
        // grow with each one a stream names, which untrusted input chooses.
        self.reported.retain(|section, _| match *section {
            // Why forget a section past the last: its version stays
            // remembered otherwise, and when the version numbers come round
            // to it again, the section that comes back is taken for a resend.
            TableSection::ProgramAssociation {
                transport_stream_id: remembered,
                section_number,
            } => {
                remembered == transport_stream_id
                    && last_section_number.is_none_or(|last| section_number <= last)
            }
            TableSection::TsProgramMap {
                pid,
                program_number,
                ..
            } => named(pid, program_number),
            TableSection::ConditionalAccess { .. } => true,
        });
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{Admission, TableSection, VersionGate};
    use crate::psi::VersionNumber;
    use crate::ts::Pid;

    fn version(value: u8) -> VersionNumber {
        VersionNumber::try_from(value).unwrap()
    }

    fn pat(transport_stream_id: u16) -> TableSection {
        TableSection::ProgramAssociation {
            transport_stream_id,
            section_number: 0,
        }
    }

    fn pmt(program_number: u16) -> TableSection {
        TableSection::TsProgramMap {
            pid: Pid::try_from(0x0100).unwrap(),
            program_number,
            section_number: 0,
        }
    }

    #[test]
    fn a_section_is_new_when_first_seen_and_at_each_change_of_version() {
        let mut gate = VersionGate::new();
        let admissions: Vec<Admission> = [0, 0, 1, 1, 0]
            .into_iter()
            .map(|value| gate.admit(pmt(1), version(value)))
            .collect();
        assert_eq!(
            admissions,
            [
                Admission::New,
                Admission::Resent,
                Admission::New,
                Admission::Resent,
                Admission::New
            ]
        );
    }

    #[test]
    fn each_program_and_each_section_has_a_version_of_its_own() {
        let mut gate = VersionGate::new();
        let sections = [
            pmt(1),
            pmt(2),
            pat(1),
            TableSection::ProgramAssociation {
                transport_stream_id: 1,
                section_number: 1,
            },
            TableSection::ConditionalAccess { section_number: 0 },
        ];
        let mut admit_all = || {
            sections
                .iter()
                .map(|section| gate.admit(*section, version(3)))
                .collect::<Vec<_>>()
        };
        let first = admit_all();
        let again = admit_all();
        assert_eq!(
            (first, again),
            (
                [Admission::New; 5].to_vec(),
                [Admission::Resent; 5].to_vec()
            )
        );
    }

    #[test]
    fn forgotten_programs_transport_streams_and_sections_are_new_again_at_the_same_version() {
        let mut gate = VersionGate::new();
        let past_the_last = TableSection::ProgramAssociation {
            transport_stream_id: 2,
            section_number: 1,
        };
        let sections = [pmt(1), pmt(2), pat(1), pat(2), past_the_last];
        for section in sections {
            gate.admit(section, version(3));
        }
        gate.retain(2, Some(0), |_, program_number| program_number == 2);
        assert_eq!(
            sections.map(|section| gate.admit(section, version(3))),
            [
                Admission::New,
                Admission::Resent,
                Admission::New,
                Admission::Resent,
                Admission::New
            ]
        );
    }
}
