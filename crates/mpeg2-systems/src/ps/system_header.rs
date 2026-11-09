//! System header: the bounds a Program Stream keeps to, and the buffer each
//! of its elementary streams needs

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use super::SYSTEM_HEADER_START_CODE;
use crate::field::{self, FieldReader};
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::pes::StreamId;

mod stream_bound;

#[cfg(feature = "alloc")]
use stream_bound::write_entries;
pub use stream_bound::{StreamBound, StreamBoundIter, StreamBoundLoop};

/// Bytes from `system_header_start_code` to `header_length`
const PREFIX_LENGTH: usize = 6;

/// Bytes from the `marker_bit` after `header_length` to `reserved_bits`
const FIXED_LENGTH: usize = 6;

/// Largest `audio_bound`
#[cfg(feature = "alloc")]
pub(crate) const MAX_AUDIO_BOUND: u8 = 32;

/// Largest `video_bound`
#[cfg(feature = "alloc")]
pub(crate) const MAX_VIDEO_BOUND: u8 = 16;

/// Failure to read or write a system header, or an entry of its stream
/// bound loop
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The bytes do not start with `system_header_start_code` 0x000001BB
    SystemHeaderStartCode {
        /// The 32 bits found
        system_header_start_code: u32,
    },
    /// The bytes after `header_length` are not as many as it declares
    HeaderLength {
        /// The `header_length` read
        header_length: u16,
        /// The bytes given after `header_length`
        available_bytes: usize,
    },
    /// A field runs past the end of the bytes, or does not fit its width
    /// when written
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field failed
        error: field::Error,
    },
    /// An entry of the stream bound loop starts with a bit `'0'`, which in
    /// the syntax ends the loop before `header_length` does
    StreamBoundStart {
        /// The byte the entry starts with
        stream_id: u8,
    },
    /// A value to write is one the specification forbids for its field
    Forbidden {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The value given
        value: u32,
    },
    /// A stream bound to write has a `stream_id_extension` where its
    /// `stream_id` is not 0xB7, or none where it is
    StreamIdExtension {
        /// The `stream_id` of the stream bound
        stream_id: StreamId,
    },
    /// A stream bound loop to write bounds a stream twice, by itself or by
    /// a bound for all audio, all video or all extended streams that covers
    /// it, where the specification has each stream bounded exactly once
    DuplicateStreamBound {
        /// The `stream_id` of the stream
        stream_id: StreamId,
        /// The `stream_id_extension` of the stream, if it has one
        stream_id_extension: Option<u8>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SystemHeaderStartCode {
                system_header_start_code,
            } => write!(
                formatter,
                "system_header_start_code {system_header_start_code:#010x} is not 0x000001bb"
            ),
            Self::HeaderLength {
                header_length,
                available_bytes,
            } => write!(
                formatter,
                "header_length {header_length} does not match the {available_bytes} bytes given"
            ),
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read or written")
            }
            Self::StreamBoundStart { stream_id } => write!(
                formatter,
                "stream bound entry starts with {stream_id:#04x}, whose first bit is not '1'"
            ),
            Self::Forbidden { field, value } => {
                write!(formatter, "{field} {value} is forbidden")
            }
            Self::StreamIdExtension { stream_id } => write!(
                formatter,
                "stream_id {:#04x} and the presence of stream_id_extension disagree",
                stream_id.get()
            ),
            Self::DuplicateStreamBound {
                stream_id,
                stream_id_extension,
            } => match stream_id_extension {
                Some(extension) => write!(
                    formatter,
                    "stream_id {:#04x} with stream_id_extension {extension:#04x} bounds a stream \
                     already bounded",
                    stream_id.get()
                ),
                None => write!(
                    formatter,
                    "stream_id {:#04x} bounds a stream already bounded",
                    stream_id.get()
                ),
            },
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::SystemHeaderStartCode { .. }
            | Self::HeaderLength { .. }
            | Self::StreamBoundStart { .. }
            | Self::Forbidden { .. }
            | Self::StreamIdExtension { .. }
            | Self::DuplicateStreamBound { .. } => None,
        }
    }
}

