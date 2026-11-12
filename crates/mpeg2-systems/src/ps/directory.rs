//! Program stream directory: where the access units of a Program Stream
//! start, for random access

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use crate::field::{self, FieldReader};
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::pes::{PesPacket, StreamId};

mod entry;

use entry::ENTRY_LENGTH;
pub use entry::{
    CodingParametersIndicator, DirectoryEntry, DirectoryEntryIter, DirectoryEntryLoop,
};

/// `packet_start_code_prefix` and `directory_stream_id`
#[cfg(feature = "alloc")]
const DIRECTORY_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xff];

/// Bytes from `packet_start_code_prefix` to `PES_packet_length`
#[cfg(feature = "alloc")]
const PREFIX_LENGTH: usize = 6;

/// Bytes from `number_of_access_units` to the last `marker_bit` of
/// `next_directory_offset`
#[cfg(feature = "alloc")]
const HEADER_LENGTH: usize = 14;

/// Failure to read or write a program stream directory, or an entry of its
/// access unit loop
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The PES packet is not of `directory_stream_id` 0xFF
    DirectoryStreamId {
        /// The `stream_id` of the PES packet
        stream_id: StreamId,
    },
    /// A field runs past the end of the packet, or does not fit its width
    /// when written
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field failed
        error: field::Error,
    },
    /// The bytes after `next_directory_offset` are not those of as many
    /// entries as `number_of_access_units` counts
    NumberOfAccessUnits {
        /// The `number_of_access_units` read
        number_of_access_units: u16,
        /// The bytes after `next_directory_offset`
        available_bytes: usize,
    },
    /// An entry to write has a `packet_stream_id_extension` where its
    /// `packet_stream_id` is not 0xFD, or none where it is
    PacketStreamIdExtension {
        /// The `packet_stream_id` of the entry
        packet_stream_id: StreamId,
    },
    /// A directory to write is longer than `PES_packet_length` can state
    PesPacketLength {
        /// The bytes after `PES_packet_length`
        length: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DirectoryStreamId { stream_id } => write!(
                formatter,
                "stream_id {:#04x} is not directory_stream_id 0xff",
                stream_id.get()
            ),
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read or written")
            }
            Self::NumberOfAccessUnits {
                number_of_access_units,
                available_bytes,
            } => write!(
                formatter,
                "number_of_access_units {number_of_access_units} does not match the \
                 {available_bytes} bytes of entries"
            ),
            Self::PacketStreamIdExtension { packet_stream_id } => write!(
                formatter,
                "packet_stream_id {:#04x} and the presence of packet_stream_id_extension \
                 disagree",
                packet_stream_id.get()
            ),
            Self::PesPacketLength { length } => write!(
                formatter,
                "program stream directory of {length} bytes is longer than 65535 bytes"
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::DirectoryStreamId { .. }
            | Self::NumberOfAccessUnits { .. }
            | Self::PacketStreamIdExtension { .. }
            | Self::PesPacketLength { .. } => None,
        }
    }
}

/// Returns a mapping of a failure on `field` to an [`Error::Field`]
const fn field_error(field: &'static str) -> impl Fn(field::Error) -> Error {
    move |error| Error::Field { field, error }
}

