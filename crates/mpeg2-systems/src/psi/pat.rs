//! Program association table, which maps each program to the PID of its
//! program map table

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;
use core::num::NonZeroU16;

#[cfg(feature = "alloc")]
use super::section::SectionSyntax;
use super::section::{self, Section};
use super::{TableId, VersionNumber};
#[cfg(feature = "alloc")]
use crate::field::FieldWriter;
use crate::field::{self, FieldReader};
use crate::ts::Pid;

/// Failure to convert a section into a [`ProgramAssociationSection`], to
/// read an entry of its program loop, or to write it
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The section is of another table than the program association table
    TableId {
        /// The `table_id` of the section
        table_id: TableId,
    },
    /// The section lacks the fields `section_syntax_indicator` `'1'`
    /// announces
    SectionSyntaxIndicator,
    /// The program loop ends inside an entry, or a field of it cannot be
    /// written
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field ran past the end
        error: field::Error,
    },
    /// A PID to write is outside `0x0010` to `0x1FFE`, the values Table 2-3
    /// lets a network or a program map take
    Pid {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The PID given
        pid: Pid,
    },
    /// A `program_number` to write appears more than once in the section
    ProgramNumber {
        /// The `program_number` repeated
        program_number: u16,
    },
    /// The section to write is refused as a section
    Section(section::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TableId { table_id } => write!(
                formatter,
                "table_id {:#04x} is not that of a program association section",
                table_id.get()
            ),
            Self::SectionSyntaxIndicator => write!(
                formatter,
                "a program association section must have section_syntax_indicator '1'"
            ),
            Self::Field { field, error } => write!(formatter, "{field}: {error}"),
            Self::Pid { field, pid } => write!(
                formatter,
                "{field} {:#06x} is outside 0x0010 to 0x1ffe",
                pid.get()
            ),
            Self::ProgramNumber { program_number } => write!(
                formatter,
                "program_number {program_number} appears more than once"
            ),
            Self::Section(error) => write!(formatter, "{error}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::Section(error) => Some(error),
            Self::TableId { .. }
            | Self::SectionSyntaxIndicator
            | Self::Pid { .. }
            | Self::ProgramNumber { .. } => None,
        }
    }
}

impl From<section::Error> for Error {
    fn from(error: section::Error) -> Self {
        Self::Section(error)
    }
}

/// Entry of the program loop of a [`ProgramAssociationSection`]
///
/// `program_number` 0 announces `network_PID`, and any other
/// `program_number` a `program_map_PID` (ISO/IEC 13818-1 §2.4.4.6).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Program {
    /// `program_number` 0: the PID of the network information table
    Network {
        /// PID of the Transport Packets carrying the network information
        /// table
        network_pid: Pid,
    },
    /// A program and the PID of its program map table
    ProgramMap {
        /// The program, defined by the user
        program_number: NonZeroU16,
        /// PID of the Transport Packets carrying the
        /// `TS_program_map_section` of the program
        program_map_pid: Pid,
    },
}

impl Program {
    /// Returns `program_number`, 0 for the network
    #[must_use]
    pub const fn program_number(self) -> u16 {
        match self {
            Self::Network { .. } => 0,
            Self::ProgramMap { program_number, .. } => program_number.get(),
        }
    }

    /// Returns the PID the entry names, `network_PID` or `program_map_PID`
    #[must_use]
    pub const fn pid(self) -> Pid {
        match self {
            Self::Network { network_pid } => network_pid,
            Self::ProgramMap {
                program_map_pid, ..
            } => program_map_pid,
        }
    }

    /// Returns the name of the PID field in the syntax table
    #[cfg(feature = "alloc")]
    const fn pid_field(self) -> &'static str {
        match self {
            Self::Network { .. } => "network_PID",
            Self::ProgramMap { .. } => "program_map_PID",
        }
    }

    /// Refuses a PID that Table 2-3 does not let a network or a program map
    /// take
    #[cfg(feature = "alloc")]
    const fn check(self) -> Result<Self, Error> {
        let pid = self.pid();
        if matches!(pid.get(), 0x0010..=0x1ffe) {
            Ok(self)
        } else {
            Err(Error::Pid {
                field: self.pid_field(),
                pid,
            })
        }
    }
}

/// The program loop of a [`ProgramAssociationSection`], borrowed
///
/// The bytes are read entry by entry as [`iter`](Self::iter) walks them, so
/// a view of any bytes can be made; an entry the bytes end inside is an
/// error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ProgramLoop<'input>(&'input [u8]);

