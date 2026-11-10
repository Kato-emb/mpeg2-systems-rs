//! Pack header: the start of a pack, with the time its bytes are due and
//! the rate they arrive at

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use super::PACK_START_CODE;
use crate::field::{self, FieldReader};
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
#[cfg(feature = "alloc")]
use crate::time::write_marked_clock_reference;
use crate::time::{ClockReference, read_marked_clock_reference};

/// Bytes from `pack_start_code` to `pack_stuffing_length`
pub(crate) const FIXED_LENGTH: usize = 14;

/// The most `stuffing_byte`s a pack header may have, ISO/IEC 13818-1
/// §2.5.3.4
#[cfg(feature = "alloc")]
pub(super) const MAX_PACK_STUFFING_LENGTH: u8 = 7;

/// Failure to read or write a pack header
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The bytes do not start with `pack_start_code` 0x000001BA
    PackStartCode {
        /// The 32 bits found
        pack_start_code: u32,
    },
    /// The pack is an ISO/IEC 11172-1 pack, whose bits after
    /// `pack_start_code` are `'0010'`, which this crate does not read
    Iso11172Pack,
    /// The bits after `pack_start_code` are neither `'01'` nor the
    /// `'0010'` of an ISO/IEC 11172-1 pack
    PackHeaderStart {
        /// The two bits found
        bits: u8,
    },
    /// The bytes after `pack_stuffing_length` are not as many as it declares
    PackStuffingLength {
        /// The `pack_stuffing_length` read
        pack_stuffing_length: u8,
        /// The bytes given after `pack_stuffing_length`
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
    /// An SCR to write has an extension of 300 or more, which the
    /// specification does not allow
    ScrExtension {
        /// The extension given
        extension: u16,
    },
    /// A value to write is one the specification forbids for its field
    Forbidden {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The value given
        value: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PackStartCode { pack_start_code } => write!(
                formatter,
                "pack_start_code {pack_start_code:#010x} is not 0x000001ba"
            ),
            Self::Iso11172Pack => write!(
                formatter,
                "pack header starts with '0010', an ISO/IEC 11172-1 pack"
            ),
            Self::PackHeaderStart { bits } => {
                write!(formatter, "pack header starts with '{bits:02b}', not '01'")
            }
            Self::PackStuffingLength {
                pack_stuffing_length,
                available_bytes,
            } => write!(
                formatter,
                "pack_stuffing_length {pack_stuffing_length} does not match the \
                 {available_bytes} bytes given"
            ),
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read or written")
            }
            Self::ScrExtension { extension } => write!(
                formatter,
                "system_clock_reference_extension {extension} is not below 300"
            ),
            Self::Forbidden { field, value } => {
                write!(formatter, "{field} {value} is forbidden")
            }
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::PackStartCode { .. }
            | Self::Iso11172Pack
            | Self::PackHeaderStart { .. }
            | Self::PackStuffingLength { .. }
            | Self::ScrExtension { .. }
            | Self::Forbidden { .. } => None,
        }
    }
}

/// Returns a mapping of a failure on `field` to an [`Error::Field`]
const fn field_error(field: &'static str) -> impl Fn(field::Error) -> Error {
    move |error| Error::Field { field, error }
}