/// Directory of the access units a Program Stream can be entered at, and of
/// the directories before and after it
///
/// `directory_PES_packet`, ISO/IEC 13818-1 §2.5.5
///
/// A directory is carried as a PES packet of `stream_id` 0xFF whose bytes
/// after `PES_packet_length` are its own syntax: obtain it from a
/// [`PesPacket`] with [`TryFrom`]. `number_of_access_units` and
/// `PES_packet_length` are derived from the access unit loop. The
/// `marker_bit`s and the `reserved` bits are not checked when read and are
/// written as `'1'`. The crate does not choose the access units a directory
/// names nor count their offsets.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::pes::{PesPacket, StreamId};
/// use mpeg2_systems::ps::ProgramStreamDirectory;
/// use mpeg2_systems::ps::directory::{CodingParametersIndicator, DirectoryEntry};
/// use mpeg2_systems::time::Timestamp;
///
/// // A directory of one intra-coded access unit of video at the start of
/// // the stream, its PTS one second
/// let bytes = [
///     0x00, 0x00, 0x01, 0xff, 0x00, 0x20, 0x00, 0x03, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00,
///     0x01, 0x00, 0x01, 0x00, 0x01, 0xe0, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xf1,
///     0x00, 0x05, 0xbf, 0x21, 0x00, 0x07, 0xe8, 0xcf,
/// ];
/// let packet = PesPacket::read(&bytes)?;
/// let directory = ProgramStreamDirectory::try_from(&packet)?;
/// assert_eq!(
///     directory.access_units.iter().collect::<Vec<_>>(),
///     [Ok(DirectoryEntry::new(
///         StreamId::from(0xe0),
///         false,
///         0,
///         0,
///         Timestamp::try_from(90_000)?,
///         1_000,
///     )
///     .with_intra_coded_indicator(true)
///     .with_coding_parameters_indicator(CodingParametersIndicator::DEFAULT_VALUES))]
/// );
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[doc(alias("directory_PES_packet", "program_stream_directory"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramStreamDirectory<'input> {
    /// `prev_directory_offset`: the bytes back from this packet's start code
    /// to that of the directory before it, 0 where there is none
    pub prev_directory_offset: u64,
    /// `next_directory_offset`: the bytes on from this packet's start code to
    /// that of the directory after it, 0 where there is none
    pub next_directory_offset: u64,
    /// The access units the directory names, in the order of the stream
    pub access_units: DirectoryEntryLoop<'input>,
}

impl<'input> ProgramStreamDirectory<'input> {
    /// Creates a directory of no access units, with no directory before or
    /// after it
    #[expect(
        clippy::new_without_default,
        reason = "the specification defines no default program stream directory"
    )]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_directory_offset: 0,
            next_directory_offset: 0,
            access_units: DirectoryEntryLoop(&[]),
        }
    }

    /// Returns the value with the given `prev_directory_offset`
    #[must_use]
    pub const fn with_prev_directory_offset(mut self, prev_directory_offset: u64) -> Self {
        self.prev_directory_offset = prev_directory_offset;
        self
    }

    /// Returns the value with the given `next_directory_offset`
    #[must_use]
    pub const fn with_next_directory_offset(mut self, next_directory_offset: u64) -> Self {
        self.next_directory_offset = next_directory_offset;
        self
    }

    /// Returns the value with the given `access_units`
    #[must_use]
    pub const fn with_access_units(mut self, access_units: DirectoryEntryLoop<'input>) -> Self {
        self.access_units = access_units;
        self
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[cfg(feature = "alloc")]
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        PREFIX_LENGTH
            .saturating_add(HEADER_LENGTH)
            .saturating_add(self.access_units.0.len())
    }

    /// Appends the directory to `out` as a PES packet of `stream_id` 0xFF, its
    /// `marker_bit` and `reserved` bits `'1'`
    ///
    /// A write that fails leaves `out` as it was. The bytes read back with
    /// [`PesPacket::read`] give a packet that [`PackWriter`] writes in a pack.
    ///
    /// [`PackWriter`]: crate::ps::mux::PackWriter
    ///
    /// # Errors
    ///
    /// * [`PesPacketLength`](Error::PesPacketLength): the directory names
    ///   more access units than 65535 bytes after `PES_packet_length` hold.
    /// * [`Field`](Error::Field): `prev_directory_offset` or
    ///   `next_directory_offset` does not fit 45 bits, or the access unit
    ///   loop ends inside an entry.
    /// * Any error of [`DirectoryEntryLoop::write_from`] for an entry.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::ps::ProgramStreamDirectory;
    ///
    /// // A directory of no access units, the next one 2048 bytes on
    /// let mut written = Vec::new();
    /// ProgramStreamDirectory::new()
    ///     .with_next_directory_offset(2048)
    ///     .write(&mut written)?;
    /// assert_eq!(
    ///     written,
    ///     [
    ///         0x00, 0x00, 0x01, 0xff, 0x00, 0x0e, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01,
    ///         0x00, 0x01, 0x00, 0x01, 0x10, 0x01,
    ///     ]
    /// );
    /// # }
    /// # Ok::<(), mpeg2_systems::ps::directory::Error>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let length = HEADER_LENGTH.saturating_add(self.access_units.0.len());
        let pes_packet_length =
            u16::try_from(length).map_err(|_| Error::PesPacketLength { length })?;
        // Why not an error: the loop is part of `length`, so it holds fewer
        // than 2^15 entries.
        let number_of_access_units =
            u16::try_from(self.access_units.0.len() / ENTRY_LENGTH).unwrap_or(u16::MAX);

        write_or_truncate(out, |out| {
            out.extend_from_slice(&DIRECTORY_START_CODE);
            out.extend_from_slice(&pes_packet_length.to_be_bytes());
            let mut writer = FieldWriter::new(out);
            writer
                .write_bits::<15>(number_of_access_units)
                .and_then(|()| writer.write_bool(true))
                .map_err(field_error("number_of_access_units"))?;
            writer
                .write_marked::<15>(self.prev_directory_offset)
                .map_err(field_error("prev_directory_offset"))?;
            // Why not an error of its own for finish: the 14 bytes before the
            // loop end on a byte boundary.
            writer
                .write_marked::<15>(self.next_directory_offset)
                .and_then(|()| writer.finish())
                .map_err(field_error("next_directory_offset"))?;
            DirectoryEntryLoop::write_entries(self.access_units.iter(), out)
        })
    }
}

