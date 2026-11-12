//! Conditional access table, which names the conditional access systems of a
//! transport stream and their EMM streams

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

#[cfg(feature = "alloc")]
use super::section::SectionSyntax;
use super::section::{self, Section};
use super::{TableId, VersionNumber};
use crate::descriptor::{self, DescriptorLoop};

/// The 16 `reserved` bits a CA section has where other sections have
/// `table_id_extension`
#[cfg(feature = "alloc")]
const RESERVED_TABLE_ID_EXTENSION: u16 = 0xffff;

/// Failure to convert a section into a [`CaSection`], or to write it
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The section is of another table than the conditional access table
    TableId {
        /// The `table_id` of the section
        table_id: TableId,
    },
    /// The section lacks the fields `section_syntax_indicator` `'1'`
    /// announces
    SectionSyntaxIndicator,
    /// A descriptor to write is refused
    Descriptor(descriptor::Error),
    /// The section to write is refused as a section
    Section(section::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TableId { table_id } => write!(
                formatter,
                "table_id {:#04x} is not that of a CA section",
                table_id.get()
            ),
            Self::SectionSyntaxIndicator => write!(
                formatter,
                "a CA section must have section_syntax_indicator '1'"
            ),
            Self::Descriptor(_) => write!(formatter, "a descriptor could not be written"),
            Self::Section(_) => write!(
                formatter,
                "the CA section could not be written as a section"
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Descriptor(error) => Some(error),
            Self::Section(error) => Some(error),
            Self::TableId { .. } | Self::SectionSyntaxIndicator => None,
        }
    }
}

impl From<descriptor::Error> for Error {
    fn from(error: descriptor::Error) -> Self {
        Self::Descriptor(error)
    }
}

impl From<section::Error> for Error {
    fn from(error: section::Error) -> Self {
        Self::Section(error)
    }
}

/// CA section, which names the conditional access systems of a transport
/// stream and the PIDs of their EMM streams
///
/// `CA_section`, ISO/IEC 13818-1 §2.4.4.7, Table 2-32, semantics §2.4.4.8
///
/// It is converted from a [`Section`] the demultiplexer gathered and checked,
/// with `TryFrom`: the conditional access table takes no path of its own,
/// and a crate that defines a table of its own converts a [`Section`] the
/// same way. The 16 bits other sections give to `table_id_extension` are
/// `reserved` here: they are not kept, and are written `'1'`.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::DescriptorLoop;
/// use mpeg2_systems::psi::VersionNumber;
/// use mpeg2_systems::psi::cat::CaSection;
/// use mpeg2_systems::psi::section::Section;
///
/// // A CA section of one CA_descriptor: CA_system_ID 0x0005, EMMs on PID
/// // 0x0100
/// let bytes = [
///     0x01, 0xb0, 0x0f, 0xff, 0xff, 0xc1, 0x00, 0x00, 0x09, 0x04, 0x00, 0x05, 0xe1, 0x00,
///     0xf4, 0xaf, 0xf4, 0xde,
/// ];
/// let section = Section::read(&bytes)?;
/// let cat = CaSection::try_from(&section)?;
/// let ca_descriptor = [0x09, 0x04, 0x00, 0x05, 0xe1, 0x00];
/// assert_eq!(
///     cat,
///     CaSection::new(VersionNumber::try_from(0)?)
///         .with_descriptors(DescriptorLoop::from(ca_descriptor.as_slice()))
/// );
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// cat.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[doc(alias("CAT", "CA_section", "conditional_access_section"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaSection<'input> {
    /// Version of the table
    pub version_number: VersionNumber,
    /// Whether the table applies now rather than next
    pub current_next_indicator: bool,
    /// Number of this section in the table
    pub section_number: u8,
    /// Number of the last section of the table
    pub last_section_number: u8,
    /// The descriptors of the table
    pub descriptors: DescriptorLoop<'input>,
}

impl<'input> CaSection<'input> {
    /// Creates the single section of a table that applies now, without
    /// descriptors
    #[must_use]
    pub const fn new(version_number: VersionNumber) -> Self {
        Self {
            version_number,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            descriptors: DescriptorLoop::EMPTY,
        }
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

    /// Returns the value with the given `descriptors`
    #[must_use]
    pub const fn with_descriptors(mut self, descriptors: DescriptorLoop<'input>) -> Self {
        self.descriptors = descriptors;
        self
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        section::LONG_SECTION_OVERHEAD.saturating_add(self.descriptors.as_bytes().len())
    }

    /// Appends the section to `out`, with its `CRC_32`
    ///
    /// The descriptor loop is written from the descriptors it frames. A
    /// write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Section`](Error::Section): `section_length` would be larger than
    ///   1021, checked before the loop is walked or a buffer sized, or
    ///   `section_number` is past `last_section_number`.
    /// * [`Descriptor`](Error::Descriptor): the descriptor loop ends inside a
    ///   descriptor, or a descriptor is refused by
    ///   [`RawDescriptor::write`](crate::descriptor::RawDescriptor::write).
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        section::check_encoded_len(TableId::CONDITIONAL_ACCESS, self.encoded_len())?;
        let mut body = Vec::with_capacity(self.descriptors.as_bytes().len());
        DescriptorLoop::write_entries(self.descriptors, &mut body)?;

        let mut section = Section::new(TableId::CONDITIONAL_ACCESS, &body);
        section.syntax = Some(SectionSyntax::new(
            RESERVED_TABLE_ID_EXTENSION,
            self.version_number,
            self.current_next_indicator,
            self.section_number,
            self.last_section_number,
        ));
        Ok(section.write(out)?)
    }
}