/// Header that opens a pack of a Program Stream, with the time its bytes
/// are due at the decoder
///
/// `pack_header`, ISO/IEC 13818-1 §2.5.3.3
///
/// A system header that follows the pack header is a [`SystemHeader`] of its
/// own, read from its own bytes; finding where either ends in a stream is a
/// framer's work, which this type does not do. The `stuffing_byte`s are
/// kept only as their count, and written as 0xFF; the `marker_bit`s and the
/// `reserved` bits are not checked when read and are written as `'1'`.
///
/// [`SystemHeader`]: super::SystemHeader
///
/// # Examples
///
/// ```
/// use mpeg2_systems::ps::PackHeader;
/// use mpeg2_systems::time::ClockReference;
///
/// // A pack due at SCR 0 at 55 059 900 bytes/s, with no stuffing
/// let bytes = [
///     0x00, 0x00, 0x01, 0xba, 0x44, 0x00, 0x04, 0x00, 0x04, 0x01, 0x43, 0x36, 0x3b, 0xf8,
/// ];
/// let pack_header = PackHeader::read(&bytes)?;
/// assert_eq!(
///     pack_header,
///     PackHeader::new(ClockReference::new(0, 0)?, 1_101_198)
/// );
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[doc(alias = "pack_header")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PackHeader {
    /// System clock reference (SCR): the time the byte holding the last bit
    /// of its base is due at the decoder
    pub system_clock_reference: ClockReference,
    /// `program_mux_rate`: the rate the decoder receives the pack at, in
    /// units of 50 bytes per second
    pub program_mux_rate: u32,
    /// `pack_stuffing_length`: the number of `stuffing_byte`s after it, at
    /// most 7
    pub pack_stuffing_length: u8,
}

impl PackHeader {
    /// Creates a pack header without stuffing
    #[must_use]
    pub const fn new(system_clock_reference: ClockReference, program_mux_rate: u32) -> Self {
        Self {
            system_clock_reference,
            program_mux_rate,
            pack_stuffing_length: 0,
        }
    }

    /// Returns the value with the given `pack_stuffing_length`
    #[must_use]
    pub const fn with_pack_stuffing_length(mut self, pack_stuffing_length: u8) -> Self {
        self.pack_stuffing_length = pack_stuffing_length;
        self
    }

    /// Reads a pack header from `bytes`, which hold it and nothing else
    ///
    /// # Errors
    ///
    /// * [`PackStartCode`](Error::PackStartCode): the bytes do not start
    ///   with 0x000001BA.
    /// * [`Iso11172Pack`](Error::Iso11172Pack): the pack is an ISO/IEC
    ///   11172-1 pack.
    /// * [`PackHeaderStart`](Error::PackHeaderStart): the bits after the
    ///   start code are not `'01'`.
    /// * [`Field`](Error::Field): the bytes end before
    ///   `pack_stuffing_length`.
    /// * [`PackStuffingLength`](Error::PackStuffingLength): the bytes after
    ///   `pack_stuffing_length` are not as many as it declares.
    pub fn read(bytes: &[u8]) -> Result<Self, Error> {
        let mut reader = FieldReader::new(bytes);
        let pack_start_code = reader
            .read_u32::<32>()
            .map_err(field_error("pack_start_code"))?;
        if pack_start_code.to_be_bytes() != PACK_START_CODE {
            return Err(Error::PackStartCode { pack_start_code });
        }
        let bits = reader.read_u8::<2>().map_err(field_error("'01'"))?;
        if bits != 0b01 {
            let is_iso_11172 = bytes
                .get(PACK_START_CODE.len())
                .is_some_and(|byte| byte.wrapping_shr(4) == 0b0010);
            return Err(if is_iso_11172 {
                Error::Iso11172Pack
            } else {
                Error::PackHeaderStart { bits }
            });
        }
        let system_clock_reference = read_marked_clock_reference(&mut reader)
            .map_err(field_error("system_clock_reference"))?;
        let mut program_mux_rate = || {
            let rate = reader.read_u32::<22>()?;
            reader.read_bits::<2>()?;
            Ok(rate)
        };
        let program_mux_rate = program_mux_rate().map_err(field_error("program_mux_rate"))?;
        let pack_stuffing_length = reader
            .read_bits::<5>()
            .and_then(|_| reader.read_u8::<3>())
            .map_err(field_error("pack_stuffing_length"))?;
        let available_bytes = reader.remaining_bytes();
        if available_bytes != usize::from(pack_stuffing_length) {
            return Err(Error::PackStuffingLength {
                pack_stuffing_length,
                available_bytes,
            });
        }

        Ok(Self {
            system_clock_reference,
            program_mux_rate,
            pack_stuffing_length,
        })
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        FIXED_LENGTH.saturating_add(usize::from(self.pack_stuffing_length))
    }

    /// Appends the pack header to `out`, its `marker_bit`s and `reserved`
    /// bits `'1'` and its `stuffing_byte`s 0xFF
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `program_mux_rate` is 0.
    /// * [`ScrExtension`](Error::ScrExtension): the extension of
    ///   `system_clock_reference` is 300 or more.
    /// * [`Field`](Error::Field): `program_mux_rate` does not fit 22 bits,
    ///   or `pack_stuffing_length` is more than 7.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::ps::PackHeader;
    /// use mpeg2_systems::time::ClockReference;
    ///
    /// let pack_header = PackHeader::new(ClockReference::new(0x1_2345_6789, 123)?, 1_101_198)
    ///     .with_pack_stuffing_length(2);
    /// let mut written = Vec::new();
    /// pack_header.write(&mut written)?;
    /// assert_eq!(
    ///     written,
    ///     [
    ///         0x00, 0x00, 0x01, 0xba, 0x66, 0x34, 0x57, 0x3c, 0x4c, 0xf7, 0x43, 0x36, 0x3b, 0xfa,
    ///         0xff, 0xff,
    ///     ]
    /// );
    /// # }
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.program_mux_rate == 0 {
            return Err(Error::Forbidden {
                field: "program_mux_rate",
                value: 0,
            });
        }
        let extension = self.system_clock_reference.extension();
        if extension >= ClockReference::EXTENSION_MODULUS {
            return Err(Error::ScrExtension { extension });
        }

        write_or_truncate(out, |out| {
            let mut writer = FieldWriter::new(out);
            writer
                .write_bytes(&PACK_START_CODE)
                .map_err(field_error("pack_start_code"))?;
            (|| {
                writer.write_bits::<2>(0b01_u8)?;
                write_marked_clock_reference(&mut writer, self.system_clock_reference)
            })()
            .map_err(field_error("system_clock_reference"))?;
            (|| {
                writer.write_bits::<22>(self.program_mux_rate)?;
                writer.write_bits::<2>(0b11_u8)
            })()
            .map_err(field_error("program_mux_rate"))?;
            (|| {
                writer.write_bits::<5>(0b1_1111_u8)?;
                writer.write_bits::<3>(self.pack_stuffing_length)
            })()
            .map_err(field_error("pack_stuffing_length"))?;
            writer.finish().map_err(field_error("pack_header"))?;
            out.resize(
                out.len()
                    .saturating_add(usize::from(self.pack_stuffing_length)),
                0xff,
            );

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{Error, PackHeader};
    use crate::field;
    use crate::time::ClockReference;

    const PROGRAM_STREAM: &[u8] = include_bytes!("../../tests/fixtures/program.ps");

    /// SCR `0x1_2345_6789` and extension 123, 1 101 198 × 50 bytes/s, 2
    /// `stuffing_byte`s
    const WITH_STUFFING: [u8; 16] = [
        0x00, 0x00, 0x01, 0xba, 0x66, 0x34, 0x57, 0x3c, 0x4c, 0xf7, 0x43, 0x36, 0x3b, 0xfa, 0xff,
        0xff,
    ];

    fn with_stuffing() -> PackHeader {
        let mut pack_header =
            PackHeader::new(ClockReference::new(0x1_2345_6789, 123).unwrap(), 1_101_198);
        pack_header.pack_stuffing_length = 2;
        pack_header
    }

    /// Returns the pack headers of the fixture, which has a pack every 2048
    /// bytes and no stuffing
    fn pack_headers() -> impl Iterator<Item = &'static [u8]> {
        PROGRAM_STREAM
            .chunks(2048)
            .map(|pack| pack.get(..super::FIXED_LENGTH).unwrap())
    }

    #[test]
    fn every_pack_header_of_an_ffmpeg_program_stream_is_read_with_its_scr() {
        let mut read = pack_headers().map(PackHeader::read);

        assert_eq!(
            core::array::from_fn(|_| read.next()),
            [0, 3, 6, 45_001, 83_179].map(|base| Some(Ok(PackHeader::new(
                ClockReference::new(base, 0).unwrap(),
                1_101_198
            ))))
        );
        assert_eq!(read.next(), None);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn every_pack_header_of_an_ffmpeg_program_stream_is_written_back_as_read() {
        for bytes in pack_headers() {
            let mut written = Vec::new();
            PackHeader::read(bytes)
                .unwrap()
                .write(&mut written)
                .unwrap();

            assert_eq!(written, bytes);
        }
    }

    #[test]
    fn stuffing_is_kept_as_its_count() {
        assert_eq!(PackHeader::read(&WITH_STUFFING), Ok(with_stuffing()));
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn stuffing_is_written_as_0xff() {
        let mut written = Vec::new();
        with_stuffing().write(&mut written).unwrap();

        assert_eq!(
            (written.as_slice(), with_stuffing().encoded_len()),
            (WITH_STUFFING.as_slice(), WITH_STUFFING.len())
        );
    }

    #[test]
    fn marker_and_reserved_bits_read_as_0_are_not_checked() {
        let cleared = [
            0x00, 0x00, 0x01, 0xba, 0x62, 0x34, 0x53, 0x3c, 0x48, 0xf6, 0x43, 0x36, 0x38, 0x02,
            0x00, 0x00,
        ];

        assert_eq!(PackHeader::read(&cleared), Ok(with_stuffing()));
    }

    #[test]
    fn an_iso_11172_1_pack_is_an_error_of_its_own() {
        let iso_11172 = [
            0x00, 0x00, 0x01, 0xba, 0x21, 0x00, 0x01, 0x00, 0x01, 0x80, 0x00, 0x01,
        ];
        let mut other_start = WITH_STUFFING;
        other_start[4] |= 0xc0;

        assert_eq!(
            [&iso_11172[..], &other_start].map(PackHeader::read),
            [
                Err(Error::Iso11172Pack),
                Err(Error::PackHeaderStart { bits: 0b11 })
            ]
        );
    }

    #[test]
    fn bytes_that_are_not_one_whole_pack_header_are_not_read() {
        let mut system_header = WITH_STUFFING;
        system_header[3] = 0xbb;

        assert_eq!(
            [
                &system_header[..],
                &WITH_STUFFING[..10],
                &WITH_STUFFING[..15],
                &[WITH_STUFFING.as_slice(), &[0xff]].concat(),
            ]
            .map(PackHeader::read),
            [
                Err(Error::PackStartCode {
                    pack_start_code: 0x0000_01bb
                }),
                Err(Error::Field {
                    field: "program_mux_rate",
                    error: field::Error::Truncated {
                        required_bits: 22,
                        remaining_bits: 0
                    }
                }),
                Err(Error::PackStuffingLength {
                    pack_stuffing_length: 2,
                    available_bytes: 1
                }),
                Err(Error::PackStuffingLength {
                    pack_stuffing_length: 2,
                    available_bytes: 3
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_pack_header_is_written_only_with_values_the_specification_allows() {
        let scr = ClockReference::new(0, 0).unwrap();
        let mut stuffed = PackHeader::new(scr, 1);
        stuffed.pack_stuffing_length = 8;
        let mut written = Vec::new();

        assert_eq!(
            [
                PackHeader::new(scr, 0),
                PackHeader::new(ClockReference::new(0, 300).unwrap(), 1),
                PackHeader::new(scr, 1 << 22),
                stuffed,
            ]
            .map(|pack_header| pack_header.write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "program_mux_rate",
                    value: 0
                }),
                Err(Error::ScrExtension { extension: 300 }),
                Err(Error::Field {
                    field: "program_mux_rate",
                    error: field::Error::ValueTooWide {
                        width: 22,
                        value: 1 << 22
                    }
                }),
                Err(Error::Field {
                    field: "pack_stuffing_length",
                    error: field::Error::ValueTooWide { width: 3, value: 8 }
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