/// Returns a mapping of a failure on `field` to an [`Error::Field`]
const fn field_error(field: &'static str) -> impl Fn(field::Error) -> Error {
    move |error| Error::Field { field, error }
}

/// Header that states the bounds a Program Stream keeps to, and the
/// buffer each of its elementary streams needs
///
/// `system_header`, ISO/IEC 13818-1 §2.5.3.5
///
/// A system header follows a pack header, read as a [`PackHeader`] from its
/// own bytes; finding where either ends in a stream is a framer's work,
/// which this type does not do. `header_length` is derived from the fields.
/// The `marker_bit`s and the `reserved_bits` are not checked when read and
/// are written as `'1'`.
///
/// [`PackHeader`]: super::PackHeader
///
/// # Examples
///
/// ```
/// use mpeg2_systems::pes::StreamId;
/// use mpeg2_systems::ps::{StreamBound, SystemHeader};
///
/// // One video stream with a 230 KiB buffer and one audio stream with 4 KiB
/// let bytes = [
///     0x00, 0x00, 0x01, 0xbb, 0x00, 0x0c, 0xa1, 0x9b, 0x1d, 0x04, 0x21, 0xff, 0xe0, 0xe0, 0xe6,
///     0xc0, 0xc0, 0x20,
/// ];
/// let system_header = SystemHeader::read(&bytes)?;
/// let mut stream_bounds = system_header.stream_bounds.iter();
/// assert_eq!(
///     (system_header.rate_bound, system_header.audio_bound, system_header.video_bound),
///     (1_101_198, 1, 1)
/// );
/// assert_eq!(
///     [stream_bounds.next(), stream_bounds.next(), stream_bounds.next()],
///     [
///         Some(Ok(StreamBound::new(StreamId::from(0xe0), true, 230))),
///         Some(Ok(StreamBound::new(StreamId::from(0xc0), false, 32))),
///         None,
///     ]
/// );
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[doc(alias = "system_header")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SystemHeader<'input> {
    /// `rate_bound`: at least the largest `program_mux_rate` of any pack of
    /// the stream, in units of 50 bytes per second
    pub rate_bound: u32,
    /// `audio_bound`: at least the number of audio streams decoded at once,
    /// 0 to 32
    pub audio_bound: u8,
    /// `fixed_flag`: the stream has a fixed bitrate
    pub fixed_flag: bool,
    /// `CSPS_flag`: the stream is a constrained system parameter stream
    pub csps_flag: bool,
    /// `system_audio_lock_flag`: the audio sampling rate is locked to the
    /// system clock
    pub system_audio_lock_flag: bool,
    /// `system_video_lock_flag`: the video time base is locked to the
    /// system clock
    pub system_video_lock_flag: bool,
    /// `video_bound`: at least the number of video streams decoded at once,
    /// 0 to 16
    pub video_bound: u8,
    /// `packet_rate_restriction_flag`: which packet rate constraint applies
    /// when `csps_flag` is set
    pub packet_rate_restriction_flag: bool,
    /// The buffer bound of each elementary stream
    pub stream_bounds: StreamBoundLoop<'input>,
}

impl SystemHeader<'_> {
    /// Creates a system header with no flag set and no stream bounds
    #[must_use]
    pub const fn new(rate_bound: u32, audio_bound: u8, video_bound: u8) -> Self {
        Self {
            rate_bound,
            audio_bound,
            fixed_flag: false,
            csps_flag: false,
            system_audio_lock_flag: false,
            system_video_lock_flag: false,
            video_bound,
            packet_rate_restriction_flag: false,
            stream_bounds: StreamBoundLoop(&[]),
        }
    }

    /// Returns `header_length`, the bytes after it
    #[must_use]
    pub const fn header_length(&self) -> usize {
        FIXED_LENGTH.saturating_add(self.stream_bounds.0.len())
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        PREFIX_LENGTH.saturating_add(self.header_length())
    }
}