impl<'input> ProgramLoop<'input> {
    /// Returns the bytes of the loop
    #[must_use]
    pub const fn as_bytes(self) -> &'input [u8] {
        self.0
    }

    /// Returns an iterator over the entries, each read as it is reached
    #[must_use]
    pub const fn iter(self) -> ProgramIter<'input> {
        ProgramIter(self.0)
    }

    /// Appends the bytes of a loop of `programs` to `out`, its `reserved`
    /// bits `'111'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`Pid`](Error::Pid): a PID outside `0x0010` to `0x1FFE`.
    #[cfg(feature = "alloc")]
    pub fn write_from(
        programs: impl IntoIterator<Item = Program>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        let start = out.len();
        for program in programs {
            if let Err(error) = program.check() {
                out.truncate(start);
                return Err(error);
            }
            let mut writer = FieldWriter::new(out);
            // Why not an error per field: every value fits its field's width.
            let entry = (|| {
                writer.write_bits::<16>(program.program_number())?;
                writer.write_bits::<3>(0b111_u8)?;
                writer.write_bits::<13>(program.pid().get())?;
                writer.finish()
            })();
            if let Err(error) = entry {
                out.truncate(start);
                return Err(Error::Field {
                    field: "program loop",
                    error,
                });
            }
        }

        Ok(())
    }
}

impl<'input> From<&'input [u8]> for ProgramLoop<'input> {
    fn from(bytes: &'input [u8]) -> Self {
        Self(bytes)
    }
}

impl<'input> IntoIterator for ProgramLoop<'input> {
    type Item = Result<Program, Error>;
    type IntoIter = ProgramIter<'input>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the entries of a [`ProgramLoop`]
///
/// An entry the bytes end inside is an [`Error::Field`], and the last item.
#[derive(Debug, Clone)]
pub struct ProgramIter<'input>(&'input [u8]);

impl Iterator for ProgramIter<'_> {
    type Item = Result<Program, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }

        let mut reader = FieldReader::new(self.0);
        let entry = (|| {
            let program_number = reader.read_u16::<16>().map_err(|error| Error::Field {
                field: "program_number",
                error,
            })?;
            let pid = reader
                .read_bits::<3>()
                .and_then(|_| reader.read_u16::<13>())
                .map(Pid)
                .map_err(|error| Error::Field {
                    field: if program_number == 0 {
                        "network_PID"
                    } else {
                        "program_map_PID"
                    },
                    error,
                })?;
            Ok(NonZeroU16::new(program_number).map_or(
                Program::Network { network_pid: pid },
                |program_number| Program::ProgramMap {
                    program_number,
                    program_map_pid: pid,
                },
            ))
        })();
        // Why not an error: the entry ends on a byte boundary, or the
        // entry failed and nothing after it is read.
        self.0 = reader
            .read_bytes(reader.remaining_bytes())
            .ok()
            .filter(|_| entry.is_ok())
            .unwrap_or_default();

        Some(entry)
    }
}

/// Program association section, which maps each program to the PID of its
/// program map table
///
/// `program_association_section`, ISO/IEC 13818-1 §2.4.4.4, Table 2-30,
/// semantics §2.4.4.6
///
/// It is converted from a [`Section`] the demultiplexer gathered and checked,
/// with `TryFrom`: the program association table takes no path of its own,
/// and a crate that defines a table of its own converts a [`Section`] the
/// same way. `transport_stream_id` is the section's `table_id_extension`.
/// The table may span sections; joining them is the demultiplexer's.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection};
/// use mpeg2_systems::psi::section::Section;
///
/// // The program association section of a stream of one program
/// let bytes = [
///     0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xf0, 0x00, 0x2a, 0xb1,
///     0x04, 0xb2,
/// ];
/// let section = Section::read(&bytes)?;
/// let pat = ProgramAssociationSection::try_from(&section)?;
/// assert_eq!(pat.transport_stream_id, 1);
///
/// let program = pat.programs.iter().next().unwrap()?;
/// assert_eq!(program.program_number(), 1);
/// assert_eq!(program.pid().get(), 0x1000);
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// pat.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[doc(alias("PAT", "program_association_section"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramAssociationSection<'input> {
    /// Label of the transport stream among the others of a network
    pub transport_stream_id: u16,
    /// Version of the table
    pub version_number: VersionNumber,
    /// Whether the table applies now rather than next
    pub current_next_indicator: bool,
    /// Number of this section in the table
    pub section_number: u8,
    /// Number of the last section of the table
    pub last_section_number: u8,
    /// The programs and their PIDs
    pub programs: ProgramLoop<'input>,
}

