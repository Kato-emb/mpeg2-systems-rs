//! Elementary stream loop of the program map table: each program element
//! and the PID that carries it

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::Error;
use crate::descriptor::DescriptorLoop;
use crate::field::FieldReader;
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::stream_type::StreamType;
use crate::ts::Pid;

/// Largest `ES_info_length`, whose first two bits are `'00'`
#[cfg(feature = "alloc")]
const MAX_ES_INFO_LENGTH: usize = 0x3ff;

/// Bytes of an entry from `stream_type` to `ES_info_length`
#[cfg(feature = "alloc")]
const ENTRY_PREFIX_LENGTH: usize = 5;

/// Entry of the elementary stream loop of a
/// [`TsProgramMapSection`](super::TsProgramMapSection): a program element and
/// the PID that carries it
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementaryStream<'input> {
    /// Type of the program element
    pub stream_type: StreamType,
    /// PID of the Transport Packets carrying the program element
    pub elementary_pid: Pid,
    /// The descriptors of the program element
    pub es_info: DescriptorLoop<'input>,
}

impl<'input> ElementaryStream<'input> {
    /// Creates an entry without descriptors
    #[must_use]
    pub const fn new(stream_type: StreamType, elementary_pid: Pid) -> Self {
        Self {
            stream_type,
            elementary_pid,
            es_info: DescriptorLoop(&[]),
        }
    }

    /// Returns the value with the given `stream_type`
    #[must_use]
    pub const fn with_stream_type(mut self, stream_type: StreamType) -> Self {
        self.stream_type = stream_type;
        self
    }

    /// Returns the value with the given `elementary_pid`
    #[must_use]
    pub const fn with_elementary_pid(mut self, elementary_pid: Pid) -> Self {
        self.elementary_pid = elementary_pid;
        self
    }

    /// Returns the value with the given `es_info`
    #[must_use]
    pub const fn with_es_info(mut self, es_info: DescriptorLoop<'input>) -> Self {
        self.es_info = es_info;
        self
    }

    /// Returns the bytes the entry takes in the elementary stream loop
    #[cfg(feature = "alloc")]
    pub(super) const fn encoded_len(&self) -> usize {
        ENTRY_PREFIX_LENGTH.saturating_add(self.es_info.0.len())
    }
}

/// The elementary stream loop of a
/// [`TsProgramMapSection`](super::TsProgramMapSection), borrowed
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

    /// Appends the bytes of a loop of `streams` to `out`, their `reserved`
    /// bits all `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`ElementaryPid`](Error::ElementaryPid): an `elementary_PID` outside
    ///   `0x0010` to `0x1FFE`.
    /// * [`EsInfoLength`](Error::EsInfoLength): descriptors of more than
    ///   1023 bytes.
    /// * [`Descriptor`](Error::Descriptor): a descriptor loop ends inside a
    ///   descriptor, or a descriptor is refused by
    ///   [`RawDescriptor::write`](crate::descriptor::RawDescriptor::write).
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::psi::VersionNumber;
    /// use mpeg2_systems::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
    /// use mpeg2_systems::stream_type::StreamType;
    /// use mpeg2_systems::ts::Pid;
    ///
    /// // A program of one MPEG-2 video stream on PID 0x0100, which carries the
    /// // PCR
    /// let video = Pid::try_from(0x0100)?;
    /// let mut streams = Vec::new();
    /// ElementaryStreamLoop::write_from(
    ///     [ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video)],
    ///     &mut streams,
    /// )?;
    /// let pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0)?)
    ///     .with_pcr_pid(video)
    ///     .with_elementary_streams(ElementaryStreamLoop::from(streams.as_slice()));
    ///
    /// let mut written = Vec::new();
    /// pmt.write(&mut written)?;
    /// assert_eq!(
    ///     written,
    ///     [
    ///         0x02, 0xb0, 0x12, 0x00, 0x01, 0xc1, 0x00, 0x00, 0xe1, 0x00, 0xf0, 0x00, 0x02, 0xe1,
    ///         0x00, 0xf0, 0x00, 0x9e, 0x8b, 0x23, 0xd1,
    ///     ]
    /// );
    /// # }
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from<'stream>(
        streams: impl IntoIterator<Item = ElementaryStream<'stream>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        write_entries(streams.into_iter().map(Ok), out)
    }
}

/// Appends the bytes of `entries` to `out`, their `reserved` bits all `'1'`,
/// leaving `out` as it was if an entry is an error or is refused
#[cfg(feature = "alloc")]
pub(super) fn write_entries<'stream>(
    entries: impl IntoIterator<Item = Result<ElementaryStream<'stream>, Error>>,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    write_or_truncate(out, |out| {
        for stream in entries {
            let stream = stream?;
            let pid = stream.elementary_pid;
            if !pid.is_assignable() {
                return Err(Error::ElementaryPid { pid });
            }
            check_es_info_length(stream.es_info)?;
            let es_info_length = stream.es_info.0.len();
            let mut writer = FieldWriter::new(out);
            // Why not an error per field: every value fits its field's width.
            (|| {
                writer.write_bits::<8>(stream.stream_type.get())?;
                writer.write_bits::<3>(0b111_u8)?;
                writer.write_bits::<13>(pid.get())?;
                writer.write_bits::<4>(0b1111_u8)?;
                writer.write_bits::<12>(u64::try_from(es_info_length).unwrap_or(u64::MAX))?;
                writer.finish()
            })()
            .map_err(|error| Error::Field {
                field: "elementary stream loop",
                error,
            })?;
            DescriptorLoop::write_entries(stream.es_info, out)?;
        }

        Ok(())
    })
}

/// Refuses `es_info` longer than `ES_info_length` counts
#[cfg(feature = "alloc")]
pub(super) const fn check_es_info_length(es_info: DescriptorLoop<'_>) -> Result<(), Error> {
    let length = es_info.0.len();
    if length > MAX_ES_INFO_LENGTH {
        return Err(Error::EsInfoLength { length });
    }
    Ok(())
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
/// [`Error::Field`], and the last item.
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
                .map_err(|error| Error::Field {
                    field: "stream_type",
                    error,
                })?;
            let elementary_pid = reader
                .read_bits::<3>()
                .and_then(|_| reader.read_u16::<13>())
                .map(Pid)
                .map_err(|error| Error::Field {
                    field: "elementary_PID",
                    error,
                })?;
            let es_info_length = reader
                .read_bits::<4>()
                .and_then(|_| reader.read_u16::<12>())
                .map_err(|error| Error::Field {
                    field: "ES_info_length",
                    error,
                })?;
            let es_info = reader
                .read_bytes(usize::from(es_info_length))
                .map(DescriptorLoop)
                .map_err(|error| Error::Field {
                    field: "ES_info",
                    error,
                })?;
            Ok(ElementaryStream {
                stream_type,
                elementary_pid,
                es_info,
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