impl<'input> SystemHeader<'input> {
    /// Reads a system header from `bytes`, which hold it and nothing else
    ///
    /// The stream bound loop is read entry by entry as it is iterated.
    ///
    /// # Errors
    ///
    /// * [`SystemHeaderStartCode`](Error::SystemHeaderStartCode): the bytes
    ///   do not start with 0x000001BB.
    /// * [`HeaderLength`](Error::HeaderLength): the bytes after
    ///   `header_length` are not as many as it declares.
    /// * [`Field`](Error::Field): the bytes end before `reserved_bits`.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        let mut reader = FieldReader::new(bytes);
        let system_header_start_code = reader
            .read_u32::<32>()
            .map_err(field_error("system_header_start_code"))?;
        if system_header_start_code.to_be_bytes() != SYSTEM_HEADER_START_CODE {
            return Err(Error::SystemHeaderStartCode {
                system_header_start_code,
            });
        }
        let header_length = reader
            .read_u16::<16>()
            .map_err(field_error("header_length"))?;
        let available_bytes = reader.remaining_bytes();
        if available_bytes != usize::from(header_length) {
            return Err(Error::HeaderLength {
                header_length,
                available_bytes,
            });
        }

        let mut rate_bound = || {
            reader.read_bits::<1>()?;
            let rate = reader.read_u32::<22>()?;
            reader.read_bits::<1>()?;
            Ok(rate)
        };
        let rate_bound = rate_bound().map_err(field_error("rate_bound"))?;
        let audio_bound = reader.read_u8::<6>().map_err(field_error("audio_bound"))?;
        let mut flag = |name| reader.read_bool().map_err(field_error(name));
        let fixed_flag = flag("fixed_flag")?;
        let csps_flag = flag("CSPS_flag")?;
        let system_audio_lock_flag = flag("system_audio_lock_flag")?;
        let system_video_lock_flag = flag("system_video_lock_flag")?;
        let video_bound = reader
            .read_bits::<1>()
            .and_then(|_| reader.read_u8::<5>())
            .map_err(field_error("video_bound"))?;
        let packet_rate_restriction_flag = reader
            .read_bool()
            .map_err(field_error("packet_rate_restriction_flag"))?;
        reader
            .read_bits::<7>()
            .map_err(field_error("reserved_bits"))?;
        // Why not an error: every field before the loop ends on a byte
        // boundary.
        let stream_bounds = reader
            .read_bytes(reader.remaining_bytes())
            .unwrap_or_default();

