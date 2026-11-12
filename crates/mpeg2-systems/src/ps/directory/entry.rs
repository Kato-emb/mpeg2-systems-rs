//! Access unit loop of the program stream directory: each access unit, where
//! its PES packet starts and how many bytes decode it

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{Error, field_error};
use crate::field::{self, FieldReader, TwoBitFieldOutOfRange};
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::pes::StreamId;
use crate::time::Timestamp;

/// Bytes of an entry of the access unit loop
pub(super) const ENTRY_LENGTH: usize = 18;

// Why not two fields of their own when read and written:
// `PES_header_position_offset_sign` and the 14 bits above the two 15-bit runs
// of `PES_header_position_offset` fill one 15-bit run before a `marker_bit`.
/// Bits of `PES_header_position_offset`
const OFFSET_WIDTH: u32 = 44;

/// The bits of `PES_header_position_offset` among those of the sign and it
const OFFSET_MASK: u64 = (1 << OFFSET_WIDTH) - 1;

/// Where the coding parameters an access unit needs are set
///
/// `coding_parameters_indicator`, ISO/IEC 13818-1 §2.5.5.2, Table 2-44
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::TwoBitFieldOutOfRange;
/// use mpeg2_systems::ps::directory::CodingParametersIndicator;
///
/// assert_eq!(
///     CodingParametersIndicator::try_from(0b11),
///     Ok(CodingParametersIndicator::NONE_CODED)
/// );
/// assert_eq!(
///     CodingParametersIndicator::try_from(4).map_err(TwoBitFieldOutOfRange::value),
///     Err(4)
/// );
/// ```
#[doc(alias = "coding_parameters_indicator")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CodingParametersIndicator(u8);

impl CodingParametersIndicator {
    /// `'00'`: all coding parameters are set to their default values
    pub const DEFAULT_VALUES: Self = Self(0b00);
    /// `'01'`: all coding parameters are set in this access unit, at least one
    /// of them not to a default
    pub const ALL_SET: Self = Self(0b01);
    /// `'10'`: some coding parameters are set in this access unit
    pub const SOME_SET: Self = Self(0b10);
    /// `'11'`: no coding parameters are coded in this access unit
    pub const NONE_CODED: Self = Self(0b11);

    /// Returns the 2-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for CodingParametersIndicator {
    type Error = TwoBitFieldOutOfRange;

    /// Takes a 2-bit value
    ///
    /// # Errors
    ///
    /// [`TwoBitFieldOutOfRange`]: `value` is 4 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        TwoBitFieldOutOfRange::check(value).map(Self)
    }
}

/// Entry of the access unit loop of a [`ProgramStreamDirectory`](super::ProgramStreamDirectory): an access
/// unit, where its PES packet starts and how many bytes decode it
///
/// The `for` loop of `directory_PES_packet`, ISO/IEC 13818-1 §2.5.5.1,
/// Table 2-42
///
/// `PES_header_position_offset` and `prev_directory_offset` count from the
/// first byte of the start code of the directory that holds the entry.
#[doc(alias = "directory entry")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DirectoryEntry {
    /// `stream_id` of the elementary stream the access unit belongs to
    pub packet_stream_id: StreamId,
    /// `stream_id_extension` of the PES packets of the access unit, present
    /// only where `packet_stream_id` is 0xFD
    pub packet_stream_id_extension: Option<u8>,
    /// `PES_header_position_offset_sign`: the PES packet lies before the
    /// directory
    pub pes_header_position_offset_sign: bool,
    /// `PES_header_position_offset`: the bytes from the directory's start code
    /// to that of the PES packet holding the first byte of the access unit,
    /// 0 where the entry names no access unit
    pub pes_header_position_offset: u64,
    /// `reference_offset`: the bytes from the first byte of that PES packet
    /// to the first byte of the access unit
    pub reference_offset: u16,
    /// `PTS` of the access unit
    pub pts: Timestamp,
    /// `bytes_to_read`: the bytes of the stream after the first byte of the
    /// access unit that decode it whole, those of other streams included
    pub bytes_to_read: u32,
    /// `intra_coded_indicator`: the access unit is not predictively coded,
    /// undefined but for H.262 video (Table 2-43)
    pub intra_coded_indicator: bool,
    /// Where the coding parameters the access unit needs are set
    pub coding_parameters_indicator: CodingParametersIndicator,
}

