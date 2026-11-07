//! Program map table, which lists the program elements of each program

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

#[cfg(feature = "alloc")]
use super::section::SectionSyntax;
use super::section::{self, Section};
use super::{TableId, VersionNumber};
#[cfg(feature = "alloc")]
use crate::field::FieldWriter;
use crate::field::{self, FieldReader};
use crate::ts::Pid;

pub mod elementary_stream;

pub use elementary_stream::{ElementaryStream, ElementaryStreamLoop};

/// Largest `program_info_length` and `ES_info_length`, whose first two bits
/// are `'00'`
#[cfg(feature = "alloc")]
const MAX_INFO_LENGTH: usize = 0x3ff;

/// Bytes from `PCR_PID` to `program_info_length`
#[cfg(feature = "alloc")]
const PROGRAM_HEADER_LENGTH: usize = 4;

/// Failure to convert a section into a [`TsProgramMapSection`], to read an
/// entry of its elementary stream loop, or to write it
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The section is of another table than the program map table
    TableId {
        /// The `table_id` of the section
        table_id: TableId,
    },
    /// The section lacks the fields `section_syntax_indicator` `'1'`
    /// announces
    SectionSyntaxIndicator,
    /// The section, or an entry of its elementary stream loop, ends inside a
    /// field or the descriptors its length announces; or a field cannot be
    /// written
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field ran past the end
        error: field::Error,
    },
    /// The section to write has a `section_number` or `last_section_number`
    /// other than 0
    SectionNumber {
        /// The `section_number`
        section_number: u8,
        /// The `last_section_number`
        last_section_number: u8,
    },
    /// The `PCR_PID` to write is of a PID Table 2-3 does not let carry a PCR
    PcrPid {
        /// The PID given
        pid: Pid,
    },
    /// The descriptors to write are more than the 1023 bytes a length field
    /// whose first two bits are `'00'` counts
    InfoLength {
        /// The name of the length field in the syntax table
        field: &'static str,
        /// The bytes of descriptors given
        length: usize,
    },
    /// The section to write is refused as a section
    Section(section::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TableId { table_id } => write!(
                formatter,
                "table_id {:#04x} is not that of a TS program map section",
                table_id.get()
            ),
            Self::SectionSyntaxIndicator => write!(
                formatter,
                "a TS program map section must have section_syntax_indicator '1'"
            ),
            Self::Field { field, error } => write!(formatter, "{field}: {error}"),
            Self::SectionNumber {
                section_number,
                last_section_number,
            } => write!(
                formatter,
                "section_number {section_number} and last_section_number \
                 {last_section_number} of a TS program map section must be 0"
            ),
            Self::PcrPid { pid } => write!(
                formatter,
                "PCR_PID {:#06x} is of a PID that cannot carry a PCR",
                pid.get()
            ),
            Self::InfoLength { field, length } => write!(
                formatter,
                "{field} {length} exceeds 1023, which its first two bits '00' allow"
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
            | Self::SectionNumber { .. }
            | Self::PcrPid { .. }
            | Self::InfoLength { .. } => None,
        }
    }
}

impl From<section::Error> for Error {
    fn from(error: section::Error) -> Self {
        Self::Section(error)
    }
}

/// Refuses descriptors longer than a length field whose first two bits are
/// `'00'` counts
#[cfg(feature = "alloc")]
const fn check_info_length(field: &'static str, descriptors: &[u8]) -> Result<(), Error> {
    if descriptors.len() > MAX_INFO_LENGTH {
        return Err(Error::InfoLength {
            field,
            length: descriptors.len(),
        });
    }

    Ok(())
}

