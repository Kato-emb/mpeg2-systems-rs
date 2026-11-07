//! Elementary stream loop of the program map table: each program element
//! and the PID that carries it

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::Error;
#[cfg(feature = "alloc")]
use super::check_info_length;
use crate::field::FieldReader;
#[cfg(feature = "alloc")]
use crate::field::FieldWriter;
use crate::stream_type::StreamType;
use crate::ts::Pid;

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
    /// The descriptors of the program element, as bytes
    pub es_info: &'input [u8],
}

impl<'input> ElementaryStream<'input> {
    /// Creates an entry without descriptors
    #[must_use]
    pub const fn new(stream_type: StreamType, elementary_pid: Pid) -> Self {
        Self {
            stream_type,
            elementary_pid,
            es_info: &[],
        }
    }
}

/// The elementary stream loop of a
/// [`TsProgramMapSection`](super::TsProgramMapSection), borrowed
///
/// The bytes are read entry by entry as [`iter`](Self::iter) walks them, so
/// a view of any bytes can be made; an entry the bytes end inside is an
/// error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
    /// [`InfoLength`](Error::InfoLength): descriptors of more than 1023
    /// bytes.
    #[cfg(feature = "alloc")]
    pub fn write_from<'stream>(
        streams: impl IntoIterator<Item = ElementaryStream<'stream>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        let start = out.len();
        for stream in streams {
            let entry = check_info_length("ES_info_length", stream.es_info).and_then(|()| {
                let mut writer = FieldWriter::new(out);
                // Why not an error per field: every value fits its field's
                // width.
                (|| {
                    writer.write_bits::<8>(stream.stream_type.get())?;
                    writer.write_bits::<3>(0b111_u8)?;
                    writer.write_bits::<13>(stream.elementary_pid.get())?;
                    writer.write_bits::<4>(0b1111_u8)?;
                    writer.write_bits::<12>(
                        u64::try_from(stream.es_info.len()).unwrap_or(u64::MAX),
                    )?;
                    writer.write_bytes(stream.es_info)?;
                    writer.finish()
                })()
                .map_err(|error| Error::Field {
                    field: "elementary stream loop",
                    error,
                })
            });
            if let Err(error) = entry {
                out.truncate(start);
                return Err(error);
            }
        }

        Ok(())
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
/// [`Error::Field`], and the last item.
#[derive(Debug, Clone)]
pub struct ElementaryStreamIter<'input>(&'input [u8]);

impl<'input> Iterator for ElementaryStreamIter<'input> {
    type Item = Result<ElementaryStream<'input>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }

        let mut reader = FieldReader::new(self.0);
        let mut entry = || {
            let stream_type = StreamType::from(reader.read_u8::<8>()?);
            reader.read_bits::<3>()?;
            let elementary_pid = Pid(reader.read_u16::<13>()?);
            reader.read_bits::<4>()?;
            let es_info_length = reader.read_u16::<12>()?;
            let es_info = reader.read_bytes(usize::from(es_info_length))?;
            Ok(ElementaryStream {
                stream_type,
                elementary_pid,
                es_info,
            })
        };
        let entry = entry().map_err(|error| Error::Field {
            field: "ES_info_length",
            error,
        });
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
