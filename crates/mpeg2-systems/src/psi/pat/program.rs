//! Program loop of the program association table: each program and the PID
//! of its program map table

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::num::NonZeroU16;

use super::Error;
use crate::field::FieldReader;
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::ts::Pid;

/// Entry of the program loop of a
/// [`ProgramAssociationSection`](super::ProgramAssociationSection)
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
    pub(super) const fn check(self) -> Result<Self, Error> {
        let pid = self.pid();
        if pid.is_assignable() {
            Ok(self)
        } else {
            Err(Error::Pid {
                field: self.pid_field(),
                pid,
            })
        }
    }
}

/// The program loop of a
/// [`ProgramAssociationSection`](super::ProgramAssociationSection), borrowed
///
/// The bytes are read entry by entry as [`iter`](Self::iter) walks them, so
/// a view of any bytes can be made; an entry the bytes end inside is an
/// error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramLoop<'input>(pub(super) &'input [u8]);

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
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use core::num::NonZeroU16;
    ///
    /// use mpeg2_systems::psi::VersionNumber;
    /// use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
    /// use mpeg2_systems::ts::Pid;
    ///
    /// // A stream of one program, its program map table on PID 0x1000
    /// let mut programs = Vec::new();
    /// ProgramLoop::write_from(
    ///     [Program::ProgramMap {
    ///         program_number: NonZeroU16::MIN,
    ///         program_map_pid: Pid::try_from(0x1000)?,
    ///     }],
    ///     &mut programs,
    /// )?;
    /// let pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0)?)
    ///     .with_programs(ProgramLoop::from(programs.as_slice()));
    ///
    /// let mut written = Vec::new();
    /// pat.write(&mut written)?;
    /// assert_eq!(
    ///     written,
    ///     [
    ///         0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xf0, 0x00, 0x2a, 0xb1,
    ///         0x04, 0xb2,
    ///     ]
    /// );
    /// # }
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from(
        programs: impl IntoIterator<Item = Program>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        write_entries(programs.into_iter().map(Ok), out)
    }
}

/// Appends the bytes of `entries` to `out`, their `reserved` bits `'111'`,
/// leaving `out` as it was if an entry is an error or is refused
#[cfg(feature = "alloc")]
pub(super) fn write_entries(
    entries: impl IntoIterator<Item = Result<Program, Error>>,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    write_or_truncate(out, |out| {
        for program in entries {
            let program = program.and_then(Program::check)?;
            let mut writer = FieldWriter::new(out);
            // Why not an error per field: every value fits its field's width.
            (|| {
                writer.write_bits::<16>(program.program_number())?;
                writer.write_bits::<3>(0b111_u8)?;
                writer.write_bits::<13>(program.pid().get())?;
                writer.finish()
            })()
            .map_err(|error| Error::Field {
                field: "program loop",
                error,
            })?;
        }

        Ok(())
    })
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