impl<'input> TryFrom<&Section<'input>> for CaSection<'input> {
    type Error = Error;

    /// Takes the fields of a CA section
    ///
    /// The `CRC_32` was checked when the section was read.
    ///
    /// # Errors
    ///
    /// * [`TableId`](Error::TableId): `table_id` is not 0x01.
    /// * [`SectionSyntaxIndicator`](Error::SectionSyntaxIndicator): the
    ///   section lacks the fields up to `last_section_number`.
    fn try_from(section: &Section<'input>) -> Result<Self, Self::Error> {
        if section.table_id != TableId::CONDITIONAL_ACCESS {
            return Err(Error::TableId {
                table_id: section.table_id,
            });
        }
        let syntax = section.syntax.ok_or(Error::SectionSyntaxIndicator)?;

        Ok(Self {
            version_number: syntax.version_number,
            current_next_indicator: syntax.current_next_indicator,
            section_number: syntax.section_number,
            last_section_number: syntax.last_section_number,
            descriptors: DescriptorLoop::from(section.body),
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{CaSection, Error};
    #[cfg(feature = "alloc")]
    use crate::descriptor::{self, DescriptorLoop};
    use crate::psi::TableId;
    #[cfg(feature = "alloc")]
    use crate::psi::VersionNumber;
    use crate::psi::section::Section;
    #[cfg(feature = "alloc")]
    use crate::psi::section::{self, SectionSyntax};

    /// A CA section of one `CA_descriptor`, its `CRC_32` computed outside
    /// this crate
    #[cfg(feature = "alloc")]
    const ONE_CA_DESCRIPTOR: [u8; 18] = [
        0x01, 0xb0, 0x0f, 0xff, 0xff, 0xc1, 0x00, 0x00, 0x09, 0x04, 0x00, 0x05, 0xe1, 0x00, 0xf4,
        0xaf, 0xf4, 0xde,
    ];

    /// `CA_descriptor` of `CA_system_ID` 0x0005 and `CA_PID` 0x0100
    #[cfg(feature = "alloc")]
    const CA_DESCRIPTOR: [u8; 6] = [0x09, 0x04, 0x00, 0x05, 0xe1, 0x00];

    #[cfg(feature = "alloc")]
    #[test]
    fn a_section_built_from_its_descriptors_is_written_with_its_reserved_bits_set() {
        let mut cat = CaSection::new(VersionNumber::try_from(0).unwrap());
        cat.descriptors = DescriptorLoop::from(CA_DESCRIPTOR.as_slice());
        let mut written = Vec::new();
        cat.write(&mut written).unwrap();

        assert_eq!(
            (written.as_slice(), cat.encoded_len()),
            (ONE_CA_DESCRIPTOR.as_slice(), ONE_CA_DESCRIPTOR.len())
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn the_bits_where_other_sections_have_table_id_extension_are_not_kept() {
        let mut section = Section::new(TableId::CONDITIONAL_ACCESS, &CA_DESCRIPTOR);
        section.syntax = Some(SectionSyntax::new(
            0x0000,
            VersionNumber::try_from(0).unwrap(),
            true,
            0,
            0,
        ));
        let mut written = Vec::new();
        CaSection::try_from(&section)
            .unwrap()
            .write(&mut written)
            .unwrap();

        assert_eq!(written, ONE_CA_DESCRIPTOR);
    }

    #[test]
    fn a_section_of_another_table_or_without_the_syntax_fields_is_not_converted() {
        let pat = Section::read(&[
            0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xf0, 0x00, 0x2a, 0xb1,
            0x04, 0xb2,
        ])
        .unwrap();
        let short = Section::new(TableId::CONDITIONAL_ACCESS, &[]);

        assert_eq!(
            [&pat, &short].map(CaSection::try_from),
            [
                Err(Error::TableId {
                    table_id: TableId::PROGRAM_ASSOCIATION
                }),
                Err(Error::SectionSyntaxIndicator),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn sections_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let version = VersionNumber::try_from(0).unwrap();
        let mut too_long = CaSection::new(version);
        too_long.descriptors = DescriptorLoop::from([0xff; 1013].as_slice());
        let mut truncated = CaSection::new(version);
        truncated.descriptors = DescriptorLoop::from([0x09, 0x04, 0x00].as_slice());
        let mut written = Vec::new();

        assert_eq!(
            [too_long.write(&mut written), truncated.write(&mut written)],
            [
                Err(Error::Section(section::Error::SectionLength {
                    section_length: 1022,
                    limit: 1021
                })),
                Err(Error::Descriptor(descriptor::Error::Length {
                    descriptor_length: 4,
                    available_bytes: 1
                })),
            ]
        );
        assert_eq!(written, []);
    }
}