impl DirectoryEntry {
    /// Creates an entry of an access unit of `packet_stream_id`, without
    /// `packet_stream_id_extension`, not intra-coded and with its coding
    /// parameters at their default values
    #[must_use]
    pub const fn new(
        packet_stream_id: StreamId,
        pes_header_position_offset_sign: bool,
        pes_header_position_offset: u64,
        reference_offset: u16,
        pts: Timestamp,
        bytes_to_read: u32,
    ) -> Self {
        Self {
            packet_stream_id,
            packet_stream_id_extension: None,
            pes_header_position_offset_sign,
            pes_header_position_offset,
            reference_offset,
            pts,
            bytes_to_read,
            intra_coded_indicator: false,
            coding_parameters_indicator: CodingParametersIndicator::DEFAULT_VALUES,
        }
    }

    /// Returns the value with `packet_stream_id_extension` present, set to
    /// the one given
    #[must_use]
    pub const fn with_packet_stream_id_extension(mut self, packet_stream_id_extension: u8) -> Self {
        self.packet_stream_id_extension = Some(packet_stream_id_extension);
        self
    }

    /// Returns the value with the given `intra_coded_indicator`
    #[must_use]
    pub const fn with_intra_coded_indicator(mut self, intra_coded_indicator: bool) -> Self {
        self.intra_coded_indicator = intra_coded_indicator;
        self
    }

    /// Returns the value with the given `coding_parameters_indicator`
    #[must_use]
    pub const fn with_coding_parameters_indicator(
        mut self,
        coding_parameters_indicator: CodingParametersIndicator,
    ) -> Self {
        self.coding_parameters_indicator = coding_parameters_indicator;
        self
    }
}

/// The access unit loop of a [`ProgramStreamDirectory`](super::ProgramStreamDirectory), borrowed
///
/// The bytes are read entry by entry as [`iter`](Self::iter) walks them, so
/// a view of any bytes can be made; an entry the bytes end inside is an
/// error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DirectoryEntryLoop<'input>(pub(super) &'input [u8]);

impl<'input> DirectoryEntryLoop<'input> {
    /// Returns the bytes of the loop
    #[must_use]
    pub const fn as_bytes(self) -> &'input [u8] {
        self.0
    }

    /// Returns an iterator over the entries, each read as it is reached
    #[must_use]
    pub const fn iter(self) -> DirectoryEntryIter<'input> {
        DirectoryEntryIter(self.0)
    }

    /// Appends the bytes of a loop of `entries` to `out`, the `marker_bit`s
    /// and the `reserved` bits `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`PacketStreamIdExtension`](Error::PacketStreamIdExtension): an
    ///   entry has a `packet_stream_id_extension` where `packet_stream_id` is
    ///   not 0xFD, or none where it is.
    /// * [`Field`](Error::Field): a `packet_stream_id_extension` that does
    ///   not fit 7 bits, a `PES_header_position_offset` that does not fit 44
    ///   bits, or a `bytes_to_read` that does not fit 23 bits.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::pes::StreamId;
    /// use mpeg2_systems::ps::directory::{DirectoryEntry, DirectoryEntryLoop};
    /// use mpeg2_systems::time::Timestamp;
    ///
    /// // An access unit of video at the start of the stream
    /// let mut written = Vec::new();
    /// DirectoryEntryLoop::write_from(
    ///     [DirectoryEntry::new(StreamId::from(0xe0), false, 0, 0, Timestamp::try_from(90_000)?, 1_000)],
    ///     &mut written,
    /// )?;
    /// assert_eq!(written.len(), 18);
    /// # }
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from(
        entries: impl IntoIterator<Item = DirectoryEntry>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        Self::write_entries(entries.into_iter().map(Ok), out)
    }

    /// Appends the bytes of `entries` to `out`, leaving `out` as it was if
    /// an entry is an error or is refused
    #[cfg(feature = "alloc")]
    pub(super) fn write_entries(
        entries: impl IntoIterator<Item = Result<DirectoryEntry, Error>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        write_or_truncate(out, |out| {
            for entry in entries {
                let entry = entry?;
                let packet_stream_id = entry.packet_stream_id;
                if entry.packet_stream_id_extension.is_some()
                    != (packet_stream_id == StreamId::EXTENDED_STREAM_ID)
                {
                    return Err(Error::PacketStreamIdExtension { packet_stream_id });
                }
                // Why not split the reserved bits of an entry without an
                // extension from an extension: both are 7 bits in the same
                // places, the reserved ones all '1'.
                let extension = entry.packet_stream_id_extension.unwrap_or(0x7f);
                check_width("packet_stream_id_extension", 7, u64::from(extension))?;
                let offset = entry.pes_header_position_offset;
                check_width("PES_header_position_offset", OFFSET_WIDTH, offset)?;
                let bytes_to_read = entry.bytes_to_read;
                check_width("bytes_to_read", 23, u64::from(bytes_to_read))?;

                let mut writer = FieldWriter::new(out);
                // Why not an error per field: each value was checked or masked
                // to its field's width above.
                (|| {
                    writer.write_bits::<8>(packet_stream_id.get())?;
                    writer.write_marked::<15>(
                        u64::from(entry.pes_header_position_offset_sign).wrapping_shl(OFFSET_WIDTH)
                            | offset,
                    )?;
                    writer.write_bits::<16>(entry.reference_offset)?;
                    writer.write_bool(true)?;
                    writer.write_bits::<3>(extension.wrapping_shr(4))?;
                    writer.write_marked::<3>(entry.pts.get())?;
                    writer.write_bits::<15>(bytes_to_read.wrapping_shr(8) & 0x7fff)?;
                    writer.write_bool(true)?;
                    writer.write_bits::<8>(bytes_to_read & 0xff)?;
                    writer.write_bool(true)?;
                    writer.write_bool(entry.intra_coded_indicator)?;
                    writer.write_bits::<2>(entry.coding_parameters_indicator.get())?;
                    writer.write_bits::<4>(extension & 0x0f)?;
                    writer.finish()
                })()
                .map_err(field_error("access_unit"))?;
            }

            Ok(())
        })
    }
}

