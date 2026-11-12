//! Program stream map: the elementary streams of a Program Stream, and the
//! descriptors of the program and of each stream

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use super::PROGRAM_STREAM_MAP_START_CODE;
use crate::crc::crc_32;
use crate::descriptor::{self, DescriptorLoop};
use crate::field::{self, FieldReader};
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::pes::StreamId;
use crate::psi::VersionNumber;

mod elementary_stream;

pub use elementary_stream::{ElementaryStream, ElementaryStreamIter, ElementaryStreamLoop};

/// Bytes from `packet_start_code_prefix` to `program_stream_map_length`
const PREFIX_LENGTH: usize = 6;

/// Returns the bytes of the program stream map `prefix` starts with, from its
/// `program_stream_map_length`, `None` where `prefix` ends before it
#[cfg(feature = "alloc")]
pub(super) fn encoded_len(prefix: &[u8]) -> Option<usize> {
    let [.., high, low] = *prefix.first_chunk::<PREFIX_LENGTH>()?;
    Some(PREFIX_LENGTH.saturating_add(usize::from(u16::from_be_bytes([high, low]))))
}

/// Bytes of a map besides its descriptors and its elementary stream loop
const OVERHEAD: usize = 16;

/// Bytes of `CRC_32`
const CRC_32_LENGTH: usize = 4;

/// Largest `program_stream_map_length`
const MAX_PROGRAM_STREAM_MAP_LENGTH: u16 = 0x3fa;

/// Failure to read or write a program stream map, or an entry of its
/// elementary stream loop
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The bytes do not start with `packet_start_code_prefix` 0x000001 and
    /// `map_stream_id` 0xBC
    PacketStartCode {
        /// The 32 bits found
        packet_start_code: u32,
    },
    /// A field runs past the end of what frames it, or does not fit its width
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field failed
        error: field::Error,
    },
    /// `program_stream_map_length` is larger than 1018
    ProgramStreamMapLength {
        /// The `program_stream_map_length` found, or that the fields add up
        /// to
        program_stream_map_length: usize,
        /// The largest `program_stream_map_length`, 1018
        limit: u16,
    },
    /// The bytes given are not as many as `program_stream_map_length`
    /// declares
    Length {
        /// The `program_stream_map_length` read
        program_stream_map_length: u16,
        /// The bytes given after `program_stream_map_length`
        available_bytes: usize,
    },
    /// The map does not bring the CRC decoder of Annex A to zero
    Crc32 {
        /// The `CRC_32` the map carries
        crc_32: u32,
        /// The `CRC_32` the bytes before it give
        computed: u32,
    },
    /// The bytes between `elementary_stream_map_length` and `CRC_32` are not
    /// as many as it declares
    ElementaryStreamMapLength {
        /// The `elementary_stream_map_length` read
        elementary_stream_map_length: u16,
        /// The bytes between `elementary_stream_map_length` and `CRC_32`
        available_bytes: usize,
    },
    /// A descriptor to write is cut short or refused
    Descriptor(descriptor::Error),
    /// A value to write is one the specification forbids for its field
    Forbidden {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The value given
        value: u32,
    },
    /// An elementary stream to write has an `elementary_stream_id_extension`
    /// where it may not, or none where it must: only 0xFD has one, and only
    /// in a map whose `single_extension_stream_flag` is `'0'`
    ElementaryStreamIdExtension {
        /// The `elementary_stream_id` of the elementary stream
        elementary_stream_id: StreamId,
    },
    /// A map to write has `single_extension_stream_flag` set and more than
    /// one elementary stream of `elementary_stream_id` 0xFD
    SingleExtensionStream,
    /// The descriptors of an elementary stream to write are longer than
    /// `elementary_stream_info_length` can state
    ElementaryStreamInfoLength {
        /// The bytes after `elementary_stream_info_length`
        length: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketStartCode { packet_start_code } => write!(
                formatter,
                "packet start code {packet_start_code:#010x} is not 0x000001bc"
            ),
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read or written")
            }
            Self::ProgramStreamMapLength {
                program_stream_map_length,
                limit,
            } => write!(
                formatter,
                "program_stream_map_length {program_stream_map_length} is larger than {limit}"
            ),
            Self::Length {
                program_stream_map_length,
                available_bytes,
            } => write!(
                formatter,
                "program_stream_map_length {program_stream_map_length} does not match the \
                 {available_bytes} bytes given"
            ),
            Self::Crc32 { crc_32, computed } => write!(
                formatter,
                "CRC_32 {crc_32:#010x} does not match {computed:#010x}, the CRC of the map"
            ),
            Self::ElementaryStreamMapLength {
                elementary_stream_map_length,
                available_bytes,
            } => write!(
                formatter,
                "elementary_stream_map_length {elementary_stream_map_length} does not match the \
                 {available_bytes} bytes before CRC_32"
            ),
            Self::Descriptor(_) => write!(formatter, "a descriptor could not be written"),
            Self::Forbidden { field, value } => {
                write!(formatter, "{field} {value} is forbidden")
            }
            Self::ElementaryStreamIdExtension {
                elementary_stream_id,
            } => write!(
                formatter,
                "elementary_stream_id {:#04x} and the presence of \
                 elementary_stream_id_extension disagree",
                elementary_stream_id.get()
            ),
            Self::SingleExtensionStream => write!(
                formatter,
                "single_extension_stream_flag is set for more than one stream_id 0xfd"
            ),
            Self::ElementaryStreamInfoLength { length } => write!(
                formatter,
                "elementary_stream_info of {length} bytes is longer than 65535 bytes"
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::Descriptor(error) => Some(error),
            Self::PacketStartCode { .. }
            | Self::ProgramStreamMapLength { .. }
            | Self::Length { .. }
            | Self::Crc32 { .. }
            | Self::ElementaryStreamMapLength { .. }
            | Self::Forbidden { .. }
            | Self::ElementaryStreamIdExtension { .. }
            | Self::SingleExtensionStream
            | Self::ElementaryStreamInfoLength { .. } => None,
        }
    }
}

