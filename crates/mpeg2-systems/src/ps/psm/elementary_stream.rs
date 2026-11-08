//! Elementary stream loop of the program stream map: each elementary stream,
//! the `stream_id` of the PES packets that carry it, and its descriptors

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{Error, field_error};
use crate::descriptor::DescriptorLoop;
use crate::field::FieldReader;
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::pes::StreamId;
use crate::stream_type::StreamType;

/// `pseudo_descriptor_tag` and `pseudo_descriptor_length`, the 2 bytes before
/// an `elementary_stream_id_extension`
const PSEUDO_DESCRIPTOR: [u8; 2] = [0x01, 0x01];

/// Entry of the elementary stream loop of a
/// [`ProgramStreamMap`](super::ProgramStreamMap): an elementary stream and
/// the `stream_id` of the PES packets that carry it
///
/// An entry of `elementary_stream_id` 0xFD in a map whose
/// `single_extension_stream_flag` is `'0'` names its stream by
/// `elementary_stream_id_extension`, which the syntax codes as a pseudo
/// descriptor ahead of the descriptors, ISO/IEC 13818-1 §2.5.4.2.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementaryStream<'input> {
    /// Type of the elementary stream
    pub stream_type: StreamType,
    /// `stream_id` of the PES packets carrying the elementary stream
    pub elementary_stream_id: StreamId,
    /// `stream_id_extension` of the PES packets carrying the elementary
    /// stream, present only where `elementary_stream_id` is 0xFD
    pub elementary_stream_id_extension: Option<u8>,
    /// The descriptors of the elementary stream
    pub elementary_stream_info: DescriptorLoop<'input>,
}

impl ElementaryStream<'_> {
    /// Creates an entry without `elementary_stream_id_extension` or
    /// descriptors
    #[must_use]
    pub const fn new(stream_type: StreamType, elementary_stream_id: StreamId) -> Self {
        Self {
            stream_type,
            elementary_stream_id,
            elementary_stream_id_extension: None,
            elementary_stream_info: DescriptorLoop(&[]),
        }
    }
}

/// The elementary stream loop of a
/// [`ProgramStreamMap`](super::ProgramStreamMap), borrowed
///
/// The bytes are read entry by entry as [`iter`](Self::iter) walks them, so
/// a view of any bytes can be made; an entry the bytes end inside is an
/// error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementaryStreamLoop<'input>(pub(super) &'input [u8]);

