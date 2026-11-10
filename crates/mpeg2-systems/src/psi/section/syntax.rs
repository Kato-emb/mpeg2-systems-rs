//! Fields a section with `section_syntax_indicator` `'1'` has after its
//! length

use super::{Error, RESERVED};
use crate::psi::VersionNumber;

/// The fields a section with `section_syntax_indicator` `'1'` has after
/// `section_length`
///
/// `table_id_extension` to `last_section_number`, ISO/IEC 13818-1 §2.4.4.11
///
/// PAT, CAT and PMT name `table_id_extension` after what it holds:
/// `transport_stream_id` in a PAT, `program_number` in a PMT.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SectionSyntax {
    /// Field the table defines, such as `transport_stream_id`
    pub table_id_extension: u16,
    /// The 2 bits between `table_id_extension` and `version_number`,
    /// `reserved` in a `private_section` and kept as read (see [`Section`](super::Section))
    pub reserved_before_version_number: u8,
    /// Version of the table
    pub version_number: VersionNumber,
    /// Whether the table applies now rather than next
    pub current_next_indicator: bool,
    /// Number of this section in the table
    pub section_number: u8,
    /// Number of the last section of the table
    pub last_section_number: u8,
}

impl SectionSyntax {
    /// Creates the fields from their values, the reserved bits `'11'`
    #[must_use]
    pub const fn new(
        table_id_extension: u16,
        version_number: VersionNumber,
        current_next_indicator: bool,
        section_number: u8,
        last_section_number: u8,
    ) -> Self {
        Self {
            table_id_extension,
            reserved_before_version_number: RESERVED,
            version_number,
            current_next_indicator,
            section_number,
            last_section_number,
        }
    }

    /// Returns the value with the given `table_id_extension`
    #[must_use]
    pub const fn with_table_id_extension(mut self, table_id_extension: u16) -> Self {
        self.table_id_extension = table_id_extension;
        self
    }

    /// Returns the value with the given `reserved_before_version_number`
    #[must_use]
    pub const fn with_reserved_before_version_number(
        mut self,
        reserved_before_version_number: u8,
    ) -> Self {
        self.reserved_before_version_number = reserved_before_version_number;
        self
    }

    /// Returns the value with the given `version_number`
    #[must_use]
    pub const fn with_version_number(mut self, version_number: VersionNumber) -> Self {
        self.version_number = version_number;
        self
    }

    /// Returns the value with the given `current_next_indicator`
    #[must_use]
    pub const fn with_current_next_indicator(mut self, current_next_indicator: bool) -> Self {
        self.current_next_indicator = current_next_indicator;
        self
    }

    /// Returns the value with the given `section_number`
    #[must_use]
    pub const fn with_section_number(mut self, section_number: u8) -> Self {
        self.section_number = section_number;
        self
    }

    /// Returns the value with the given `last_section_number`
    #[must_use]
    pub const fn with_last_section_number(mut self, last_section_number: u8) -> Self {
        self.last_section_number = last_section_number;
        self
    }

    /// Refuses a `section_number` past `last_section_number`
    pub(super) const fn check_section_number(self) -> Result<Self, Error> {
        if self.section_number > self.last_section_number {
            return Err(Error::SectionNumber {
                section_number: self.section_number,
                last_section_number: self.last_section_number,
            });
        }

        Ok(self)
    }
}
