//! Section, the unit every PSI table and private table is cut into

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use super::{TableId, VersionNumber};
use crate::crc::crc_32;
use crate::field::{self, FieldReader};
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};

/// Bytes from `table_id` to `section_length`
pub(super) const PREFIX_LENGTH: usize = 3;

/// Bytes from `table_id_extension` to `last_section_number`
pub(super) const SYNTAX_LENGTH: usize = 5;

/// Bytes of `CRC_32`
pub(super) const CRC_32_LENGTH: usize = 4;

/// The `reserved` bits a new section has, and a PSI table requires
const RESERVED: u8 = 0b11;

/// Failure to read or write a section
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// `table_id` is 0xFF, which is forbidden: the byte is stuffing
    ForbiddenTableId,
    /// The bytes end before the fields that frame the section
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field failed
        error: field::Error,
    },
    /// `section_length` is larger than the table allows
    SectionLength {
        /// The `section_length` found, or that the fields add up to
        section_length: usize,
        /// The largest `section_length` the table allows
        limit: u16,
    },
    /// The bytes given are not as many as `section_length` declares
    Length {
        /// The `section_length` read
        section_length: u16,
        /// The bytes given after `section_length`
        available_bytes: usize,
    },
    /// The section does not bring the CRC decoder of Annex A to zero
    Crc32 {
        /// The `CRC_32` the section carries
        crc_32: u32,
        /// The `CRC_32` the bytes before it give
        computed: u32,
    },
    /// `section_number` is larger than `last_section_number`
    SectionNumber {
        /// The `section_number`
        section_number: u8,
        /// The `last_section_number`
        last_section_number: u8,
    },
    /// A section of a PSI table to write lacks the fields
    /// `section_syntax_indicator` `'1'` announces
    SectionSyntaxIndicator {
        /// The `table_id` of the section
        table_id: TableId,
    },
    /// A section of a PSI table to write has `private_indicator` set where
    /// the table has a `'0'` bit
    PrivateIndicator {
        /// The `table_id` of the section
        table_id: TableId,
    },
    /// A section of a PSI table to write has a `reserved` field that is not
    /// `'11'`
    Reserved {
        /// The `table_id` of the section
        table_id: TableId,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForbiddenTableId => write!(formatter, "table_id 0xff is forbidden"),
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read or written")
            }
            Self::SectionLength {
                section_length,
                limit,
            } => write!(
                formatter,
                "section_length {section_length} exceeds the {limit} the table allows"
            ),
            Self::Length {
                section_length,
                available_bytes,
            } => write!(
                formatter,
                "section_length {section_length} does not match the {available_bytes} bytes given"
            ),
            Self::Crc32 { crc_32, computed } => write!(
                formatter,
                "CRC_32 {crc_32:#010x} does not match {computed:#010x}, the CRC of the section"
            ),
            Self::SectionNumber {
                section_number,
                last_section_number,
            } => write!(
                formatter,
                "section_number {section_number} is past last_section_number {last_section_number}"
            ),
            Self::SectionSyntaxIndicator { table_id } => write!(
                formatter,
                "section_syntax_indicator of table_id {:#04x} must be '1'",
                table_id.get()
            ),
            Self::Reserved { table_id } => write!(
                formatter,
                "the reserved bits of table_id {:#04x} must be '11'",
                table_id.get()
            ),
            Self::PrivateIndicator { table_id } => write!(
                formatter,
                "the bit after section_syntax_indicator of table_id {:#04x} must be '0'",
                table_id.get()
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::ForbiddenTableId
            | Self::SectionLength { .. }
            | Self::Length { .. }
            | Self::Crc32 { .. }
            | Self::SectionNumber { .. }
            | Self::SectionSyntaxIndicator { .. }
            | Self::PrivateIndicator { .. }
            | Self::Reserved { .. } => None,
        }
    }
}

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
    /// `reserved` in a `private_section` and kept as read (see [`Section`])
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

    /// Refuses a `section_number` past `last_section_number`
    const fn check_section_number(self) -> Result<Self, Error> {
        if self.section_number > self.last_section_number {
            return Err(Error::SectionNumber {
                section_number: self.section_number,
                last_section_number: self.last_section_number,
            });
        }

        Ok(self)
    }
}