        Ok(Self {
            rate_bound,
            audio_bound,
            fixed_flag,
            csps_flag,
            system_audio_lock_flag,
            system_video_lock_flag,
            video_bound,
            packet_rate_restriction_flag,
            stream_bounds: StreamBoundLoop(stream_bounds),
        })
    }

    /// Appends the system header to `out`, its `marker_bit`s and
    /// `reserved_bits` `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `rate_bound` is 0, `audio_bound`
    ///   is more than 32 or `video_bound` more than 16; or a stream bound is
    ///   refused as [`StreamBoundLoop::write_from`] refuses it.
    /// * [`DuplicateStreamBound`](Error::DuplicateStreamBound): a stream is
    ///   bounded twice, by itself or by a group bound that covers it.
    /// * [`StreamBoundStart`](Error::StreamBoundStart),
    ///   [`Field`](Error::Field): the stream bound loop ends inside an entry
    ///   or holds bytes that are not an entry; or `rate_bound` does not fit
    ///   22 bits.
    /// * Any error of [`StreamBoundLoop::write_from`].
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::pes::StreamId;
    /// use mpeg2_systems::ps::{StreamBound, StreamBoundLoop, SystemHeader};
    ///
    /// // A stream of one video stream with a 230 KiB buffer
    /// let mut stream_bounds = Vec::new();
    /// StreamBoundLoop::write_from(
    ///     [StreamBound::new(StreamId::from(0xe0), true, 230)],
    ///     &mut stream_bounds,
    /// )?;
    /// let mut system_header = SystemHeader::new(1_101_198, 0, 1);
    /// system_header.stream_bounds = StreamBoundLoop::from(stream_bounds.as_slice());
    ///
    /// let mut written = Vec::new();
    /// system_header.write(&mut written)?;
    /// assert_eq!(
    ///     written,
    ///     [0x00, 0x00, 0x01, 0xbb, 0x00, 0x09, 0xa1, 0x9b, 0x1d, 0x00, 0x21, 0x7f, 0xe0, 0xe0, 0xe6]
    /// );
    /// # }
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.rate_bound == 0 {
            return Err(Error::Forbidden {
                field: "rate_bound",
                value: 0,
            });
        }
        if self.audio_bound > MAX_AUDIO_BOUND {
            return Err(Error::Forbidden {
                field: "audio_bound",
                value: u32::from(self.audio_bound),
            });
        }
        if self.video_bound > MAX_VIDEO_BOUND {
            return Err(Error::Forbidden {
                field: "video_bound",
                value: u32::from(self.video_bound),
            });
        }
        // Why not an error: a loop of more than 65 529 bytes bounds some
        // stream twice, the bounds of distinct streams taking 978 bytes at
        // most, and `write_entries` refuses it.
        let header_length = u16::try_from(self.header_length()).unwrap_or(u16::MAX);

        write_or_truncate(out, |out| {
            let mut writer = FieldWriter::new(out);
            writer
                .write_bytes(&SYSTEM_HEADER_START_CODE)
                .map_err(field_error("system_header_start_code"))?;
            writer
                .write_bits::<16>(header_length)
                .map_err(field_error("header_length"))?;
            (|| {
                writer.write_bool(true)?;
                writer.write_bits::<22>(self.rate_bound)?;
                writer.write_bool(true)
            })()
            .map_err(field_error("rate_bound"))?;
            // Why not an error per field: every value from here fits its
            // field's width, the bounds having been checked above.
            (|| {
                writer.write_bits::<6>(self.audio_bound)?;
                writer.write_bool(self.fixed_flag)?;
                writer.write_bool(self.csps_flag)?;
                writer.write_bool(self.system_audio_lock_flag)?;
                writer.write_bool(self.system_video_lock_flag)?;
                writer.write_bool(true)?;
                writer.write_bits::<5>(self.video_bound)?;
                writer.write_bool(self.packet_rate_restriction_flag)?;
                writer.write_bits::<7>(0x7f_u8)?;
                writer.finish()
            })()
            .map_err(field_error("system_header"))?;
            write_entries(self.stream_bounds.iter(), out)
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;
    use core::array;

    use super::{Error, StreamBound, StreamBoundLoop, SystemHeader};
    use crate::field;
    use crate::pes::StreamId;

    const PROGRAM_STREAM: &[u8] = include_bytes!("../../tests/fixtures/program.ps");

    /// Rate bound 20 000, 2 audio and 1 video streams, `fixed_flag`,
    /// `system_audio_lock_flag` and `system_video_lock_flag`; all audio
    /// streams within 4 KiB, all video streams within 232 KiB, and the stream
    /// of `stream_id_extension` 0x55 within 100 KiB
    const THREE_BOUNDS: [u8; 24] = [
        0x00, 0x00, 0x01, 0xbb, 0x00, 0x12, 0x80, 0x9c, 0x41, 0x0a, 0xe1, 0x7f, 0xb8, 0xc0, 0x20,
        0xb9, 0xe0, 0xe8, 0xb7, 0xc0, 0x55, 0xb6, 0xe0, 0x64,
    ];

    fn stream_id(value: u8) -> StreamId {
        StreamId::from(value)
    }

    fn three_bounds() -> [StreamBound; 3] {
        let mut extended = StreamBound::new(StreamBound::WITH_STREAM_ID_EXTENSION, true, 100);
        extended.stream_id_extension = Some(0x55);
        [
            StreamBound::new(StreamBound::ALL_AUDIO_STREAMS, false, 32),
            StreamBound::new(StreamBound::ALL_VIDEO_STREAMS, true, 232),
            extended,
        ]
    }

    fn three_bounds_header(stream_bounds: &[u8]) -> SystemHeader<'_> {
        let mut system_header = SystemHeader::new(20_000, 2, 1);
        system_header.fixed_flag = true;
        system_header.system_audio_lock_flag = true;
        system_header.system_video_lock_flag = true;
        system_header.stream_bounds = StreamBoundLoop::from(stream_bounds);
        system_header
    }

    /// Returns the system header of the fixture, the 18 bytes after its
    /// first pack header
    fn fixture_system_header() -> &'static [u8] {
        PROGRAM_STREAM.get(14..32).unwrap()
    }

    #[test]
    fn the_system_header_of_an_ffmpeg_program_stream_is_read_with_its_stream_bounds() {
        let system_header = SystemHeader::read(fixture_system_header()).unwrap();
        let mut expected = SystemHeader::new(1_101_198, 1, 1);
        expected.packet_rate_restriction_flag = true;
        expected.stream_bounds = StreamBoundLoop::from(fixture_system_header().get(12..).unwrap());
        let mut entries = system_header.stream_bounds.iter();

        assert_eq!(
            (system_header, array::from_fn(|_| entries.next())),
            (
                expected,
                [
                    Some(Ok(StreamBound::new(stream_id(0xe0), true, 230))),
                    Some(Ok(StreamBound::new(stream_id(0xc0), false, 32))),
                    None,
                ]
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn the_system_header_of_an_ffmpeg_program_stream_is_written_back_as_read() {
        let bytes = fixture_system_header();
        let mut written = Vec::new();
        SystemHeader::read(bytes)
            .unwrap()
            .write(&mut written)
            .unwrap();

        assert_eq!(written, bytes);
    }

    #[test]
    fn stream_bounds_for_all_audio_all_video_and_an_extended_stream_id_are_read() {
        let system_header = SystemHeader::read(&THREE_BOUNDS).unwrap();
        let mut entries = system_header.stream_bounds.iter();

        assert_eq!(
            (system_header, array::from_fn(|_| entries.next())),
            (
                three_bounds_header(&THREE_BOUNDS[12..]),
                three_bounds().map(|bound| Some(Ok(bound)))
            )
        );
        assert_eq!(entries.next(), None);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_system_header_built_from_its_stream_bounds_is_written_with_its_header_length() {
        let mut stream_bounds = Vec::new();
        StreamBoundLoop::write_from(three_bounds(), &mut stream_bounds).unwrap();
        let system_header = three_bounds_header(&stream_bounds);
        let mut written = Vec::new();
        system_header.write(&mut written).unwrap();

        assert_eq!(
            (written.as_slice(), system_header.encoded_len()),
            (THREE_BOUNDS.as_slice(), THREE_BOUNDS.len())
        );
    }

    #[test]
    fn marker_fixed_and_reserved_bits_read_as_0_are_not_checked() {
        let cleared = [
            0x00, 0x00, 0x01, 0xbb, 0x00, 0x12, 0x00, 0x9c, 0x40, 0x0a, 0xc1, 0x00, 0xb8, 0x00,
            0x20, 0xb9, 0x20, 0xe8, 0xb7, 0x00, 0x55, 0x00, 0x20, 0x64,
        ];
        let system_header = SystemHeader::read(&cleared).unwrap();
        let mut entries = system_header.stream_bounds.iter();

        assert_eq!(
            (system_header, array::from_fn(|_| entries.next())),
            (
                three_bounds_header(&cleared[12..]),
                three_bounds().map(|bound| Some(Ok(bound)))
            )
        );
    }

    #[test]
    fn bytes_that_are_not_one_whole_system_header_are_not_read() {
        let mut pack_header = THREE_BOUNDS;
        pack_header[3] = 0xba;

        assert_eq!(
            [
                &pack_header[..],
                &THREE_BOUNDS[..23],
                &[THREE_BOUNDS.as_slice(), &[0xe0]].concat(),
                &[0x00, 0x00, 0x01, 0xbb, 0x00, 0x03, 0x80, 0x9c, 0x41],
            ]
            .map(SystemHeader::read),
            [
                Err(Error::SystemHeaderStartCode {
                    system_header_start_code: 0x0000_01ba
                }),
                Err(Error::HeaderLength {
                    header_length: 18,
                    available_bytes: 17
                }),
                Err(Error::HeaderLength {
                    header_length: 18,
                    available_bytes: 19
                }),
                Err(Error::Field {
                    field: "audio_bound",
                    error: field::Error::Truncated {
                        required_bits: 6,
                        remaining_bits: 0
                    }
                }),
            ]
        );
    }

    #[test]
    fn an_entry_cut_short_or_not_starting_with_a_bit_1_ends_the_loop() {
        let first_entry = |bytes: &'static [u8]| StreamBoundLoop::from(bytes).iter().next();
        let mut after_error = StreamBoundLoop::from([0x7f, 0xe0, 0xe0, 0xe6].as_slice()).iter();

        assert_eq!(
            [
                first_entry(&[0xe0, 0xe0]),
                first_entry(&[0xb7, 0xc0]),
                after_error.next(),
                after_error.next(),
            ],
            [
                Some(Err(Error::Field {
                    field: "P-STD_buffer_size_bound",
                    error: field::Error::Truncated {
                        required_bits: 13,
                        remaining_bits: 5
                    }
                })),
                Some(Err(Error::Field {
                    field: "stream_id_extension",
                    error: field::Error::Truncated {
                        required_bits: 9,
                        remaining_bits: 8
                    }
                })),
                Some(Err(Error::StreamBoundStart { stream_id: 0x7f })),
                None,
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn stream_bounds_are_written_only_with_values_the_specification_allows() {
        let mut extension_on_video = StreamBound::new(stream_id(0xe0), true, 1);
        extension_on_video.stream_id_extension = Some(0x55);
        let mut wide_extension = StreamBound::new(stream_id(0xb7), true, 1);
        wide_extension.stream_id_extension = Some(0x80);
        let mut written = Vec::new();

        assert_eq!(
            [
                StreamBound::new(stream_id(0xbb), false, 1),
                StreamBound::new(stream_id(0xb7), true, 1),
                extension_on_video,
                StreamBound::new(stream_id(0xc0), true, 1),
                StreamBound::new(stream_id(0xb8), true, 1),
                StreamBound::new(stream_id(0xe0), false, 1),
                StreamBound::new(stream_id(0xb9), false, 1),
                wide_extension,
                StreamBound::new(stream_id(0xbd), false, 0x2000),
                StreamBound::new(stream_id(0xbd), false, 1),
            ]
            .map(|bound| StreamBoundLoop::write_from(
                [StreamBound::new(stream_id(0xbd), true, 1), bound],
                &mut written
            )),
            [
                Err(Error::Forbidden {
                    field: "stream_id",
                    value: 0xbb
                }),
                Err(Error::StreamIdExtension {
                    stream_id: stream_id(0xb7)
                }),
                Err(Error::StreamIdExtension {
                    stream_id: stream_id(0xe0)
                }),
                Err(Error::Forbidden {
                    field: "P-STD_buffer_bound_scale",
                    value: 1
                }),
                Err(Error::Forbidden {
                    field: "P-STD_buffer_bound_scale",
                    value: 1
                }),
                Err(Error::Forbidden {
                    field: "P-STD_buffer_bound_scale",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "P-STD_buffer_bound_scale",
                    value: 0
                }),
                Err(Error::Field {
                    field: "stream_id_extension",
                    error: field::Error::ValueTooWide {
                        width: 7,
                        value: 0x80
                    }
                }),
                Err(Error::Field {
                    field: "P-STD_buffer_size_bound",
                    error: field::Error::ValueTooWide {
                        width: 13,
                        value: 0x2000
                    }
                }),
                Err(Error::DuplicateStreamBound {
                    stream_id: stream_id(0xbd),
                    stream_id_extension: None
                }),
            ]
        );
        assert_eq!(written, []);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_stream_a_group_bound_covers_is_not_bounded_again() {
        let mut extended = StreamBound::new(StreamBound::WITH_STREAM_ID_EXTENSION, false, 1);
        extended.stream_id_extension = Some(0x01);
        let all_audio = StreamBound::new(StreamBound::ALL_AUDIO_STREAMS, false, 1);
        let all_video = StreamBound::new(StreamBound::ALL_VIDEO_STREAMS, true, 1);
        let all_extended = StreamBound::new(StreamId::EXTENDED_STREAM_ID, false, 1);
        let audio = StreamBound::new(stream_id(0xdf), false, 1);
        let video = StreamBound::new(stream_id(0xe0), true, 1);
        let duplicate = |bound: StreamBound| {
            Err(Error::DuplicateStreamBound {
                stream_id: bound.stream_id,
                stream_id_extension: bound.stream_id_extension,
            })
        };
        let mut written = Vec::new();

        assert_eq!(
            [
                [all_audio, audio],
                [audio, all_audio],
                [all_video, video],
                [video, all_video],
                [all_extended, extended],
                [extended, all_extended],
                [all_audio, video],
                [all_extended, audio],
                [all_audio, all_video],
            ]
            .map(|bounds| StreamBoundLoop::write_from(bounds, &mut written)),
            [
                duplicate(audio),
                duplicate(all_audio),
                duplicate(video),
                duplicate(all_video),
                duplicate(extended),
                duplicate(all_extended),
                Ok(()),
                Ok(()),
                Ok(()),
            ]
        );
        assert_eq!(
            written,
            [
                0xb8, 0xc0, 0x01, 0xe0, 0xe0, 0x01, 0xfd, 0xc0, 0x01, 0xdf, 0xc0, 0x01, 0xb8, 0xc0,
                0x01, 0xb9, 0xe0, 0x01,
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_system_header_is_written_only_with_values_the_specification_allows() {
        let twice = [0xe0, 0xe0, 0xe6, 0xc0, 0xc0, 0x20, 0xe0, 0xe0, 0x10];
        let mut duplicate = SystemHeader::new(1, 1, 1);
        duplicate.stream_bounds = StreamBoundLoop::from(twice.as_slice());
        let mut cut_short = SystemHeader::new(1, 1, 1);
        cut_short.stream_bounds = StreamBoundLoop::from([0xe0, 0xe0].as_slice());
        let mut written = Vec::new();

        assert_eq!(
            [
                SystemHeader::new(0, 1, 1),
                SystemHeader::new(1, 33, 1),
                SystemHeader::new(1, 1, 17),
                SystemHeader::new(1 << 22, 1, 1),
                duplicate,
                cut_short,
            ]
            .map(|system_header| system_header.write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "rate_bound",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "audio_bound",
                    value: 33
                }),
                Err(Error::Forbidden {
                    field: "video_bound",
                    value: 17
                }),
                Err(Error::Field {
                    field: "rate_bound",
                    error: field::Error::ValueTooWide {
                        width: 22,
                        value: 1 << 22
                    }
                }),
                Err(Error::DuplicateStreamBound {
                    stream_id: stream_id(0xe0),
                    stream_id_extension: None
                }),
                Err(Error::Field {
                    field: "P-STD_buffer_size_bound",
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
