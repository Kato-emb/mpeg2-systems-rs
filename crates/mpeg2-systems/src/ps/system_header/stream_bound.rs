//! Stream bound loop of the system header: the buffer each elementary
//! stream, or group of them, needs at most

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{Error, field_error};
use crate::field::FieldReader;
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::pes::StreamId;

/// The audio `stream_id`s 0xC0 to 0xDF, as bits at their low 7 bits
#[cfg(feature = "alloc")]
const AUDIO_STREAMS: u128 = 0xffff_ffff_u128.wrapping_shl(0x40);

/// The video `stream_id`s 0xE0 to 0xEF, as bits at their low 7 bits
#[cfg(feature = "alloc")]
const VIDEO_STREAMS: u128 = 0xffff_u128.wrapping_shl(0x60);

/// Entry of the stream bound loop of a [`SystemHeader`](super::SystemHeader): the buffer an
/// elementary stream, or a group of them, needs at most
///
/// Besides the `stream_id` of a stream, the entry may name
/// [`ALL_AUDIO_STREAMS`](Self::ALL_AUDIO_STREAMS),
/// [`ALL_VIDEO_STREAMS`](Self::ALL_VIDEO_STREAMS), or
/// [`WITH_STREAM_ID_EXTENSION`](Self::WITH_STREAM_ID_EXTENSION) followed by a
/// `stream_id_extension` naming the stream; 0xFD bounds all streams with an
/// `extended_stream_id`, ISO/IEC 13818-1 §2.5.3.6.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamBound {
    /// `stream_id` of the stream or group of streams bounded
    pub stream_id: StreamId,
    /// `stream_id_extension`, present when `stream_id` is 0xB7
    pub stream_id_extension: Option<u8>,
    /// `P-STD_buffer_bound_scale`: `p_std_buffer_size_bound` counts units
    /// of 1024 bytes when set and of 128 bytes when not; set for video,
    /// clear for audio
    pub p_std_buffer_bound_scale: bool,
    /// `P-STD_buffer_size_bound`: at least the largest input buffer the
    /// stream needs, in the units `p_std_buffer_bound_scale` gives
    pub p_std_buffer_size_bound: u16,
}

impl StreamBound {
    /// `stream_id` 0xB7: the entry bounds the stream its
    /// `stream_id_extension` names
    pub const WITH_STREAM_ID_EXTENSION: StreamId = StreamId::from_bits_truncate(0xb7);
    /// `stream_id` 0xB8: the entry bounds all audio streams
    pub const ALL_AUDIO_STREAMS: StreamId = StreamId::from_bits_truncate(0xb8);
    /// `stream_id` 0xB9: the entry bounds all video streams
    pub const ALL_VIDEO_STREAMS: StreamId = StreamId::from_bits_truncate(0xb9);

    /// Creates a stream bound without `stream_id_extension`
    #[must_use]
    pub const fn new(
        stream_id: StreamId,
        p_std_buffer_bound_scale: bool,
        p_std_buffer_size_bound: u16,
    ) -> Self {
        Self {
            stream_id,
            stream_id_extension: None,
            p_std_buffer_bound_scale,
            p_std_buffer_size_bound,
        }
    }

    /// Returns the value with `stream_id_extension` present, set to the one given
    #[must_use]
    pub const fn with_stream_id_extension(mut self, stream_id_extension: u8) -> Self {
        self.stream_id_extension = Some(stream_id_extension);
        self
    }