impl<'input> TryFrom<&PesPacket<'input>> for ProgramStreamDirectory<'input> {
    type Error = Error;

    /// Reads the directory a PES packet of `stream_id` 0xFF carries in its
    /// payload
    ///
    /// The access unit loop is read entry by entry as it is iterated.
    ///
    /// # Errors
    ///
    /// * [`DirectoryStreamId`](Error::DirectoryStreamId): the `stream_id` is
    ///   not 0xFF.
    /// * [`Field`](Error::Field): the payload ends before
    ///   `next_directory_offset` does.
    /// * [`NumberOfAccessUnits`](Error::NumberOfAccessUnits): the bytes after
    ///   `next_directory_offset` are not 18 for each access unit
    ///   `number_of_access_units` counts.
    fn try_from(packet: &PesPacket<'input>) -> Result<Self, Self::Error> {
        if packet.stream_id != StreamId::PROGRAM_STREAM_DIRECTORY {
            return Err(Error::DirectoryStreamId {
                stream_id: packet.stream_id,
            });
        }
        let mut reader = FieldReader::new(packet.payload);
        let number_of_access_units = reader
            .read_u16::<15>()
            .and_then(|count| reader.read_bits::<1>().map(|_| count))
            .map_err(field_error("number_of_access_units"))?;
        let prev_directory_offset = reader
            .read_marked::<15>()
            .map_err(field_error("prev_directory_offset"))?;
        let next_directory_offset = reader
            .read_marked::<15>()
            .map_err(field_error("next_directory_offset"))?;
        let available_bytes = reader.remaining_bytes();
        if Some(available_bytes) != usize::from(number_of_access_units).checked_mul(ENTRY_LENGTH) {
            return Err(Error::NumberOfAccessUnits {
                number_of_access_units,
                available_bytes,
            });
        }
        // Why not an error: the fields before the loop end on a byte boundary.
        let access_units = reader.read_bytes(available_bytes).unwrap_or_default();

        Ok(Self {
            prev_directory_offset,
            next_directory_offset,
            access_units: DirectoryEntryLoop(access_units),
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec;
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    #[cfg(feature = "alloc")]
    use super::DirectoryEntryLoop;
    use super::entry::tests::{extended_entry, video_entry};
    use super::{Error, ProgramStreamDirectory};
    use crate::field;
    use crate::pes::{PesPacket, StreamId};
    #[cfg(feature = "alloc")]
    use crate::ps::PackHeader;
    #[cfg(feature = "alloc")]
    use crate::ps::demux::{Demuxer, Event};
    #[cfg(feature = "alloc")]
    use crate::ps::mux::PackWriter;
    #[cfg(feature = "alloc")]
    use crate::time::ClockReference;

    /// Two access units, one of video 150 000 bytes back and one of the
    /// stream 0xFD with `stream_id_extension` 0x55, every field of the second
    /// at its widest; no directory before, the next 2 000 000 bytes on
    /// (Table 2-42, laid out by hand)
    const TWO_ACCESS_UNITS: [u8; 56] = [
        0x00, 0x00, 0x01, 0xff, 0x00, 0x32, 0x00, 0x05, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00,
        0x01, 0x00, 0x7b, 0x09, 0x01, 0xe0, 0x80, 0x01, 0x00, 0x09, 0x93, 0xe1, 0x00, 0x0e, 0xf1,
        0x00, 0x05, 0xbf, 0x21, 0x01, 0x39, 0x40, 0xdf, 0xfd, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xdf, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xb5,
    ];

    #[test]
    fn a_directory_names_its_access_units_and_the_directories_around_it() {
        let packet = PesPacket::read(&TWO_ACCESS_UNITS).unwrap();
        let directory = ProgramStreamDirectory::try_from(&packet).unwrap();
        let mut entries = directory.access_units.iter();

        assert_eq!(
            (
                directory.prev_directory_offset,
                directory.next_directory_offset,
                [entries.next(), entries.next(), entries.next()]
            ),
            (
                0,
                2_000_000,
                [Some(Ok(video_entry())), Some(Ok(extended_entry())), None]
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_directory_read_is_written_back_to_the_same_bytes() {
        let packet = PesPacket::read(&TWO_ACCESS_UNITS).unwrap();
        let mut written = Vec::new();
        ProgramStreamDirectory::try_from(&packet)
            .unwrap()
            .write(&mut written)
            .unwrap();

        assert_eq!(written, TWO_ACCESS_UNITS);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_directory_built_from_its_entries_is_written_as_one_laid_out_by_hand() {
        let mut entries = Vec::new();
        DirectoryEntryLoop::write_from([video_entry(), extended_entry()], &mut entries).unwrap();
        let mut written = Vec::new();
        ProgramStreamDirectory::new()
            .with_next_directory_offset(2_000_000)
            .with_access_units(DirectoryEntryLoop::from(entries.as_slice()))
            .write(&mut written)
            .unwrap();

        assert_eq!(written, TWO_ACCESS_UNITS);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_directory_written_through_the_pack_writer_is_read_back_from_the_stream() {
        let mut stream = Vec::new();
        let writer = PackWriter::new();
        writer
            .write_pack_header(
                &PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000),
                &mut stream,
            )
            .unwrap();
        writer
            .write_pes_packets(&PesPacket::read(&TWO_ACCESS_UNITS).unwrap(), &mut stream)
            .unwrap();
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(&stream);
        demuxer.finish();
        let mut directories = Vec::new();
        while let Some(event) = demuxer.poll_event() {
            if let Event::PesPacket(packet) = event {
                directories.push(ProgramStreamDirectory::try_from(&packet).map(|directory| {
                    (
                        directory.next_directory_offset,
                        directory.access_units.iter().collect::<Vec<_>>(),
                    )
                }));
            }
        }

        assert_eq!(
            directories,
            [Ok((
                2_000_000,
                vec![Ok(video_entry()), Ok(extended_entry())]
            ))]
        );
    }

    #[test]
    fn marker_and_reserved_bits_read_as_0_are_not_checked() {
        // One video entry as in TWO_ACCESS_UNITS, every marker_bit and
        // reserved bit '0'
        let bytes = [
            0x00, 0x00, 0x01, 0xff, 0x00, 0x20, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x7a, 0x09, 0x00, 0xe0, 0x80, 0x00, 0x00, 0x08, 0x93, 0xe0, 0x00,
            0x0e, 0x00, 0x00, 0x04, 0xbf, 0x20, 0x01, 0x38, 0x40, 0x50,
        ];
        let packet = PesPacket::read(&bytes).unwrap();
        let directory = ProgramStreamDirectory::try_from(&packet).unwrap();
        let mut entries = directory.access_units.iter();

        assert_eq!(
            (
                directory.next_directory_offset,
                [entries.next(), entries.next()]
            ),
            (2_000_000, [Some(Ok(video_entry())), None])
        );
    }

    #[test]
    fn a_pes_packet_of_another_stream_id_is_not_a_directory() {
        let packet = PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0x00; 14]);

        assert_eq!(
            ProgramStreamDirectory::try_from(&packet),
            Err(Error::DirectoryStreamId {
                stream_id: StreamId::PRIVATE_STREAM_2
            })
        );
    }

    #[test]
    fn a_count_of_access_units_that_disagrees_with_the_bytes_after_it_is_refused() {
        let packet = PesPacket::new(StreamId::PROGRAM_STREAM_DIRECTORY, &TWO_ACCESS_UNITS[6..55]);

        assert_eq!(
            ProgramStreamDirectory::try_from(&packet),
            Err(Error::NumberOfAccessUnits {
                number_of_access_units: 2,
                available_bytes: 35
            })
        );
    }

    #[test]
    fn a_payload_that_ends_before_the_next_directory_offset_is_refused() {
        let packet = PesPacket::new(StreamId::PROGRAM_STREAM_DIRECTORY, &TWO_ACCESS_UNITS[6..19]);

        assert_eq!(
            ProgramStreamDirectory::try_from(&packet),
            Err(Error::Field {
                field: "next_directory_offset",
                error: field::Error::Truncated {
                    required_bits: 15,
                    remaining_bits: 8
                }
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn directories_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let mut entries = Vec::new();
        DirectoryEntryLoop::write_from([video_entry(); 3_641], &mut entries).unwrap();
        let mut written = vec![0x47];

        assert_eq!(
            [
                ProgramStreamDirectory::new()
                    .with_access_units(DirectoryEntryLoop::from(entries.as_slice()))
                    .write(&mut written),
                ProgramStreamDirectory::new()
                    .with_prev_directory_offset(1 << 45)
                    .write(&mut written),
            ],
            [
                Err(Error::PesPacketLength { length: 65_552 }),
                Err(Error::Field {
                    field: "prev_directory_offset",
                    error: field::Error::ValueTooWide {
                        width: 45,
                        value: 1 << 45
                    }
                }),
            ]
        );
        assert_eq!(written, [0x47]);
    }
}