/// TS program map section, which lists the program elements of a program
///
/// `TS_program_map_section`, ISO/IEC 13818-1 §2.4.4.9, Table 2-33,
/// semantics §2.4.4.10
///
/// It is converted from a [`Section`] the demultiplexer gathered and checked,
/// with `TryFrom`: the program map table takes no path of its own, and a
/// crate that defines a table of its own converts a [`Section`] the same
/// way. `program_number` is the section's `table_id_extension`. A program is
/// defined in a single section, so `section_number` and
/// `last_section_number` are 0 in every section written.
///
/// The descriptors are kept as bytes. `PCR_PID` 0x1FFF, a program without a
/// PCR, is `None`.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::psi::pmt::TsProgramMapSection;
/// use mpeg2_systems::psi::section::Section;
/// use mpeg2_systems::stream_type::StreamType;
///
/// // A program of one MPEG-2 video stream on PID 0x0100, which carries the
/// // PCR
/// let bytes = [
///     0x02, 0xb0, 0x12, 0x00, 0x01, 0xc1, 0x00, 0x00, 0xe1, 0x00, 0xf0, 0x00, 0x02, 0xe1,
///     0x00, 0xf0, 0x00, 0x9e, 0x8b, 0x23, 0xd1,
/// ];
/// let section = Section::read(&bytes)?;
/// let pmt = TsProgramMapSection::try_from(&section)?;
/// assert_eq!(pmt.program_number, 1);
/// assert_eq!(pmt.pcr_pid.map(|pid| pid.get()), Some(0x0100));
///
/// let stream = pmt.elementary_streams.iter().next().unwrap()?;
/// assert_eq!(stream.stream_type, StreamType::ISO_IEC_13818_2_VIDEO);
/// assert_eq!(stream.elementary_pid.get(), 0x0100);
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// pmt.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[doc(alias("PMT", "TS_program_map_section"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TsProgramMapSection<'input> {
    /// The program the section defines
    pub program_number: u16,
    /// Version of the program definition
    pub version_number: VersionNumber,
    /// Whether the program definition applies now rather than next
    pub current_next_indicator: bool,
    /// Number of this section, 0
    pub section_number: u8,
    /// Number of the last section, 0
    pub last_section_number: u8,
    /// PID of the Transport Packets carrying the PCR of the program, `None`
    /// for 0x1FFF
    pub pcr_pid: Option<Pid>,
    /// The descriptors of the program, as bytes
    pub program_info: &'input [u8],
    /// The program elements and their PIDs
    pub elementary_streams: ElementaryStreamLoop<'input>,
}

impl<'input> TsProgramMapSection<'input> {
    /// Creates a program definition that applies now, without a PCR,
    /// descriptors or program elements
    #[must_use]
    pub const fn new(program_number: u16, version_number: VersionNumber) -> Self {
        Self {
            program_number,
            version_number,
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            pcr_pid: None,
            program_info: &[],
            elementary_streams: ElementaryStreamLoop(&[]),
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        super::LONG_SECTION_OVERHEAD
            .saturating_add(4)
            .saturating_add(self.program_info.len())
            .saturating_add(self.elementary_streams.0.len())
    }

    /// Appends the section to `out`, with its `CRC_32`
    ///
    /// The elementary stream loop is written as it is held, after every
    /// entry is read and checked. A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`SectionNumber`](Error::SectionNumber): `section_number` or
    ///   `last_section_number` is not 0.
    /// * [`PcrPid`](Error::PcrPid): `pcr_pid` is of a PID other than 0x0000,
    ///   0x0001 and 0x0010 to 0x1FFE, which Table 2-3 lets carry a PCR.
    /// * [`InfoLength`](Error::InfoLength): descriptors of more than 1023
    ///   bytes, for the program or a program element.
    /// * [`Field`](Error::Field): the elementary stream loop ends inside an
    ///   entry.
    /// * [`Section`](Error::Section): `section_length` would be larger than
    ///   1021.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.section_number != 0 || self.last_section_number != 0 {
            return Err(Error::SectionNumber {
                section_number: self.section_number,
                last_section_number: self.last_section_number,
            });
        }
        if let Some(pid) = self.pcr_pid
            && !matches!(pid.get(), 0x0000 | 0x0001 | 0x0010..=0x1ffe)
        {
            return Err(Error::PcrPid { pid });
        }
        check_info_length("program_info_length", self.program_info)?;
        for stream in self.elementary_streams {
            check_info_length("ES_info_length", stream?.es_info)?;
        }