impl<'input> ProgramAssociationSection<'input> {
    /// Creates the single section of a table that applies now, without
    /// programs
    #[must_use]
    pub const fn new(transport_stream_id: u16, version_number: VersionNumber) -> Self {
        Self {
            transport_stream_id,
            version_number,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            programs: ProgramLoop(&[]),
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        super::LONG_SECTION_OVERHEAD.saturating_add(self.programs.0.len())
    }

    /// Appends the section to `out`, with its `CRC_32`
    ///
    /// The program loop is written as it is held, after every entry is read
    /// and checked. A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Field`](Error::Field): the program loop ends inside an entry.
    /// * [`Pid`](Error::Pid): a PID outside `0x0010` to `0x1FFE`.
    /// * [`ProgramNumber`](Error::ProgramNumber): a `program_number` that
    ///   appears more than once, which §2.4.4.6 forbids.
    /// * [`Section`](Error::Section): `section_length` would be larger than
    ///   1021, or `section_number` is past `last_section_number`.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let mut entries = self.programs.iter();
        while let Some(program) = entries.next() {
            let program = program?.check()?;
            let program_number = program.program_number();
            if entries
                .clone()
                .any(|other| other.is_ok_and(|other| other.program_number() == program_number))
            {
                return Err(Error::ProgramNumber { program_number });
            }
        }

        let mut section = Section::new(TableId::PROGRAM_ASSOCIATION, self.programs.0);
        section.syntax = Some(SectionSyntax::new(
            self.transport_stream_id,
            self.version_number,
            self.current_next_indicator,
            self.section_number,
            self.last_section_number,
        ));
        Ok(section.write(out)?)
    }
}