/// Refuses `value` for `field` where it has a bit set above `width`
#[cfg(feature = "alloc")]
fn check_width(field: &'static str, width: u32, value: u64) -> Result<(), Error> {
    if value.checked_shr(width).unwrap_or(0) == 0 {
        Ok(())
    } else {
        Err(Error::Field {
            field,
            error: field::Error::ValueTooWide { width, value },
        })
    }
}

impl<'input> From<&'input [u8]> for DirectoryEntryLoop<'input> {
    fn from(bytes: &'input [u8]) -> Self {
        Self(bytes)
    }
}

impl<'input> IntoIterator for DirectoryEntryLoop<'input> {
    type Item = Result<DirectoryEntry, Error>;
    type IntoIter = DirectoryEntryIter<'input>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the entries of a [`DirectoryEntryLoop`]
///
/// An entry the bytes end inside is an [`Error::Field`], and the last item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DirectoryEntryIter<'input>(&'input [u8]);

impl Iterator for DirectoryEntryIter<'_> {
    type Item = Result<DirectoryEntry, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        let Some((bytes, rest)) = self.0.split_first_chunk::<ENTRY_LENGTH>() else {
            let remaining_bits = u64::try_from(self.0.len())
                .unwrap_or(u64::MAX)
                .saturating_mul(8);
            self.0 = &[];
            return Some(Err(Error::Field {
                field: "access_unit",
                error: field::Error::Truncated {
                    required_bits: 144,
                    remaining_bits,
                },
            }));
        };
        self.0 = rest;

        let mut reader = FieldReader::new(bytes);
        // Why not an error per field: the 18 bytes hold every field.
        let entry = (|| {
            let packet_stream_id = StreamId::from(reader.read_u8::<8>()?);
            let position = reader.read_marked::<15>()?;
            let reference_offset = reader.read_u16::<16>()?;
            reader.read_bits::<1>()?;
            let extension_msbs = reader.read_u8::<3>()?;
            let pts = reader.read_marked::<3>()?;
            let bytes_to_read_msbs = reader.read_u32::<15>()?;
            reader.read_bits::<1>()?;
            let bytes_to_read = bytes_to_read_msbs.wrapping_shl(8) | reader.read_u32::<8>()?;
            reader.read_bits::<1>()?;
            let intra_coded_indicator = reader.read_bool()?;
            let coding_parameters_indicator = reader.read_u8::<2>()?;
            let extension_lsbs = reader.read_u8::<4>()?;
            Ok(DirectoryEntry {
                packet_stream_id,
                packet_stream_id_extension: (packet_stream_id == StreamId::EXTENDED_STREAM_ID)
                    .then_some(extension_msbs.wrapping_shl(4) | extension_lsbs),
                pes_header_position_offset_sign: position.wrapping_shr(OFFSET_WIDTH) == 1,
                pes_header_position_offset: position & OFFSET_MASK,
                reference_offset,
                pts: Timestamp::from_bits_truncate(pts),
                bytes_to_read,
                intra_coded_indicator,
                coding_parameters_indicator: CodingParametersIndicator(coding_parameters_indicator),
            })
        })();