/// Section, the unit a table is cut into for carriage
///
/// `private_section`, ISO/IEC 13818-1 §2.4.4.11
///
/// The fields shared by every section: those of `private_section`, whose
/// first fields PAT, CAT and PMT share too (§2.4.4.4, §2.4.4.7, §2.4.4.9).
/// `section_length` follows from the fields and the body, and
/// `section_syntax_indicator` from whether [`syntax`](Self::syntax) is
/// present. `CRC_32` is checked when the section is read and computed when
/// it is written, so it is not a field. A table the crate does not
/// interpret is kept whole as its body. The crate does not gather a section
/// from Transport Packets; the demultiplexer does.
///
/// The 2 bits before `section_length`, and the 2 before `version_number`,
/// are `reserved` in a `private_section`, but `Metadata_section` gives them
/// meanings of its own (§2.12.6), so the bits read are kept and written
/// back. A new section has them `'11'`, which PAT, CAT, PMT and the
/// transport stream description table require.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::psi::section::{Error, Section};
/// use mpeg2_systems::psi::TableId;
///
/// // The program association section of a stream of one program
/// let bytes = [
///     0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xf0, 0x00, 0x2a, 0xb1,
///     0x04, 0xb2,
/// ];
/// let section = Section::read(&bytes)?;
/// assert_eq!(section.table_id, TableId::PROGRAM_ASSOCIATION);
/// assert_eq!(section.body, [0x00, 0x01, 0xf0, 0x00]);
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// section.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Error>(())
/// ```
#[doc(alias("private_section", "PSI section"))]
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Section<'input> {
    /// The table the section belongs to
    pub table_id: TableId,
    /// The bit after `section_syntax_indicator`: `private_indicator` in a
    /// private section, `'0'` in PAT, CAT and PMT
    pub private_indicator: bool,
    /// The 2 bits between `private_indicator` and `section_length`, kept as
    /// read
    pub reserved_before_section_length: u8,
    /// `table_id_extension` to `last_section_number`, present when
    /// `section_syntax_indicator` is `'1'`
    pub syntax: Option<SectionSyntax>,
    /// The bytes after the header and before `CRC_32`
    pub body: &'input [u8],
}

impl<'input> Section<'input> {
    /// Creates a section of `table_id` carrying `body`, without the fields
    /// `section_syntax_indicator` `'1'` announces
    #[must_use]
    pub const fn new(table_id: TableId, body: &'input [u8]) -> Self {
        Self {
            table_id,
            private_indicator: false,
            reserved_before_section_length: RESERVED,
            syntax: None,
            body,
        }
    }

    /// Returns whether the section has the fields from
    /// `table_id_extension` to `last_section_number`, and a `CRC_32`
    #[must_use]
    pub const fn section_syntax_indicator(&self) -> bool {
        self.syntax.is_some()
    }

    /// Returns the `section_length` the fields and the body add up to
    #[must_use]
    pub fn section_length(&self) -> usize {
        let syntax = if self.syntax.is_some() {
            SYNTAX_LENGTH.saturating_add(CRC_32_LENGTH)
        } else {
            0
        };

        syntax.saturating_add(self.body.len())
    }