/// Returns a mapping of a failure on `field` to an [`Error::Field`]
const fn field_error(field: &'static str) -> impl Fn(field::Error) -> Error {
    move |error| Error::Field { field, error }
}

impl From<descriptor::Error> for Error {
    fn from(error: descriptor::Error) -> Self {
        Self::Descriptor(error)
    }
}

/// Map of the elementary streams of a Program Stream, with the descriptors
/// of the program and of each stream
///
/// `program_stream_map` (PSM), ISO/IEC 13818-1 §2.5.4
///
/// A program stream map is carried as a packet of `stream_id` 0xBC, but its
/// syntax is its own: read it from the bytes of that packet, from
/// `packet_start_code_prefix` to `CRC_32`. `program_stream_map_length` and
/// `CRC_32` are derived from the fields. The `marker_bit`s and the
/// `reserved` bits are not checked when read and are written as `'1'`.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::pes::StreamId;
/// use mpeg2_systems::ps::ProgramStreamMap;
/// use mpeg2_systems::ps::psm::ElementaryStream;
/// use mpeg2_systems::stream_type::StreamType;
///
/// // Version 3 of a map of H.264 video on 0xE0 and AAC audio on 0xC0
/// let bytes = [
///     0x00, 0x00, 0x01, 0xbc, 0x00, 0x12, 0xe3, 0xff, 0x00, 0x00, 0x00, 0x08, 0x1b, 0xe0, 0x00,
///     0x00, 0x0f, 0xc0, 0x00, 0x00, 0x82, 0x5d, 0x94, 0x4b,
/// ];
/// let psm = ProgramStreamMap::read(&bytes)?;
/// assert_eq!(psm.program_stream_map_version.get(), 3);
/// assert_eq!(
///     psm.elementary_streams.iter().next(),
///     Some(Ok(ElementaryStream::new(StreamType::AVC_VIDEO, StreamId::from(0xe0))))
/// );
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[doc(alias("PSM", "program_stream_map"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramStreamMap<'input> {
    /// `current_next_indicator`: the map applies now, not next
    pub current_next_indicator: bool,
    /// `single_extension_stream_flag`: the stream holds at most one
    /// elementary stream of `stream_id` 0xFD, which then carries no
    /// `elementary_stream_id_extension`
    pub single_extension_stream_flag: bool,
    /// `program_stream_map_version`, incremented modulo 32 whenever the map
    /// changes
    pub program_stream_map_version: VersionNumber,
    /// The descriptors of the program
    pub program_stream_info: DescriptorLoop<'input>,
    /// The elementary streams of the program
    pub elementary_streams: ElementaryStreamLoop<'input>,
}

impl<'input> ProgramStreamMap<'input> {
    /// Creates a map that applies now, without descriptors or elementary
    /// streams
    #[must_use]
    pub const fn new(program_stream_map_version: VersionNumber) -> Self {
        Self {
            current_next_indicator: true,
            single_extension_stream_flag: false,
            program_stream_map_version,
            program_stream_info: DescriptorLoop::EMPTY,
            elementary_streams: ElementaryStreamLoop(&[]),
        }
    }

    /// Returns the value with the given `current_next_indicator`
    #[must_use]
    pub const fn with_current_next_indicator(mut self, current_next_indicator: bool) -> Self {
        self.current_next_indicator = current_next_indicator;
        self
    }

    /// Returns the value with the given `single_extension_stream_flag`
    #[must_use]
    pub const fn with_single_extension_stream_flag(
        mut self,
        single_extension_stream_flag: bool,
    ) -> Self {
        self.single_extension_stream_flag = single_extension_stream_flag;
        self
    }

    /// Returns the value with the given `program_stream_info`
    #[must_use]
    pub const fn with_program_stream_info(
        mut self,
        program_stream_info: DescriptorLoop<'input>,
    ) -> Self {
        self.program_stream_info = program_stream_info;
        self
    }

    /// Returns the value with the given `elementary_streams`
    #[must_use]
    pub const fn with_elementary_streams(
        mut self,
        elementary_streams: ElementaryStreamLoop<'input>,
    ) -> Self {
        self.elementary_streams = elementary_streams;
        self
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        OVERHEAD
            .saturating_add(self.program_stream_info.as_bytes().len())
            .saturating_add(self.elementary_streams.0.len())
    }

    /// Reads a program stream map from `bytes`, which hold it and nothing
    /// else, and checks its `CRC_32`
    ///
    /// The elementary stream loop is read entry by entry as it is iterated.
    ///
    /// # Errors
    ///
    /// * [`PacketStartCode`](Error::PacketStartCode): the bytes do not start
    ///   with 0x000001BC.
    /// * [`ProgramStreamMapLength`](Error::ProgramStreamMapLength):
    ///   `program_stream_map_length` is larger than 1018.
    /// * [`Length`](Error::Length): the bytes after
    ///   `program_stream_map_length` are not as many as it declares.
    /// * [`Crc32`](Error::Crc32): the map does not bring the CRC decoder to
    ///   zero.
    /// * [`Field`](Error::Field): the map ends inside a field before the
    ///   elementary stream loop, or the descriptors of the program run past
    ///   `CRC_32`.
    /// * [`ElementaryStreamMapLength`](Error::ElementaryStreamMapLength):
    ///   the bytes before `CRC_32` are not as many as
    ///   `elementary_stream_map_length` declares.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        let mut reader = FieldReader::new(bytes);
        let mut prefix = || Ok((reader.read_u32::<32>()?, reader.read_u16::<16>()?));
        let (packet_start_code, program_stream_map_length) =
            prefix().map_err(field_error("program_stream_map_length"))?;
        if packet_start_code.to_be_bytes() != PROGRAM_STREAM_MAP_START_CODE {
            return Err(Error::PacketStartCode { packet_start_code });
        }
        if program_stream_map_length > MAX_PROGRAM_STREAM_MAP_LENGTH {
            return Err(Error::ProgramStreamMapLength {
                program_stream_map_length: usize::from(program_stream_map_length),
                limit: MAX_PROGRAM_STREAM_MAP_LENGTH,
            });
        }
        let available_bytes = reader.remaining_bytes();
        if available_bytes != usize::from(program_stream_map_length) {
            return Err(Error::Length {
                program_stream_map_length,
                available_bytes,
            });
        }
        let remaining_bits = reader.remaining_bits();
        let rest = bytes.get(PREFIX_LENGTH..).unwrap_or_default();
        let (body, crc_field) = rest
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

        let mut reader = FieldReader::new(body);
        let mut flags = || {
            let current_next_indicator = reader.read_bool()?;
            let single_extension_stream_flag = reader.read_bool()?;
            reader.read_bits::<1>()?;
            let version = reader.read_u8::<5>()?;
            reader.read_bits::<8>()?;
            Ok((
                current_next_indicator,
                single_extension_stream_flag,
                version,
            ))
        };
        let (current_next_indicator, single_extension_stream_flag, version) =
            flags().map_err(field_error("program_stream_map_version"))?;
        let program_stream_info = reader
            .read_u16::<16>()
            .and_then(|length| reader.read_bytes(usize::from(length)))
            .map_err(field_error("program_stream_info"))?;
        let elementary_stream_map_length = reader
            .read_u16::<16>()
            .map_err(field_error("elementary_stream_map_length"))?;
        let available_bytes = reader.remaining_bytes();
        if available_bytes != usize::from(elementary_stream_map_length) {
            return Err(Error::ElementaryStreamMapLength {
                elementary_stream_map_length,
                available_bytes,
            });
        }
        // Why not an error: every field before the loop ends on a byte
        // boundary.
        let elementary_streams = reader.read_bytes(available_bytes).unwrap_or_default();

        Ok(Self {
            current_next_indicator,
            single_extension_stream_flag,
            program_stream_map_version: VersionNumber(version),
            program_stream_info: DescriptorLoop::from(program_stream_info),
            elementary_streams: ElementaryStreamLoop(elementary_streams),
        })
    }

    /// Appends the program stream map to `out`, its `marker_bit` and
    /// `reserved` bits `'1'` and its `CRC_32` computed
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`ProgramStreamMapLength`](Error::ProgramStreamMapLength): the map
    ///   would have `program_stream_map_length` larger than 1018.
    /// * [`ElementaryStreamIdExtension`](Error::ElementaryStreamIdExtension):
    ///   an elementary stream of `elementary_stream_id` 0xFD has an
    ///   `elementary_stream_id_extension` where `single_extension_stream_flag`
    ///   is set, or none where it is not.
    /// * [`SingleExtensionStream`](Error::SingleExtensionStream):
    ///   `single_extension_stream_flag` is set and more than one elementary
    ///   stream has `elementary_stream_id` 0xFD.
    /// * [`Forbidden`](Error::Forbidden): an elementary stream is refused as
    ///   [`ElementaryStreamLoop::write_from`] refuses it.
    /// * [`Field`](Error::Field): the elementary stream loop ends inside an
    ///   entry.
    /// * [`Descriptor`](Error::Descriptor): a descriptor loop ends inside a
    ///   descriptor, or a descriptor is refused by
    ///   [`RawDescriptor::write`](crate::descriptor::RawDescriptor::write).
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::ps::ProgramStreamMap;
    /// use mpeg2_systems::ps::psm::ElementaryStreamLoop;
    /// use mpeg2_systems::psi::VersionNumber;
    ///
    /// // Version 0 of a map of MPEG-2 video on 0xE0
    /// let streams = [0x02, 0xe0, 0x00, 0x00];
    /// let psm = ProgramStreamMap::new(VersionNumber::try_from(0)?)
    ///     .with_elementary_streams(ElementaryStreamLoop::from(streams.as_slice()));
    /// let mut written = Vec::new();
    /// psm.write(&mut written)?;
    /// assert_eq!(
    ///     written,
    ///     [
    ///         0x00, 0x00, 0x01, 0xbc, 0x00, 0x0e, 0xa0, 0xff, 0x00, 0x00, 0x00, 0x04, 0x02, 0xe0,
    ///         0x00, 0x00, 0x9a, 0x44, 0x3d, 0x7b,
    ///     ]
    /// );
    /// # }
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        check_encoded_len(self.encoded_len())?;
        let length = self.encoded_len().saturating_sub(PREFIX_LENGTH);
        // Why not an error: the loops are part of `length`, at most 1018 by
        // the check above. Why not the bytes the loops are written as: an
        // entry is written as long as it was read.
        let [
            program_stream_map_length,
            program_stream_info_length,
            elementary_stream_map_length,
        ] = [
            length,
            self.program_stream_info.as_bytes().len(),
            self.elementary_streams.0.len(),
        ]
        .map(|length| u16::try_from(length).unwrap_or(u16::MAX).to_be_bytes());
        let mut has_extended_stream = false;
        let elementary_streams = self.elementary_streams.iter().map(|stream| {
            let stream = stream?;
            if stream.elementary_stream_id == StreamId::EXTENDED_STREAM_ID {
                if stream.elementary_stream_id_extension.is_some()
                    == self.single_extension_stream_flag
                {
                    return Err(Error::ElementaryStreamIdExtension {
                        elementary_stream_id: stream.elementary_stream_id,
                    });
                }
                if self.single_extension_stream_flag && has_extended_stream {
                    return Err(Error::SingleExtensionStream);
                }
                has_extended_stream = true;
            }
            Ok(stream)
        });

        write_or_truncate(out, |out| {
            let start = out.len();
            out.extend_from_slice(&PROGRAM_STREAM_MAP_START_CODE);
            out.extend_from_slice(&program_stream_map_length);
            let mut writer = FieldWriter::new(out);
            // Why not an error per field: every value fits its field's width.
            (|| {
                writer.write_bool(self.current_next_indicator)?;
                writer.write_bool(self.single_extension_stream_flag)?;
                writer.write_bool(true)?;
                writer.write_bits::<5>(self.program_stream_map_version.get())?;
                writer.write_bits::<8>(0xff_u8)?;
                writer.finish()
            })()
            .map_err(field_error("program_stream_map_version"))?;
            out.extend_from_slice(&program_stream_info_length);
            DescriptorLoop::write_entries(self.program_stream_info, out)?;
            out.extend_from_slice(&elementary_stream_map_length);
            ElementaryStreamLoop::write_entries(elementary_streams, out)?;
            let crc = crc_32(out.get(start..).unwrap_or_default());
            out.extend_from_slice(&crc.to_be_bytes());

            Ok(())
        })
    }
}