        Some(entry.map_err(field_error("access_unit")))
    }
}

#[cfg(test)]
pub(super) mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec;

    use super::{CodingParametersIndicator, DirectoryEntry, DirectoryEntryLoop, Error};
    use crate::field;
    use crate::pes::StreamId;
    use crate::time::Timestamp;

    /// The entry of an intra-coded access unit of video 150 000 bytes back
    /// (Table 2-42, laid out by hand)
    const VIDEO_ENTRY: [u8; 18] = [
        0xe0, 0x80, 0x01, 0x00, 0x09, 0x93, 0xe1, 0x00, 0x0e, 0xf1, 0x00, 0x05, 0xbf, 0x21, 0x01,
        0x39, 0x40, 0xdf,
    ];

    /// The entry [`VIDEO_ENTRY`] holds
    pub(in crate::ps::directory) fn video_entry() -> DirectoryEntry {
        DirectoryEntry::new(
            StreamId::from(0xe0),
            true,
            150_000,
            14,
            Timestamp::try_from(90_000).unwrap(),
            40_000,
        )
        .with_intra_coded_indicator(true)
        .with_coding_parameters_indicator(CodingParametersIndicator::ALL_SET)
    }

    /// An entry of the stream 0xFD with `stream_id_extension` 0x55, every
    /// field at its widest
    pub(in crate::ps::directory) fn extended_entry() -> DirectoryEntry {
        DirectoryEntry::new(
            StreamId::EXTENDED_STREAM_ID,
            false,
            (1 << 44) - 1,
            0xffff,
            Timestamp::try_from((1 << 33) - 1).unwrap(),
            (1 << 23) - 1,
        )
        .with_packet_stream_id_extension(0x55)
        .with_coding_parameters_indicator(CodingParametersIndicator::NONE_CODED)
    }

    #[test]
    fn an_entry_the_loop_ends_inside_is_the_last_item() {
        let mut bytes = [0xfd; 30];
        bytes.get_mut(..18).unwrap().copy_from_slice(&VIDEO_ENTRY);
        let mut entries = DirectoryEntryLoop::from(bytes.as_slice()).iter();

        assert_eq!(
            [entries.next(), entries.next(), entries.next()],
            [
                Some(Ok(video_entry())),
                Some(Err(Error::Field {
                    field: "access_unit",
                    error: field::Error::Truncated {
                        required_bits: 144,
                        remaining_bits: 96
                    }
                })),
                None
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn entries_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let video = video_entry();
        let mut extended_without = extended_entry();
        extended_without.packet_stream_id_extension = None;
        let too_wide = |entry: DirectoryEntry| {
            let mut out = vec![0x47];
            (
                DirectoryEntryLoop::write_from([video, entry], &mut out),
                out,
            )
        };
        let mut offset = video;
        offset.pes_header_position_offset = 1 << 44;
        let mut bytes_to_read = video;
        bytes_to_read.bytes_to_read = 1 << 23;

        assert_eq!(
            [
                too_wide(video.with_packet_stream_id_extension(0x01)),
                too_wide(extended_without),
                too_wide(extended_entry().with_packet_stream_id_extension(0x80)),
                too_wide(offset),
                too_wide(bytes_to_read),
            ],
            [
                (
                    Err(Error::PacketStreamIdExtension {
                        packet_stream_id: StreamId::from(0xe0)
                    }),
                    vec![0x47]
                ),
                (
                    Err(Error::PacketStreamIdExtension {
                        packet_stream_id: StreamId::EXTENDED_STREAM_ID
                    }),
                    vec![0x47]
                ),
                (
                    Err(Error::Field {
                        field: "packet_stream_id_extension",
                        error: field::Error::ValueTooWide {
                            width: 7,
                            value: 0x80
                        }
                    }),
                    vec![0x47]
                ),
                (
                    Err(Error::Field {
                        field: "PES_header_position_offset",
                        error: field::Error::ValueTooWide {
                            width: 44,
                            value: 1 << 44
                        }
                    }),
                    vec![0x47]
                ),
                (
                    Err(Error::Field {
                        field: "bytes_to_read",
                        error: field::Error::ValueTooWide {
                            width: 23,
                            value: 1 << 23
                        }
                    }),
                    vec![0x47]
                ),
            ]
        );
    }
}