    /// Reads a section from `bytes`, which hold it whole and nothing more
    ///
    /// # Errors
    ///
    /// * [`ForbiddenTableId`](Error::ForbiddenTableId): `table_id` is 0xFF.
    /// * [`Field`](Error::Field): `bytes` end before `section_length`, or
    ///   before `last_section_number` and `CRC_32`.
    /// * [`SectionLength`](Error::SectionLength): `section_length` is larger
    ///   than [`TableId::max_section_length`].
    /// * [`Length`](Error::Length): `section_length` does not match `bytes`.
    /// * [`Crc32`](Error::Crc32): the section fails its `CRC_32`.
    /// * [`SectionNumber`](Error::SectionNumber): `section_number` is past
    ///   `last_section_number`.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        let mut reader = FieldReader::new(bytes);
        let mut prefix = || {
            Ok((
                TableId(reader.read_u8::<8>()?),
                reader.read_bool()?,
                reader.read_bool()?,
                reader.read_u8::<2>()?,
                reader.read_u16::<12>()?,
            ))
        };
        let (table_id, section_syntax_indicator, private_indicator, reserved, section_length) =
            prefix().map_err(|error| Error::Field {
                field: "section_length",
                error,
            })?;
        if table_id == TableId::FORBIDDEN {
            return Err(Error::ForbiddenTableId);
        }
        let limit = table_id.max_section_length();
        if section_length > limit {
            return Err(Error::SectionLength {
                section_length: usize::from(section_length),
                limit,
            });
        }
        let rest = bytes.get(PREFIX_LENGTH..).unwrap_or_default();
        if rest.len() != usize::from(section_length) {
            return Err(Error::Length {
                section_length,
                available_bytes: rest.len(),
            });
        }

        let mut section = Self::new(table_id, rest);
        section.private_indicator = private_indicator;
        section.reserved_before_section_length = reserved;
        if !section_syntax_indicator {
            return Ok(section);
        }

        let mut syntax = || {
            let table_id_extension = reader.read_u16::<16>()?;
            let reserved = reader.read_u8::<2>()?;
            let mut syntax = SectionSyntax::new(
                table_id_extension,
                VersionNumber(reader.read_u8::<5>()?),
                reader.read_bool()?,
                reader.read_u8::<8>()?,
                reader.read_u8::<8>()?,
            );
            syntax.reserved_before_version_number = reserved;
            Ok(syntax)
        };
        let syntax = syntax().map_err(|error| Error::Field {
            field: "last_section_number",
            error,
        })?;
        let remaining_bits = reader.remaining_bits();
        let body_and_crc = reader
            .read_bytes(reader.remaining_bytes())
            .unwrap_or_default();
        let (body, crc_field) =
            body_and_crc
                .split_last_chunk::<CRC_32_LENGTH>()
                .ok_or(Error::Field {
                    field: "CRC_32",
                    error: field::Error::Truncated {
                        required_bits: 32,
                        remaining_bits,
                    },
                })?;
        if crc_32(bytes) != 0 {
            let before_crc = bytes
                .get(..bytes.len().saturating_sub(CRC_32_LENGTH))
                .unwrap_or_default();
            return Err(Error::Crc32 {
                crc_32: u32::from_be_bytes(*crc_field),
                computed: crc_32(before_crc),
            });
        }
        section.syntax = Some(syntax.check_section_number()?);
        section.body = body;