impl<'input> ElementaryStreamLoop<'input> {
    /// Returns the bytes of the loop
    #[must_use]
    pub const fn as_bytes(self) -> &'input [u8] {
        self.0
    }

    /// Returns an iterator over the entries, each read as it is reached
    #[must_use]
    pub const fn iter(self) -> ElementaryStreamIter<'input> {
        ElementaryStreamIter(self.0)
    }

    /// Appends the bytes of a loop of `streams` to `out`, the `marker_bit`
    /// before an `elementary_stream_id_extension` `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): a `stream_type` of 0x05, or an
    ///   `elementary_stream_id` that is not the `stream_id` of an elementary
    ///   stream: below 0xBD, 0xBE (`padding_stream`) or 0xFF
    ///   (`program_stream_directory`).
    /// * [`ElementaryStreamIdExtension`](Error::ElementaryStreamIdExtension):
    ///   an `elementary_stream_id_extension` where `elementary_stream_id` is
    ///   not 0xFD.
    /// * [`Field`](Error::Field): an `elementary_stream_id_extension` that
    ///   does not fit 7 bits.
    /// * [`ElementaryStreamInfoLength`](Error::ElementaryStreamInfoLength):
    ///   descriptors of more than 65 535 bytes.
    /// * [`Descriptor`](Error::Descriptor): a descriptor loop ends inside a
    ///   descriptor, or a descriptor is refused by
    ///   [`RawDescriptor::write`](crate::descriptor::RawDescriptor::write).
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::pes::StreamId;
    /// use mpeg2_systems::ps::psm::{ElementaryStream, ElementaryStreamLoop};
    /// use mpeg2_systems::stream_type::StreamType;
    ///
    /// // MPEG-2 video on stream_id 0xE0
    /// let mut written = Vec::new();
    /// ElementaryStreamLoop::write_from(
    ///     [ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(0xe0))],
    ///     &mut written,
    /// )?;
    /// assert_eq!(written, [0x02, 0xe0, 0x00, 0x00]);
    /// # }
    /// # Ok::<(), mpeg2_systems::ps::psm::Error>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from<'stream>(
        streams: impl IntoIterator<Item = ElementaryStream<'stream>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        Self::write_entries(streams.into_iter().map(Ok), out)
    }

    /// Appends the bytes of `entries` to `out`, leaving `out` as it was if
    /// an entry is an error or is refused
    #[cfg(feature = "alloc")]
    pub(super) fn write_entries<'stream>(
        entries: impl IntoIterator<Item = Result<ElementaryStream<'stream>, Error>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        write_or_truncate(out, |out| {
            for stream in entries {
                let stream = stream?;
                if stream.stream_type == StreamType::ISO_IEC_13818_1_PRIVATE_SECTIONS {
                    return Err(Error::Forbidden {
                        field: "stream_type",
                        value: u32::from(stream.stream_type.get()),
                    });
                }
                let elementary_stream_id = stream.elementary_stream_id;
                if elementary_stream_id <= StreamId::PROGRAM_STREAM_MAP
                    || elementary_stream_id == StreamId::PADDING_STREAM
                    || elementary_stream_id == StreamId::PROGRAM_STREAM_DIRECTORY
                {
                    return Err(Error::Forbidden {
                        field: "elementary_stream_id",
                        value: u32::from(elementary_stream_id.get()),
                    });
                }
                if stream.elementary_stream_id_extension.is_some()
                    && elementary_stream_id != StreamId::EXTENDED_STREAM_ID
                {
                    return Err(Error::ElementaryStreamIdExtension {
                        elementary_stream_id,
                    });
                }
                let length = stream
                    .elementary_stream_id_extension
                    .map_or(0, |_| PSEUDO_DESCRIPTOR.len().saturating_add(1))
                    .saturating_add(stream.elementary_stream_info.0.len());
                let elementary_stream_info_length = u16::try_from(length)
                    .map_err(|_| Error::ElementaryStreamInfoLength { length })?;

                out.extend_from_slice(&[stream.stream_type.get(), elementary_stream_id.get()]);
                out.extend_from_slice(&elementary_stream_info_length.to_be_bytes());
                if let Some(extension) = stream.elementary_stream_id_extension {
                    let mut writer = FieldWriter::new(out);
                    (|| {
                        writer.write_bytes(&PSEUDO_DESCRIPTOR)?;
                        writer.write_bool(true)?;
                        writer.write_bits::<7>(extension)?;
                        writer.finish()
                    })()
                    .map_err(field_error("elementary_stream_id_extension"))?;
                }
                DescriptorLoop::write_entries(stream.elementary_stream_info, out)?;
            }

            Ok(())
        })
    }
}

impl<'input> From<&'input [u8]> for ElementaryStreamLoop<'input> {
    fn from(bytes: &'input [u8]) -> Self {
        Self(bytes)
    }
}

impl<'input> IntoIterator for ElementaryStreamLoop<'input> {
    type Item = Result<ElementaryStream<'input>, Error>;
    type IntoIter = ElementaryStreamIter<'input>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the entries of an [`ElementaryStreamLoop`]
///
/// An entry the bytes end inside, its descriptors included, is an
/// [`Error::Field`], and the last item. An entry of `elementary_stream_id`
/// 0xFD whose descriptors start with the pseudo descriptor 0x01 0x01 has its
/// third byte read as `elementary_stream_id_extension`, whatever the
/// `single_extension_stream_flag` of the map: no descriptor may have the tag
/// 0x01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementaryStreamIter<'input>(&'input [u8]);

impl<'input> Iterator for ElementaryStreamIter<'input> {
    type Item = Result<ElementaryStream<'input>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }

        let mut reader = FieldReader::new(self.0);
        let entry = (|| {
            let stream_type = reader
                .read_u8::<8>()
                .map(StreamType::from)
                .map_err(field_error("stream_type"))?;
            let elementary_stream_id = reader
                .read_u8::<8>()
                .map(StreamId::from)
                .map_err(field_error("elementary_stream_id"))?;
            let elementary_stream_info_length = reader
                .read_u16::<16>()
                .map_err(field_error("elementary_stream_info_length"))?;
            let info = reader
                .read_bytes(usize::from(elementary_stream_info_length))
                .map_err(field_error("elementary_stream_info"))?;
            let (elementary_stream_id_extension, descriptors) = match info.split_first_chunk::<3>()
            {
                Some((&[tag, length, extension], descriptors))
                    if elementary_stream_id == StreamId::EXTENDED_STREAM_ID
                        && [tag, length] == PSEUDO_DESCRIPTOR =>
                {
                    (Some(extension & 0x7f), descriptors)
                }
                _ => (None, info),
            };
            Ok(ElementaryStream {
                stream_type,
                elementary_stream_id,
                elementary_stream_id_extension,
                elementary_stream_info: DescriptorLoop(descriptors),
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