    /// Refuses a stream bound the specification does not allow
    #[cfg(feature = "alloc")]
    fn check(self) -> Result<Self, Error> {
        let stream_id = self.stream_id;
        if !stream_id.is_pes_packet()
            && ![
                Self::WITH_STREAM_ID_EXTENSION,
                Self::ALL_AUDIO_STREAMS,
                Self::ALL_VIDEO_STREAMS,
            ]
            .contains(&stream_id)
        {
            return Err(Error::Forbidden {
                field: "stream_id",
                value: u32::from(stream_id.get()),
            });
        }
        if self.stream_id_extension.is_some() != (stream_id == Self::WITH_STREAM_ID_EXTENSION) {
            return Err(Error::StreamIdExtension { stream_id });
        }
        let is_audio = stream_id.is_audio() || stream_id == Self::ALL_AUDIO_STREAMS;
        let is_video = stream_id.is_video() || stream_id == Self::ALL_VIDEO_STREAMS;
        if (is_audio && self.p_std_buffer_bound_scale)
            || (is_video && !self.p_std_buffer_bound_scale)
        {
            return Err(Error::Forbidden {
                field: "P-STD_buffer_bound_scale",
                value: u32::from(self.p_std_buffer_bound_scale),
            });
        }

        Ok(self)
    }
}

/// The stream bound loop of a [`SystemHeader`](super::SystemHeader), borrowed
///
/// The bytes are read entry by entry as [`iter`](Self::iter) walks them, so
/// a view of any bytes can be made; an entry the bytes end inside is an
/// error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamBoundLoop<'input>(pub(super) &'input [u8]);

impl<'input> StreamBoundLoop<'input> {
    /// Returns the bytes of the loop
    #[must_use]
    pub const fn as_bytes(self) -> &'input [u8] {
        self.0
    }

    /// Returns an iterator over the entries, each read as it is reached
    #[must_use]
    pub const fn iter(self) -> StreamBoundIter<'input> {
        StreamBoundIter(self.0)
    }

    /// Appends the bytes of a loop of `stream_bounds` to `out`, their fixed
    /// bits as the syntax gives them
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): a `stream_id` below 0xBC other
    ///   than 0xB7, 0xB8 and 0xB9; or a `P-STD_buffer_bound_scale` other
    ///   than `'0'` for audio or `'1'` for video.
    /// * [`StreamIdExtension`](Error::StreamIdExtension): a
    ///   `stream_id_extension` where `stream_id` is not 0xB7, or none where
    ///   it is.
    /// * [`Field`](Error::Field): a `stream_id_extension` that does not fit
    ///   7 bits or a `P-STD_buffer_size_bound` that does not fit 13.
    /// * [`DuplicateStreamBound`](Error::DuplicateStreamBound): a stream
    ///   bounded twice, by itself or by a bound for all audio, all video or
    ///   all extended streams that covers it.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::ps::{StreamBound, StreamBoundLoop};
    ///
    /// // All audio streams within 4 KiB, all video streams within 232 KiB
    /// let mut written = Vec::new();
    /// StreamBoundLoop::write_from(
    ///     [
    ///         StreamBound::new(StreamBound::ALL_AUDIO_STREAMS, false, 32),
    ///         StreamBound::new(StreamBound::ALL_VIDEO_STREAMS, true, 232),
    ///     ],
    ///     &mut written,
    /// )?;
    /// assert_eq!(written, [0xb8, 0xc0, 0x20, 0xb9, 0xe0, 0xe8]);
    /// # }
    /// # Ok::<(), mpeg2_systems::ps::system_header::Error>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from(
        stream_bounds: impl IntoIterator<Item = StreamBound>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        write_entries(stream_bounds.into_iter().map(Ok), out)
    }
}