        Ok(section)
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        PREFIX_LENGTH.saturating_add(self.section_length())
    }

    /// Appends the section to `out`, with its `CRC_32` when it has the
    /// fields `section_syntax_indicator` `'1'` announces
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`ForbiddenTableId`](Error::ForbiddenTableId): `table_id` is 0xFF.
    /// * [`SectionSyntaxIndicator`](Error::SectionSyntaxIndicator): a section
    ///   of a PSI table without [`syntax`](Self::syntax).
    /// * [`PrivateIndicator`](Error::PrivateIndicator): a section of a PSI
    ///   table with `private_indicator` set.
    /// * [`Reserved`](Error::Reserved): a section of a PSI table with
    ///   `reserved` bits other than `'11'`.
    /// * [`Field`](Error::Field): a `reserved` field wider than 2 bits.
    /// * [`SectionLength`](Error::SectionLength): `section_length` would be
    ///   larger than [`TableId::max_section_length`].
    /// * [`SectionNumber`](Error::SectionNumber): `section_number` is past
    ///   `last_section_number`.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let table_id = self.table_id;
        if table_id == TableId::FORBIDDEN {
            return Err(Error::ForbiddenTableId);
        }
        if table_id.is_psi_table() {
            let Some(syntax) = self.syntax else {
                return Err(Error::SectionSyntaxIndicator { table_id });
            };
            if self.private_indicator {
                return Err(Error::PrivateIndicator { table_id });
            }
            if self.reserved_before_section_length != RESERVED
                || syntax.reserved_before_version_number != RESERVED
            {
                return Err(Error::Reserved { table_id });
            }
        }
        let section_length = self.section_length();
        let limit = table_id.max_section_length();
        let length = u16::try_from(section_length)
            .ok()
            .filter(|length| *length <= limit)
            .ok_or(Error::SectionLength {
                section_length,
                limit,
            })?;
        let syntax = self
            .syntax
            .map(SectionSyntax::check_section_number)
            .transpose()?;

        write_or_truncate(out, |out| {
            let start = out.len();
            let mut writer = FieldWriter::new(out);
            // Why not an error per field: every value fits its field's width.
            (|| {
                writer.write_bits::<8>(table_id.get())?;
                writer.write_bool(syntax.is_some())?;
                writer.write_bool(self.private_indicator)?;
                writer.write_bits::<2>(self.reserved_before_section_length)?;
                writer.write_bits::<12>(length)?;
                if let Some(syntax) = syntax {
                    writer.write_bits::<16>(syntax.table_id_extension)?;
                    writer.write_bits::<2>(syntax.reserved_before_version_number)?;
                    writer.write_bits::<5>(syntax.version_number.get())?;
                    writer.write_bool(syntax.current_next_indicator)?;
                    writer.write_bits::<8>(syntax.section_number)?;
                    writer.write_bits::<8>(syntax.last_section_number)?;
                }
                writer.write_bytes(self.body)?;
                writer.finish()
            })()
            .map_err(|error| Error::Field {
                field: "section header",
                error,
            })?;
            if syntax.is_some() {
                let crc = crc_32(out.get(start..).unwrap_or_default());
                out.extend_from_slice(&crc.to_be_bytes());
            }

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{Error, Section, SectionSyntax};
    use crate::field;
    use crate::psi::{TableId, VersionNumber};

    /// A user private section with every field `section_syntax_indicator`
    /// `'1'` announces, its `CRC_32` computed outside this crate
    const LONG_PRIVATE_SECTION: [u8; 14] = [
        0x80, 0xf0, 0x0b, 0x12, 0x34, 0xc7, 0x01, 0x02, 0xab, 0xcd, 0x8d, 0x77, 0x90, 0x8e,
    ];

    fn long_private_section() -> Section<'static> {
        let mut section = Section::new(TableId::from(0x80), &[0xab, 0xcd]);
        section.private_indicator = true;
        section.syntax = Some(SectionSyntax::new(
            0x1234,
            VersionNumber::try_from(3).unwrap(),
            true,
            1,
            2,
        ));
        section
    }

    #[test]
    fn a_section_with_the_syntax_fields_is_read_and_its_crc_32_checked() {
        assert_eq!(
            Section::read(&LONG_PRIVATE_SECTION),
            Ok(long_private_section())
        );
    }

    #[test]
    fn a_short_private_section_has_every_byte_after_its_length_as_its_body() {
        let mut expected = Section::new(TableId::from(0x80), &[0x01, 0x02, 0x03]);
        expected.private_indicator = true;

        assert_eq!(
            Section::read(&[0x80, 0x70, 0x03, 0x01, 0x02, 0x03]),
            Ok(expected)
        );
    }

    #[test]
    fn a_section_whose_crc_32_fails_is_refused() {
        assert_eq!(
            Section::read(&[
                0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x00, 0x2a, 0xb1,
                0x04, 0xb2,
            ]),
            Err(Error::Crc32 {
                crc_32: 0x2ab1_04b2,
                computed: 0x2b69_a835
            })
        );
    }

    #[test]
    fn malformed_sections_are_each_refused_for_what_is_wrong() {
        assert_eq!(
            [
                Section::read(&[0xff, 0xff, 0xff]),
                Section::read(&[0x00, 0xb3, 0xfe]),
                Section::read(&[0x80, 0x70, 0x03, 0x01]),
                Section::read(&[0x80, 0xf0, 0x05, 0x12, 0x34, 0xc7, 0x00, 0x00]),
                Section::read(&[
                    0x80, 0xf0, 0x0b, 0x12, 0x34, 0xc7, 0x02, 0x01, 0xab, 0xcd, 0xef, 0x69, 0xd7,
                    0x69,
                ]),
            ],
            [
                Err(Error::ForbiddenTableId),
                Err(Error::SectionLength {
                    section_length: 1022,
                    limit: 1021
                }),
                Err(Error::Length {
                    section_length: 3,
                    available_bytes: 1
                }),
                Err(Error::Field {
                    field: "CRC_32",
                    error: field::Error::Truncated {
                        required_bits: 32,
                        remaining_bits: 0
                    }
                }),
                Err(Error::SectionNumber {
                    section_number: 2,
                    last_section_number: 1
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn the_bits_a_metadata_section_gives_meaning_to_are_written_back_as_read() {
        let bytes = [
            0x06, 0x80, 0x0a, 0x01, 0xff, 0x41, 0x01, 0x01, 0xaa, 0x11, 0x10, 0x9b, 0x1f,
        ];
        let mut written = Vec::new();
        Section::read(&bytes).unwrap().write(&mut written).unwrap();

        assert_eq!(written, bytes);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_section_with_the_syntax_fields_is_written_with_its_crc_32() {
        let mut written = Vec::new();
        long_private_section().write(&mut written).unwrap();

        assert_eq!(written, LONG_PRIVATE_SECTION);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn sections_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let long_body = [0; 4094];
        let long_private = Section::new(TableId::from(0x80), &long_body);
        let mut long_pat = long_private_section();
        long_pat.table_id = TableId::PROGRAM_ASSOCIATION;
        long_pat.private_indicator = false;
        long_pat.body = &long_body[..1013];
        let short_pat = Section::new(TableId::PROGRAM_ASSOCIATION, &[]);
        let mut private_pat = long_private_section();
        private_pat.table_id = TableId::PROGRAM_ASSOCIATION;
        let mut reversed = long_private_section();
        reversed.syntax = Some(SectionSyntax::new(
            0,
            VersionNumber::try_from(0).unwrap(),
            true,
            2,
            1,
        ));
        let forbidden = Section::new(TableId::FORBIDDEN, &[]);
        let mut cleared_reserved = long_private_section();
        cleared_reserved.table_id = TableId::TS_PROGRAM_MAP;
        cleared_reserved.private_indicator = false;
        cleared_reserved.reserved_before_section_length = 0b00;
        let mut cleared_syntax_reserved = long_private_section();
        cleared_syntax_reserved.table_id = TableId::TS_PROGRAM_MAP;
        cleared_syntax_reserved.private_indicator = false;
        if let Some(syntax) = &mut cleared_syntax_reserved.syntax {
            syntax.reserved_before_version_number = 0b01;
        }
        let mut written = Vec::new();

        assert_eq!(
            [
                long_private.write(&mut written),
                long_pat.write(&mut written),
                short_pat.write(&mut written),
                private_pat.write(&mut written),
                reversed.write(&mut written),
                forbidden.write(&mut written),
                cleared_reserved.write(&mut written),
                cleared_syntax_reserved.write(&mut written),
            ],
            [
                Err(Error::SectionLength {
                    section_length: 4094,
                    limit: 4093
                }),
                Err(Error::SectionLength {
                    section_length: 1022,
                    limit: 1021
                }),
                Err(Error::SectionSyntaxIndicator {
                    table_id: TableId::PROGRAM_ASSOCIATION
                }),
                Err(Error::PrivateIndicator {
                    table_id: TableId::PROGRAM_ASSOCIATION
                }),
                Err(Error::SectionNumber {
                    section_number: 2,
                    last_section_number: 1
                }),
                Err(Error::ForbiddenTableId),
                Err(Error::Reserved {
                    table_id: TableId::TS_PROGRAM_MAP
                }),
                Err(Error::Reserved {
                    table_id: TableId::TS_PROGRAM_MAP
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