/// Refuses a map whose descriptors of the program are `program_stream_info`
/// and whose elementary stream loop would hold `elementary_streams`, its
/// `program_stream_map_length` larger than 1018, from the lengths alone
#[cfg(feature = "alloc")]
pub(super) fn check_lengths<'stream>(
    program_stream_info: DescriptorLoop<'_>,
    elementary_streams: impl IntoIterator<Item = ElementaryStream<'stream>>,
) -> Result<(), Error> {
    check_encoded_len(elementary_streams.into_iter().fold(
        OVERHEAD.saturating_add(program_stream_info.as_bytes().len()),
        |length, stream| length.saturating_add(stream.encoded_len()),
    ))
}

/// Refuses a map of `encoded_len` bytes, its `program_stream_map_length`
/// larger than 1018
#[cfg(feature = "alloc")]
fn check_encoded_len(encoded_len: usize) -> Result<(), Error> {
    let length = encoded_len.saturating_sub(PREFIX_LENGTH);
    if length > usize::from(MAX_PROGRAM_STREAM_MAP_LENGTH) {
        return Err(Error::ProgramStreamMapLength {
            program_stream_map_length: length,
            limit: MAX_PROGRAM_STREAM_MAP_LENGTH,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;
    use core::array;

    use super::{ElementaryStream, ElementaryStreamLoop, Error, ProgramStreamMap};
    use crate::descriptor::DescriptorLoop;
    use crate::field;
    use crate::pes::StreamId;
    use crate::psi::VersionNumber;
    use crate::stream_type::StreamType;

    /// Version 5, a registration descriptor `HDMV` for the program; MPEG-2
    /// video on 0xE0 with a `video_stream_descriptor`, MPEG-1 audio on 0xC0
    /// with an `ISO_639_language_descriptor`, and an elementary stream of
    /// `stream_id_extension` 0x55 on 0xFD
    const HAND_WRITTEN: [u8; 48] = [
        0x00, 0x00, 0x01, 0xbc, 0x00, 0x2a, 0xa5, 0xff, 0x00, 0x06, 0x05, 0x04, 0x48, 0x44, 0x4d,
        0x56, 0x00, 0x1a, 0x02, 0xe0, 0x00, 0x05, 0x02, 0x03, 0x22, 0x48, 0x5f, 0x03, 0xc0, 0x00,
        0x06, 0x0a, 0x04, 0x6a, 0x70, 0x6e, 0x00, 0x24, 0xfd, 0x00, 0x03, 0x01, 0x01, 0xd5, 0x50,
        0x53, 0x9e, 0x70,
    ];

    /// The map the previous muxer wrote: version 3, H.264 video on 0xE0 and
    /// AAC audio on 0xC0, `single_extension_stream_flag` set
    const PREVIOUS_MUXER: [u8; 24] = [
        0x00, 0x00, 0x01, 0xbc, 0x00, 0x12, 0xe3, 0xff, 0x00, 0x00, 0x00, 0x08, 0x1b, 0xe0, 0x00,
        0x00, 0x0f, 0xc0, 0x00, 0x00, 0x82, 0x5d, 0x94, 0x4b,
    ];

    fn version(value: u8) -> VersionNumber {
        VersionNumber::try_from(value).unwrap()
    }

    fn hand_written_streams() -> [ElementaryStream<'static>; 3] {
        let mut video =
            ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(0xe0));
        video.elementary_stream_info = DescriptorLoop::from(&HAND_WRITTEN[22..27]);
        let mut audio = ElementaryStream::new(StreamType::from(0x03), StreamId::from(0xc0));
        audio.elementary_stream_info = DescriptorLoop::from(&HAND_WRITTEN[31..37]);
        let mut extended =
            ElementaryStream::new(StreamType::from(0x24), StreamId::EXTENDED_STREAM_ID);
        extended.elementary_stream_id_extension = Some(0x55);
        [video, audio, extended]
    }

    fn hand_written(elementary_streams: &[u8]) -> ProgramStreamMap<'_> {
        let mut psm = ProgramStreamMap::new(version(5));
        psm.program_stream_info = DescriptorLoop::from(&HAND_WRITTEN[10..16]);
        psm.elementary_streams = ElementaryStreamLoop::from(elementary_streams);
        psm
    }

    #[test]
    fn a_map_laid_out_by_hand_is_read_with_its_streams() {
        let psm = ProgramStreamMap::read(&HAND_WRITTEN).unwrap();
        let mut entries = psm.elementary_streams.iter();

        assert_eq!(
            (psm, array::from_fn(|_| entries.next())),
            (
                hand_written(&HAND_WRITTEN[18..44]),
                hand_written_streams().map(|stream| Some(Ok(stream)))
            )
        );
        assert_eq!(entries.next(), None);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_map_built_from_its_streams_is_written_with_its_lengths_and_crc_32() {
        let mut streams = Vec::new();
        ElementaryStreamLoop::write_from(hand_written_streams(), &mut streams).unwrap();
        let psm = hand_written(&streams);
        let mut written = Vec::new();
        psm.write(&mut written).unwrap();

        assert_eq!(
            (written.as_slice(), psm.encoded_len()),
            (HAND_WRITTEN.as_slice(), HAND_WRITTEN.len())
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn the_map_the_previous_muxer_wrote_is_written_back_as_read() {
        let mut written = Vec::new();
        ProgramStreamMap::read(&PREVIOUS_MUXER)
            .unwrap()
            .write(&mut written)
            .unwrap();

        assert_eq!(written, PREVIOUS_MUXER);
    }

    #[test]
    fn marker_reserved_and_pseudo_descriptor_bits_read_as_0_are_not_checked() {
        let cleared = [
            0x00, 0x00, 0x01, 0xbc, 0x00, 0x2a, 0x85, 0x00, 0x00, 0x06, 0x05, 0x04, 0x48, 0x44,
            0x4d, 0x56, 0x00, 0x1a, 0x02, 0xe0, 0x00, 0x05, 0x02, 0x03, 0x22, 0x48, 0x5f, 0x03,
            0xc0, 0x00, 0x06, 0x0a, 0x04, 0x6a, 0x70, 0x6e, 0x00, 0x24, 0xfd, 0x00, 0x03, 0x01,
            0x01, 0x55, 0x76, 0xdd, 0x79, 0x19,
        ];
        let psm = ProgramStreamMap::read(&cleared).unwrap();
        let mut entries = psm.elementary_streams.iter();

        assert_eq!(
            (psm, array::from_fn(|_| entries.next())),
            (
                hand_written(&cleared[18..44]),
                hand_written_streams().map(|stream| Some(Ok(stream)))
            )
        );
    }

    #[test]
    fn a_map_whose_crc_32_does_not_match_is_not_read() {
        let mut corrupted = HAND_WRITTEN;
        corrupted[20] ^= 0x01;

        assert_eq!(
            ProgramStreamMap::read(&corrupted),
            Err(Error::Crc32 {
                crc_32: 0x5053_9e70,
                computed: crate::crc::crc_32(&corrupted[..44]),
            })
        );
    }

    #[test]
    fn bytes_that_are_not_one_whole_map_are_not_read() {
        let mut private_stream = PREVIOUS_MUXER;
        private_stream[3] = 0xbd;
        let mut too_long = PREVIOUS_MUXER;
        too_long[4..6].copy_from_slice(&0x03fb_u16.to_be_bytes());
        let longer_loop = [
            0x00, 0x00, 0x01, 0xbc, 0x00, 0x0e, 0xe0, 0xff, 0x00, 0x00, 0x00, 0x05, 0x02, 0xe0,
            0x00, 0x00, 0x6f, 0x42, 0xf5, 0xd6,
        ];
        let program_info_past_crc = [
            0x00, 0x00, 0x01, 0xbc, 0x00, 0x0a, 0xe0, 0xff, 0x00, 0x20, 0x00, 0x00, 0xd2, 0x85,
            0x37, 0x72,
        ];

        assert_eq!(
            [
                &private_stream[..],
                &too_long,
                &PREVIOUS_MUXER[..23],
                &longer_loop,
                &program_info_past_crc,
            ]
            .map(ProgramStreamMap::read),
            [
                Err(Error::PacketStartCode {
                    packet_start_code: 0x0000_01bd
                }),
                Err(Error::ProgramStreamMapLength {
                    program_stream_map_length: 1019,
                    limit: 1018
                }),
                Err(Error::Length {
                    program_stream_map_length: 18,
                    available_bytes: 17
                }),
                Err(Error::ElementaryStreamMapLength {
                    elementary_stream_map_length: 5,
                    available_bytes: 4
                }),
                Err(Error::Field {
                    field: "program_stream_info",
                    error: field::Error::Truncated {
                        required_bits: 256,
                        remaining_bits: 16
                    }
                }),
            ]
        );
    }

    #[test]
    fn an_entry_the_loop_ends_inside_is_an_error_after_the_entries_before() {
        let broken_tail = [
            0x00, 0x00, 0x01, 0xbc, 0x00, 0x14, 0xe0, 0xff, 0x00, 0x00, 0x00, 0x0a, 0x02, 0xe0,
            0x00, 0x00, 0x03, 0xc0, 0x00, 0x04, 0x0a, 0x04, 0x3b, 0xea, 0x66, 0x5e,
        ];
        let psm = ProgramStreamMap::read(&broken_tail).unwrap();
        let mut entries = psm.elementary_streams.iter();

        assert_eq!(
            array::from_fn(|_| entries.next()),
            [
                Some(Ok(ElementaryStream::new(
                    StreamType::ISO_IEC_13818_2_VIDEO,
                    StreamId::from(0xe0)
                ))),
                Some(Err(Error::Field {
                    field: "elementary_stream_info",
                    error: field::Error::Truncated {
                        required_bits: 32,
                        remaining_bits: 16
                    }
                })),
                None,
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn elementary_streams_are_written_only_with_values_the_specification_allows() {
        let video = ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(0xe0));
        let on = |stream_type: u8, stream_id: u8| {
            ElementaryStream::new(StreamType::from(stream_type), StreamId::from(stream_id))
        };
        let mut extension_on_video = video;
        extension_on_video.elementary_stream_id_extension = Some(0x01);
        let mut wide_extension = on(0x24, 0xfd);
        wide_extension.elementary_stream_id_extension = Some(0x80);
        let mut written = Vec::new();

        assert_eq!(
            [
                on(0x05, 0xe0),
                on(0x02, 0xb0),
                on(0x02, 0xbb),
                on(0x02, 0xbc),
                on(0x02, 0xbe),
                on(0x02, 0xff),
                extension_on_video,
                wide_extension,
            ]
            .map(|stream| ElementaryStreamLoop::write_from([video, stream], &mut written)),
            [
                Err(Error::Forbidden {
                    field: "stream_type",
                    value: 0x05
                }),
                Err(Error::Forbidden {
                    field: "elementary_stream_id",
                    value: 0xb0
                }),
                Err(Error::Forbidden {
                    field: "elementary_stream_id",
                    value: 0xbb
                }),
                Err(Error::Forbidden {
                    field: "elementary_stream_id",
                    value: 0xbc
                }),
                Err(Error::Forbidden {
                    field: "elementary_stream_id",
                    value: 0xbe
                }),
                Err(Error::Forbidden {
                    field: "elementary_stream_id",
                    value: 0xff
                }),
                Err(Error::ElementaryStreamIdExtension {
                    elementary_stream_id: StreamId::from(0xe0)
                }),
                Err(Error::Field {
                    field: "elementary_stream_id_extension",
                    error: field::Error::ValueTooWide {
                        width: 7,
                        value: 0x80
                    }
                }),
            ]
        );
        assert_eq!(written, []);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_stream_type_carried_in_sections_is_not_written_in_a_map() {
        let video = ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(0xe0));
        let section_types = [0x05, 0x13, 0x16, 0x2c, 0x2f, 0x30];
        let forbidden = |stream_type: u8| {
            Err(Error::Forbidden {
                field: "stream_type",
                value: u32::from(stream_type),
            })
        };
        let entries = section_types
            .map(|stream_type| [0x02, 0xe0, 0x00, 0x00, stream_type, 0xc0, 0x00, 0x00]);
        let mut written = Vec::new();

        assert_eq!(
            section_types.map(|stream_type| {
                let stream =
                    ElementaryStream::new(StreamType::from(stream_type), StreamId::from(0xc0));
                ElementaryStreamLoop::write_from([video, stream], &mut written)
            }),
            section_types.map(forbidden)
        );
        assert_eq!(
            entries.each_ref().map(|entries| {
                let mut psm = ProgramStreamMap::new(version(0));
                psm.elementary_streams = ElementaryStreamLoop::from(entries.as_slice());
                psm.write(&mut written)
            }),
            section_types.map(forbidden)
        );
        assert_eq!(written, []);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_map_is_written_only_with_values_the_specification_allows() {
        let map = |single_extension_stream_flag, elementary_streams: &'static [u8]| {
            let mut psm = ProgramStreamMap::new(version(0));
            psm.single_extension_stream_flag = single_extension_stream_flag;
            psm.elementary_streams = ElementaryStreamLoop::from(elementary_streams);
            psm
        };
        let long_info = [0_u8; 1009];
        let mut too_long = ProgramStreamMap::new(version(0));
        too_long.program_stream_info = DescriptorLoop::from(long_info.as_slice());
        let mut written = Vec::new();

        assert_eq!(
            [
                map(true, &[0x24, 0xfd, 0x00, 0x03, 0x01, 0x01, 0x81]),
                map(false, &[0x24, 0xfd, 0x00, 0x00]),
                map(true, &[0x24, 0xfd, 0x00, 0x00, 0x24, 0xfd, 0x00, 0x00]),
                too_long,
            ]
            .map(|psm| psm.write(&mut written)),
            [
                Err(Error::ElementaryStreamIdExtension {
                    elementary_stream_id: StreamId::EXTENDED_STREAM_ID
                }),
                Err(Error::ElementaryStreamIdExtension {
                    elementary_stream_id: StreamId::EXTENDED_STREAM_ID
                }),
                Err(Error::SingleExtensionStream),
                Err(Error::ProgramStreamMapLength {
                    program_stream_map_length: 1019,
                    limit: 1018
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