/// Appends the bytes of `entries` to `out`, leaving `out` as it was if an
/// entry is an error or is refused
#[cfg(feature = "alloc")]
pub(super) fn write_entries(
    entries: impl IntoIterator<Item = Result<StreamBound, Error>>,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    write_or_truncate(out, |out| {
        // Why not the whole `stream_id`: `check` lets through only values
        // with the first bit set, so the other 7 bits tell them apart, as the
        // 7 bits of a `stream_id_extension` that fit its field do.
        let bit = |value: u8| 1_u128.wrapping_shl(u32::from(value & 0x7f));
        let mut bounded = (0_u128, 0_u128);
        for stream_bound in entries {
            let stream_bound = stream_bound.and_then(StreamBound::check)?;
            let mut writer = FieldWriter::new(out);
            writer
                .write_bits::<8>(stream_bound.stream_id.get())
                .map_err(field_error("stream_id"))?;
            if let Some(extension) = stream_bound.stream_id_extension {
                (|| {
                    writer.write_bits::<9>(0b1_1000_0000_u16)?;
                    writer.write_bits::<7>(extension)?;
                    writer.write_bits::<8>(0xb6_u8)
                })()
                .map_err(field_error("stream_id_extension"))?;
            }
            (|| {
                writer.write_bits::<2>(0b11_u8)?;
                writer.write_bool(stream_bound.p_std_buffer_bound_scale)?;
                writer.write_bits::<13>(stream_bound.p_std_buffer_size_bound)?;
                writer.finish()
            })()
            .map_err(field_error("P-STD_buffer_size_bound"))?;
            let stream_id = stream_bound.stream_id;
            let covered = match stream_bound.stream_id_extension {
                Some(extension) => (0, bit(extension)),
                None if stream_id == StreamBound::ALL_AUDIO_STREAMS => {
                    (bit(stream_id.get()) | AUDIO_STREAMS, 0)
                }
                None if stream_id == StreamBound::ALL_VIDEO_STREAMS => {
                    (bit(stream_id.get()) | VIDEO_STREAMS, 0)
                }
                None if stream_id == StreamId::EXTENDED_STREAM_ID => {
                    (bit(stream_id.get()), u128::MAX)
                }
                None => (bit(stream_id.get()), 0),
            };
            if covered.0 & bounded.0 != 0 || covered.1 & bounded.1 != 0 {
                return Err(Error::DuplicateStreamBound {
                    stream_id,
                    stream_id_extension: stream_bound.stream_id_extension,
                });
            }
            bounded = (bounded.0 | covered.0, bounded.1 | covered.1);
        }

        Ok(())
    })
}

impl<'input> From<&'input [u8]> for StreamBoundLoop<'input> {
    fn from(bytes: &'input [u8]) -> Self {
        Self(bytes)
    }
}

impl<'input> IntoIterator for StreamBoundLoop<'input> {
    type Item = Result<StreamBound, Error>;
    type IntoIter = StreamBoundIter<'input>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the entries of a [`StreamBoundLoop`]
///
/// An entry the bytes end inside is an [`Error::Field`], and one that
/// starts with a bit `'0'` an [`Error::StreamBoundStart`]; either is the
/// last item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamBoundIter<'input>(&'input [u8]);

impl Iterator for StreamBoundIter<'_> {
    type Item = Result<StreamBound, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }

        let mut reader = FieldReader::new(self.0);
        let entry = (|| {
            let stream_id = reader.read_u8::<8>().map_err(field_error("stream_id"))?;
            if stream_id & 0x80 == 0 {
                return Err(Error::StreamBoundStart { stream_id });
            }
            let stream_id_extension = if stream_id == StreamBound::WITH_STREAM_ID_EXTENSION.get() {
                let mut extension = || {
                    reader.read_bits::<9>()?;
                    let extension = reader.read_u8::<7>()?;
                    reader.read_bits::<8>()?;
                    Ok(extension)
                };
                Some(extension().map_err(field_error("stream_id_extension"))?)
            } else {
                None
            };
            let mut buffer_bound = || {
                reader.read_bits::<2>()?;
                Ok((reader.read_bool()?, reader.read_u16::<13>()?))
            };
            let (p_std_buffer_bound_scale, p_std_buffer_size_bound) =
                buffer_bound().map_err(field_error("P-STD_buffer_size_bound"))?;
            Ok(StreamBound {
                stream_id: StreamId::from(stream_id),
                stream_id_extension,
                p_std_buffer_bound_scale,
                p_std_buffer_size_bound,
            })
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