        let mut body = Vec::with_capacity(
            PROGRAM_HEADER_LENGTH
                .saturating_add(self.program_info.len())
                .saturating_add(self.elementary_streams.0.len()),
        );
        let mut writer = FieldWriter::new(&mut body);
        // Why not an error per field: every value fits its field's width.
        (|| {
            writer.write_bits::<3>(0b111_u8)?;
            writer.write_bits::<13>(self.pcr_pid.unwrap_or(Pid::NULL).get())?;
            writer.write_bits::<4>(0b1111_u8)?;
            writer.write_bits::<12>(u64::try_from(self.program_info.len()).unwrap_or(u64::MAX))?;
            writer.write_bytes(self.program_info)?;
            writer.write_bytes(self.elementary_streams.0)?;
            writer.finish()
        })()
        .map_err(|error| Error::Field {
            field: "program header",
            error,
        })?;

        let mut section = Section::new(TableId::TS_PROGRAM_MAP, &body);
        section.syntax = Some(SectionSyntax::new(
            self.program_number,
            self.version_number,
            self.current_next_indicator,
            self.section_number,
            self.last_section_number,
        ));
        Ok(section.write(out)?)
    }
}

impl<'input> TryFrom<&Section<'input>> for TsProgramMapSection<'input> {
    type Error = Error;

    /// Takes the fields of a TS program map section
    ///
    /// The `CRC_32` was checked when the section was read. The elementary
    /// stream loop is not read until it is walked. A `section_number` other
    /// than 0 is kept as read.
    ///
    /// # Errors
    ///
    /// * [`TableId`](Error::TableId): `table_id` is not 0x02.
    /// * [`SectionSyntaxIndicator`](Error::SectionSyntaxIndicator): the
    ///   section lacks `program_number` to `last_section_number`.
    /// * [`Field`](Error::Field): the section ends before
    ///   `program_info_length`, or inside the descriptors it announces.
    fn try_from(section: &Section<'input>) -> Result<Self, Self::Error> {
        if section.table_id != TableId::TS_PROGRAM_MAP {
            return Err(Error::TableId {
                table_id: section.table_id,
            });
        }
        let syntax = section.syntax.ok_or(Error::SectionSyntaxIndicator)?;

        let mut reader = FieldReader::new(section.body);
        let pcr_pid = reader
            .read_bits::<3>()
            .and_then(|_| reader.read_u16::<13>())
            .map_err(|error| Error::Field {
                field: "PCR_PID",
                error,
            })?;
        let program_info = reader
            .read_bits::<4>()
            .and_then(|_| reader.read_u16::<12>())
            .and_then(|length| reader.read_bytes(usize::from(length)))
            .map_err(|error| Error::Field {
                field: "program_info_length",
                error,
            })?;

        Ok(Self {
            program_number: syntax.table_id_extension,
            version_number: syntax.version_number,
            current_next_indicator: syntax.current_next_indicator,
            section_number: syntax.section_number,
            last_section_number: syntax.last_section_number,
            pcr_pid: Some(Pid(pcr_pid)).filter(|pid| *pid != Pid::NULL),
            program_info,
            // Why not an error: the fields before end on a byte boundary.
            elementary_streams: ElementaryStreamLoop::from(
                reader
                    .read_bytes(reader.remaining_bytes())
                    .unwrap_or_default(),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;
    use core::array;

    #[cfg(feature = "alloc")]
    use super::ElementaryStreamLoop;
    use super::{ElementaryStream, Error, TsProgramMapSection};
    use crate::field;
    #[cfg(feature = "alloc")]
    use crate::psi::section;
    use crate::psi::section::{Section, SectionSyntax};
    use crate::psi::{TableId, VersionNumber};
    use crate::stream_type::StreamType;
    use crate::ts::Pid;

    /// A TS program map section with descriptors for the program and for
    /// one of its two program elements, its `CRC_32` computed outside this
    /// crate
    const WITH_DESCRIPTORS: [u8; 38] = [
        0x02, 0xb0, 0x23, 0x00, 0x64, 0xc5, 0x00, 0x00, 0xe1, 0x00, 0xf0, 0x06, 0x05, 0x04, 0x48,
        0x44, 0x4d, 0x56, 0x1b, 0xe1, 0x00, 0xf0, 0x06, 0x0a, 0x04, 0x6a, 0x70, 0x6e, 0x00, 0x0f,
        0xe1, 0x01, 0xf0, 0x00, 0x99, 0x56, 0xf0, 0x85,
    ];

    /// `registration_descriptor` of `format_identifier` "HDMV"
    const PROGRAM_INFO: [u8; 6] = [0x05, 0x04, 0x48, 0x44, 0x4d, 0x56];

    /// `ISO_639_language_descriptor` of "jpn"
    const ES_INFO: [u8; 6] = [0x0a, 0x04, 0x6a, 0x70, 0x6e, 0x00];

    fn pid(value: u16) -> Pid {
        Pid::try_from(value).unwrap()
    }

    fn streams() -> [ElementaryStream<'static>; 2] {
        let mut video = ElementaryStream::new(StreamType::AVC_VIDEO, pid(0x0100));
        video.es_info = &ES_INFO;
        [
            video,
            ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(0x0101)),
        ]
    }

    /// A section of `body` with the syntax fields of program 1
    fn section_of(body: &[u8]) -> Section<'_> {
        let mut section = Section::new(TableId::TS_PROGRAM_MAP, body);
        section.syntax = Some(SectionSyntax::new(
            1,
            VersionNumber::try_from(0).unwrap(),
            true,
            0,
            0,
        ));
        section
    }

    #[test]
    fn the_descriptors_of_the_program_and_of_each_element_are_kept_as_bytes() {
        let section = Section::read(&WITH_DESCRIPTORS).unwrap();
        let pmt = TsProgramMapSection::try_from(&section).unwrap();
        let mut entries = pmt.elementary_streams.iter();
        let [video, audio] = streams();

        assert_eq!(
            (
                pmt.program_number,
                pmt.version_number.get(),
                pmt.pcr_pid,
                pmt.program_info,
                array::from_fn(|_| entries.next())
            ),
            (
                100,
                2,
                Some(pid(0x0100)),
                PROGRAM_INFO.as_slice(),
                [Some(Ok(video)), Some(Ok(audio)), None]
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_section_built_from_its_elements_is_written_with_its_crc_32() {
        let mut entries = Vec::new();
        ElementaryStreamLoop::write_from(streams(), &mut entries).unwrap();
        let mut pmt = TsProgramMapSection::new(100, VersionNumber::try_from(2).unwrap());
        pmt.pcr_pid = Some(pid(0x0100));
        pmt.program_info = &PROGRAM_INFO;
        pmt.elementary_streams = ElementaryStreamLoop::from(entries.as_slice());
        let mut written = Vec::new();
        pmt.write(&mut written).unwrap();

        assert_eq!(
            (written.as_slice(), pmt.encoded_len()),
            (WITH_DESCRIPTORS.as_slice(), WITH_DESCRIPTORS.len())
        );
    }

    #[test]
    fn an_element_whose_descriptors_run_past_the_loop_ends_it_after_the_elements_before() {
        let body = [
            0xff, 0xff, 0xf0, 0x00, 0x02, 0xe1, 0x00, 0xf0, 0x00, 0x0f, 0xe1, 0x01, 0xf0, 0x05,
            0x0a, 0x0b,
        ];
        let section = section_of(&body);
        let pmt = TsProgramMapSection::try_from(&section).unwrap();
        let mut entries = pmt.elementary_streams.iter();

        assert_eq!(
            (pmt.pcr_pid, array::from_fn(|_| entries.next())),
            (
                None,
                [
                    Some(Ok(ElementaryStream::new(
                        StreamType::ISO_IEC_13818_2_VIDEO,
                        pid(0x0100)
                    ))),
                    Some(Err(Error::Field {
                        field: "ES_info_length",
                        error: field::Error::Truncated {
                            required_bits: 40,
                            remaining_bits: 16
                        }
                    })),
                    None,
                ]
            )
        );
    }

    #[test]
    fn a_section_that_is_not_a_whole_program_definition_is_not_converted() {
        let mut cat = section_of(&[]);
        cat.table_id = TableId::CONDITIONAL_ACCESS;
        let short = Section::new(TableId::TS_PROGRAM_MAP, &[]);
        let no_pcr_pid = section_of(&[0xe1]);
        let past_program_info = section_of(&[0xe1, 0x00, 0xf0, 0x05, 0x0a]);

        assert_eq!(
            [&cat, &short, &no_pcr_pid, &past_program_info].map(TsProgramMapSection::try_from),
            [
                Err(Error::TableId {
                    table_id: TableId::CONDITIONAL_ACCESS
                }),
                Err(Error::SectionSyntaxIndicator),
                Err(Error::Field {
                    field: "PCR_PID",
                    error: field::Error::Truncated {
                        required_bits: 13,
                        remaining_bits: 5
                    }
                }),
                Err(Error::Field {
                    field: "program_info_length",
                    error: field::Error::Truncated {
                        required_bits: 40,
                        remaining_bits: 8
                    }
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn sections_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let version = VersionNumber::try_from(0).unwrap();
        let descriptors = [0xff; 1024];
        let mut second = TsProgramMapSection::new(1, version);
        second.section_number = 1;
        second.last_section_number = 1;
        let mut reserved_pcr_pid = TsProgramMapSection::new(1, version);
        reserved_pcr_pid.pcr_pid = Some(pid(0x0002));
        let mut null_pcr_pid = TsProgramMapSection::new(1, version);
        null_pcr_pid.pcr_pid = Some(Pid::NULL);
        let mut long_program_info = TsProgramMapSection::new(1, version);
        long_program_info.program_info = &descriptors;
        let mut long_es_info = ElementaryStream::new(StreamType::AVC_VIDEO, pid(0x0100));
        long_es_info.es_info = &descriptors;
        let long_es_info = ElementaryStreamLoop::write_from([long_es_info], &mut Vec::new());
        let mut truncated = TsProgramMapSection::new(1, version);
        truncated.elementary_streams = ElementaryStreamLoop::from([0x02, 0xe1].as_slice());
        let mut too_long = TsProgramMapSection::new(1, version);
        too_long.program_info = &descriptors[..1023];
        let mut written = Vec::new();

        assert_eq!(
            [
                second.write(&mut written),
                reserved_pcr_pid.write(&mut written),
                null_pcr_pid.write(&mut written),
                long_program_info.write(&mut written),
                long_es_info,
                truncated.write(&mut written),
                too_long.write(&mut written),
            ],
            [
                Err(Error::SectionNumber {
                    section_number: 1,
                    last_section_number: 1
                }),
                Err(Error::PcrPid { pid: pid(0x0002) }),
                Err(Error::PcrPid { pid: Pid::NULL }),
                Err(Error::InfoLength {
                    field: "program_info_length",
                    length: 1024
                }),
                Err(Error::InfoLength {
                    field: "ES_info_length",
                    length: 1024
                }),
                Err(Error::Field {
                    field: "ES_info_length",
                    error: field::Error::Truncated {
                        required_bits: 13,
                        remaining_bits: 5
                    }
                }),
                Err(Error::Section(section::Error::SectionLength {
                    section_length: 1036,
                    limit: 1021
                })),
            ]
        );
        assert_eq!(written, []);
    }
}