impl<'input> TryFrom<&Section<'input>> for ProgramAssociationSection<'input> {
    type Error = Error;

    /// Takes the fields of a program association section
    ///
    /// The `CRC_32` was checked when the section was read. The program loop
    /// is not read until it is walked.
    ///
    /// # Errors
    ///
    /// * [`TableId`](Error::TableId): `table_id` is not 0x00.
    /// * [`SectionSyntaxIndicator`](Error::SectionSyntaxIndicator): the
    ///   section lacks `transport_stream_id` to `last_section_number`.
    fn try_from(section: &Section<'input>) -> Result<Self, Self::Error> {
        if section.table_id != TableId::PROGRAM_ASSOCIATION {
            return Err(Error::TableId {
                table_id: section.table_id,
            });
        }
        let syntax = section.syntax.ok_or(Error::SectionSyntaxIndicator)?;

        Ok(Self {
            transport_stream_id: syntax.table_id_extension,
            version_number: syntax.version_number,
            current_next_indicator: syntax.current_next_indicator,
            section_number: syntax.section_number,
            last_section_number: syntax.last_section_number,
            programs: ProgramLoop(section.body),
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;
    use core::array;
    use core::num::NonZeroU16;

    use super::{Error, Program, ProgramAssociationSection, ProgramLoop};
    use crate::field;
    use crate::psi::TableId;
    #[cfg(feature = "alloc")]
    use crate::psi::VersionNumber;
    #[cfg(feature = "alloc")]
    use crate::psi::section;
    use crate::psi::section::Section;
    use crate::ts::Pid;

    /// A program association section of a network and two programs, its
    /// `CRC_32` computed outside this crate
    const NETWORK_AND_TWO_PROGRAMS: [u8; 24] = [
        0x00, 0xb0, 0x15, 0x12, 0x34, 0xc7, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x10, 0x00, 0x01, 0xf0,
        0x00, 0x00, 0x02, 0xf0, 0x01, 0xe6, 0x55, 0x30, 0xf0,
    ];

    fn program(program_number: u16, pid: u16) -> Program {
        let pid = Pid::try_from(pid).unwrap();
        NonZeroU16::new(program_number).map_or(
            Program::Network { network_pid: pid },
            |program_number| Program::ProgramMap {
                program_number,
                program_map_pid: pid,
            },
        )
    }

    #[test]
    fn a_program_number_of_0_announces_the_network_pid() {
        let section = Section::read(&NETWORK_AND_TWO_PROGRAMS).unwrap();
        let pat = ProgramAssociationSection::try_from(&section).unwrap();
        let mut programs = pat.programs.iter();

        assert_eq!(
            (
                pat.transport_stream_id,
                pat.version_number.get(),
                array::from_fn(|_| programs.next())
            ),
            (
                0x1234,
                3,
                [
                    Some(Ok(program(0, 0x0010))),
                    Some(Ok(program(1, 0x1000))),
                    Some(Ok(program(2, 0x1001))),
                    None
                ]
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_section_built_from_its_programs_is_written_with_its_crc_32() {
        let mut programs = Vec::new();
        ProgramLoop::write_from(
            [program(0, 0x0010), program(1, 0x1000), program(2, 0x1001)],
            &mut programs,
        )
        .unwrap();
        let mut pat = ProgramAssociationSection::new(0x1234, VersionNumber::try_from(3).unwrap());
        pat.programs = ProgramLoop::from(programs.as_slice());
        let mut written = Vec::new();
        pat.write(&mut written).unwrap();

        assert_eq!(
            (written.as_slice(), pat.encoded_len()),
            (
                NETWORK_AND_TWO_PROGRAMS.as_slice(),
                NETWORK_AND_TWO_PROGRAMS.len()
            )
        );
    }

    #[test]
    fn a_section_of_another_table_or_without_the_syntax_fields_is_not_converted() {
        let mut pmt = Section::new(TableId::TS_PROGRAM_MAP, &[]);
        pmt.syntax = Section::read(&NETWORK_AND_TWO_PROGRAMS).unwrap().syntax;
        let private = Section::read(&[0x80, 0x70, 0x00]).unwrap();
        let short = Section::new(TableId::PROGRAM_ASSOCIATION, &[]);

        assert_eq!(
            [&pmt, &private, &short].map(ProgramAssociationSection::try_from),
            [
                Err(Error::TableId {
                    table_id: TableId::TS_PROGRAM_MAP
                }),
                Err(Error::TableId {
                    table_id: TableId::from(0x80)
                }),
                Err(Error::SectionSyntaxIndicator),
            ]
        );
    }

    #[test]
    fn a_program_loop_ending_inside_an_entry_yields_the_entries_before_then_an_error() {
        let mut programs =
            ProgramLoop::from([0x00, 0x01, 0xf0, 0x00, 0x00, 0x02, 0xf0].as_slice()).iter();

        assert_eq!(
            array::from_fn(|_| programs.next()),
            [
                Some(Ok(program(1, 0x1000))),
                Some(Err(Error::Field {
                    field: "program_map_PID",
                    error: field::Error::Truncated {
                        required_bits: 13,
                        remaining_bits: 5
                    }
                })),
                None,
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn sections_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let version = VersionNumber::try_from(0).unwrap();
        let mut reserved_pid = Vec::new();
        let reserved_pid = ProgramLoop::write_from([program(0, 0x000f)], &mut reserved_pid);
        let null_pid = ProgramLoop::write_from([program(1, 0x1fff)], &mut Vec::new());
        let mut repeated_bytes = Vec::new();
        ProgramLoop::write_from(
            [program(1, 0x1000), program(2, 0x1001), program(1, 0x1002)],
            &mut repeated_bytes,
        )
        .unwrap();
        let mut repeated = ProgramAssociationSection::new(1, version);
        repeated.programs = ProgramLoop::from(repeated_bytes.as_slice());
        let mut too_many_bytes = Vec::new();
        ProgramLoop::write_from(
            (1..=254).map(|number| program(number, 0x1000)),
            &mut too_many_bytes,
        )
        .unwrap();
        let mut too_many = ProgramAssociationSection::new(1, version);
        too_many.programs = ProgramLoop::from(too_many_bytes.as_slice());
        let mut reversed = ProgramAssociationSection::new(1, version);
        reversed.section_number = 1;
        let mut truncated = ProgramAssociationSection::new(1, version);
        truncated.programs = ProgramLoop::from([0x00, 0x01, 0xf0].as_slice());
        let mut written = Vec::new();

        assert_eq!(
            [
                reserved_pid,
                null_pid,
                repeated.write(&mut written),
                too_many.write(&mut written),
                reversed.write(&mut written),
                truncated.write(&mut written),
            ],
            [
                Err(Error::Pid {
                    field: "network_PID",
                    pid: Pid::try_from(0x000f).unwrap()
                }),
                Err(Error::Pid {
                    field: "program_map_PID",
                    pid: Pid::NULL
                }),
                Err(Error::ProgramNumber { program_number: 1 }),
                Err(Error::Section(section::Error::SectionLength {
                    section_length: 1025,
                    limit: 1021
                })),
                Err(Error::Section(section::Error::SectionNumber {
                    section_number: 1,
                    last_section_number: 0
                })),
                Err(Error::Field {
                    field: "program_map_PID",
                    error: field::Error::Truncated {
                        required_bits: 13,
                        remaining_bits: 5
                    }
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
