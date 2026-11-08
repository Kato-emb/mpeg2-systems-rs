//! Adaptation field of a Transport Packet: clock references, splice points,
//! private data and stuffing

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use crate::field::{self, FieldReader};
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::time::ClockReference;

/// Failure to read or write an adaptation field
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The bytes given are not as many as `adaptation_field_length` declares
    Length {
        /// The `adaptation_field_length` read
        adaptation_field_length: u8,
        /// The bytes given, the `adaptation_field_length` byte included
        available_bytes: usize,
    },
    /// A field could not be read or written
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field failed
        error: field::Error,
    },
    /// A clock reference to write has an extension of 300 or more, which the
    /// specification does not allow
    ClockReferenceExtension {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The extension given
        extension: u16,
    },
    /// A run of bytes to write is longer than its length field can state
    TooLong {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The length given, in bytes
        length: usize,
        /// The longest the field can be, in bytes
        limit: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length {
                adaptation_field_length,
                available_bytes,
            } => write!(
                formatter,
                "adaptation_field_length {adaptation_field_length} does not match \
                 the {available_bytes} bytes given"
            ),
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read or written")
            }
            Self::ClockReferenceExtension { field, extension } => {
                write!(formatter, "{field} {extension} is not below 300")
            }
            Self::TooLong {
                field,
                length,
                limit,
            } => write!(
                formatter,
                "{field} of {length} bytes is longer than {limit} bytes"
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::Length { .. } | Self::ClockReferenceExtension { .. } | Self::TooLong { .. } => {
                None
            }
        }
    }
}

/// Longest `adaptation_field_length` a Transport Packet can hold
pub(crate) const MAX_ADAPTATION_FIELD_LENGTH: usize = 183;

/// Bytes a clock reference occupies: 33 bits of base, 6 reserved, 9 of extension
const CLOCK_REFERENCE_LENGTH: usize = 6;

/// Adaptation field, the optional part of a Transport Packet that carries timing and stuffing
///
/// `adaptation_field`, ISO/IEC 13818-1 §2.4.3.4
///
/// Each optional field is present when its value is `Some`; the flags of the
/// syntax table follow from that, and `adaptation_field_length` from the
/// fields and [`stuffing_bytes`](Self::stuffing_bytes). The adaptation field
/// is never scrambled, so it is read whatever the packet's
/// `transport_scrambling_control` says.
///
/// `adaptation_field_extension` is kept as the bytes that follow its length
/// field, uninterpreted, so it is written back as it was read.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::time::ClockReference;
/// use mpeg2_systems::ts::adaptation_field::{AdaptationField, Error};
///
/// // An adaptation field carrying a PCR
/// let bytes = [0x07, 0x10, 0x00, 0x00, 0x00, 0x01, 0x7e, 0x64];
/// let adaptation_field = AdaptationField::read(&bytes)?;
/// assert_eq!(
///     adaptation_field.program_clock_reference,
///     Some(ClockReference::new(2, 100).unwrap())
/// );
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// adaptation_field.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Error>(())
/// ```
#[doc(alias = "adaptation_field")]
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AdaptationField<'input> {
    /// Whether the packet starts a discontinuity of the system time-base or
    /// of the `continuity_counter`
    pub discontinuity_indicator: bool,
    /// Whether the packet holds the start of a point a decoder can begin at
    pub random_access_indicator: bool,
    /// Whether the elementary stream data of the packet has a higher priority
    pub elementary_stream_priority_indicator: bool,
    /// `program_clock_reference_base` and `_extension` (PCR)
    pub program_clock_reference: Option<ClockReference>,
    /// `original_program_clock_reference_base` and `_extension` (OPCR)
    pub original_program_clock_reference: Option<ClockReference>,
    /// Packets of the PID left before a splicing point, negative past it
    pub splice_countdown: Option<i8>,
    /// `private_data_byte`s, without the length field before them
    pub transport_private_data: Option<&'input [u8]>,
    /// The bytes after `adaptation_field_extension_length`, uninterpreted
    pub adaptation_field_extension: Option<&'input [u8]>,
    /// Bytes the adaptation field occupies past its fields
    ///
    /// They are written as `0xFF`. With no flag set and no field present, the
    /// byte of flags is the first of them, so an adaptation field of one byte
    /// of stuffing is the `adaptation_field_length` of 0 alone.
    pub stuffing_bytes: usize,
}

impl<'input> AdaptationField<'input> {
    /// Creates an adaptation field with no flag set and no field present
    #[expect(
        clippy::new_without_default,
        reason = "the specification defines no default adaptation field"
    )]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            discontinuity_indicator: false,
            random_access_indicator: false,
            elementary_stream_priority_indicator: false,
            program_clock_reference: None,
            original_program_clock_reference: None,
            splice_countdown: None,
            transport_private_data: None,
            adaptation_field_extension: None,
            stuffing_bytes: 0,
        }
    }

    /// Returns whether any flag of the byte of flags is set
    fn has_flags(&self) -> bool {
        self.discontinuity_indicator
            || self.random_access_indicator
            || self.elementary_stream_priority_indicator
            || self.program_clock_reference.is_some()
            || self.original_program_clock_reference.is_some()
            || self.splice_countdown.is_some()
            || self.transport_private_data.is_some()
            || self.adaptation_field_extension.is_some()
    }

    /// Returns the `adaptation_field_length` the fields and the stuffing add up to
    #[must_use]
    pub fn adaptation_field_length(&self) -> usize {
        if !self.has_flags() {
            return self.stuffing_bytes;
        }

        let clock_references = [
            self.program_clock_reference,
            self.original_program_clock_reference,
        ]
        .iter()
        .flatten()
        .count()
        .saturating_mul(CLOCK_REFERENCE_LENGTH);
        let runs = [self.transport_private_data, self.adaptation_field_extension]
            .iter()
            .flatten()
            .fold(0_usize, |length, run| {
                length.saturating_add(1).saturating_add(run.len())
            });

        1_usize
            .saturating_add(clock_references)
            .saturating_add(usize::from(self.splice_countdown.is_some()))
            .saturating_add(runs)
            .saturating_add(self.stuffing_bytes)
    }

    /// Reads an adaptation field from `bytes`, which hold it whole and nothing more
    ///
    /// # Errors
    ///
    /// * [`Length`](Error::Length): `bytes` are not one byte more than the
    ///   `adaptation_field_length` they start with.
    /// * [`Field`](Error::Field): a field a flag announces runs past the end of
    ///   the adaptation field.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        let Some((&adaptation_field_length, body)) = bytes.split_first() else {
            return Err(Error::Length {
                adaptation_field_length: 0,
                available_bytes: 0,
            });
        };
        if body.len() != usize::from(adaptation_field_length) {
            return Err(Error::Length {
                adaptation_field_length,
                available_bytes: bytes.len(),
            });
        }

        let mut adaptation_field = Self::new();
        match body.first() {
            None => return Ok(adaptation_field),
            Some(0) => {
                adaptation_field.stuffing_bytes = body.len();
                return Ok(adaptation_field);
            }
            Some(_) => {}
        }

        let mut reader = FieldReader::new(body);
        let mut flag = || {
            reader
                .read_bool()
                .map_err(field_error("adaptation_field flags"))
        };
        adaptation_field.discontinuity_indicator = flag()?;
        adaptation_field.random_access_indicator = flag()?;
        adaptation_field.elementary_stream_priority_indicator = flag()?;
        let pcr_flag = flag()?;
        let opcr_flag = flag()?;
        let splicing_point_flag = flag()?;
        let transport_private_data_flag = flag()?;
        let adaptation_field_extension_flag = flag()?;

        if pcr_flag {
            adaptation_field.program_clock_reference = Some(read_clock_reference(
                &mut reader,
                "program_clock_reference",
            )?);
        }
        if opcr_flag {
            adaptation_field.original_program_clock_reference = Some(read_clock_reference(
                &mut reader,
                "original_program_clock_reference",
            )?);
        }
        if splicing_point_flag {
            adaptation_field.splice_countdown = Some(
                reader
                    .read_u8::<8>()
                    .map_err(field_error("splice_countdown"))?
                    .cast_signed(),
            );
        }
        if transport_private_data_flag {
            adaptation_field.transport_private_data = Some(read_run(
                &mut reader,
                "transport_private_data_length",
                "private_data_byte",
            )?);
        }
        if adaptation_field_extension_flag {
            adaptation_field.adaptation_field_extension = Some(read_run(
                &mut reader,
                "adaptation_field_extension_length",
                "adaptation_field_extension",
            )?);
        }
        adaptation_field.stuffing_bytes = reader.remaining_bytes();

        Ok(adaptation_field)
    }

    /// Returns the number of bytes [`write`](Self::write) appends, the
    /// `adaptation_field_length` byte included
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.adaptation_field_length().saturating_add(1)
    }

    /// Appends the adaptation field to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`ClockReferenceExtension`](Error::ClockReferenceExtension): a clock
    ///   reference has an extension of 300 or more.
    /// * [`TooLong`](Error::TooLong): `transport_private_data` or
    ///   `adaptation_field_extension` is longer than 255 bytes, or the
    ///   adaptation field longer than a Transport Packet can hold.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write_or_truncate(out, |out| self.write_fields(out))
    }

    /// Appends the fields, leaving whatever it appended on failure
    #[cfg(feature = "alloc")]
    fn write_fields(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let adaptation_field_length = self.adaptation_field_length();
        let too_long = Error::TooLong {
            field: "adaptation_field",
            length: adaptation_field_length,
            limit: MAX_ADAPTATION_FIELD_LENGTH,
        };
        let length_byte = u8::try_from(adaptation_field_length)
            .ok()
            .filter(|length| usize::from(*length) <= MAX_ADAPTATION_FIELD_LENGTH)
            .ok_or(too_long)?;

        let mut writer = FieldWriter::new(out);
        write_field(&mut writer, "adaptation_field_length", |writer| {
            writer.write_bits::<8>(length_byte)
        })?;
        if !self.has_flags() {
            writer.finish().map_err(field_error("adaptation_field"))?;
            if let Some(stuffing) = self.stuffing_bytes.checked_sub(1) {
                out.push(0);
                out.resize(out.len().saturating_add(stuffing), 0xff);
            }
            return Ok(());
        }

        write_field(&mut writer, "adaptation_field flags", |writer| {
            for flag in [
                self.discontinuity_indicator,
                self.random_access_indicator,
                self.elementary_stream_priority_indicator,
                self.program_clock_reference.is_some(),
                self.original_program_clock_reference.is_some(),
                self.splice_countdown.is_some(),
                self.transport_private_data.is_some(),
                self.adaptation_field_extension.is_some(),
            ] {
                writer.write_bool(flag)?;
            }
            Ok(())
        })?;
        if let Some(reference) = self.program_clock_reference {
            write_clock_reference(&mut writer, reference, "program_clock_reference")?;
        }
        if let Some(reference) = self.original_program_clock_reference {
            write_clock_reference(&mut writer, reference, "original_program_clock_reference")?;
        }
        if let Some(countdown) = self.splice_countdown {
            write_field(&mut writer, "splice_countdown", |writer| {
                writer.write_bits::<8>(countdown.cast_unsigned())
            })?;
        }
        if let Some(run) = self.transport_private_data {
            write_run(&mut writer, run, "transport_private_data_length")?;
        }
        if let Some(run) = self.adaptation_field_extension {
            write_run(&mut writer, run, "adaptation_field_extension_length")?;
        }
        writer.finish().map_err(field_error("adaptation_field"))?;
        out.resize(out.len().saturating_add(self.stuffing_bytes), 0xff);

        Ok(())
    }
}

/// Names the field a [`field::Error`] arose in
const fn field_error(field: &'static str) -> impl Fn(field::Error) -> Error {
    move |error| Error::Field { field, error }
}

/// Reads a clock reference laid out as 33 bits of base, 6 reserved bits and 9 of extension
fn read_clock_reference(
    reader: &mut FieldReader<'_>,
    field: &'static str,
) -> Result<ClockReference, Error> {
    let base = reader.read_bits::<33>().map_err(field_error(field))?;
    reader.read_bits::<6>().map_err(field_error(field))?;
    let extension = reader.read_u16::<9>().map_err(field_error(field))?;

    Ok(ClockReference::from_fields(base, extension))
}

/// Reads a length byte and the run of bytes it counts
fn read_run<'input>(
    reader: &mut FieldReader<'input>,
    length_field: &'static str,
    run_field: &'static str,
) -> Result<&'input [u8], Error> {
    let length = reader.read_u8::<8>().map_err(field_error(length_field))?;

    reader
        .read_bytes(usize::from(length))
        .map_err(field_error(run_field))
}

/// Writes one field, naming it in the error
#[cfg(feature = "alloc")]
fn write_field(
    writer: &mut FieldWriter<'_>,
    field: &'static str,
    write: impl FnOnce(&mut FieldWriter<'_>) -> Result<(), field::Error>,
) -> Result<(), Error> {
    write(writer).map_err(field_error(field))
}

/// Writes a clock reference as 33 bits of base, 6 reserved bits set and 9 of extension
#[cfg(feature = "alloc")]
fn write_clock_reference(
    writer: &mut FieldWriter<'_>,
    reference: ClockReference,
    field: &'static str,
) -> Result<(), Error> {
    if reference.extension() >= ClockReference::EXTENSION_MODULUS {
        return Err(Error::ClockReferenceExtension {
            field,
            extension: reference.extension(),
        });
    }

    write_field(writer, field, |writer| {
        writer.write_bits::<33>(reference.base())?;
        writer.write_bits::<6>(0b11_1111_u8)?;
        writer.write_bits::<9>(reference.extension())
    })
}

/// Writes a length byte and the run of bytes it counts
#[cfg(feature = "alloc")]
fn write_run(
    writer: &mut FieldWriter<'_>,
    run: &[u8],
    length_field: &'static str,
) -> Result<(), Error> {
    let length = u8::try_from(run.len()).map_err(|_| Error::TooLong {
        field: length_field,
        length: run.len(),
        limit: usize::from(u8::MAX),
    })?;

    write_field(writer, length_field, |writer| {
        writer.write_bits::<8>(length)?;
        writer.write_bytes(run)
    })
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{AdaptationField, Error};
    use crate::field;
    #[cfg(feature = "alloc")]
    use crate::time::ClockReference;

    #[test]
    fn a_field_its_flag_announces_but_that_runs_past_the_end_names_itself() {
        assert_eq!(
            AdaptationField::read(&[0x04, 0x10, 0x00, 0x00, 0x00]),
            Err(Error::Field {
                field: "program_clock_reference",
                error: field::Error::Truncated {
                    required_bits: 33,
                    remaining_bits: 24
                }
            })
        );
    }

    #[test]
    fn bytes_that_disagree_with_the_adaptation_field_length_are_refused() {
        assert_eq!(
            AdaptationField::read(&[0x03, 0x00, 0xff]),
            Err(Error::Length {
                adaptation_field_length: 3,
                available_bytes: 3
            })
        );
    }

    #[test]
    fn an_adaptation_field_with_no_flag_set_counts_its_flags_byte_as_stuffing() {
        let stuffed = |stuffing_bytes| {
            let mut adaptation_field = AdaptationField::new();
            adaptation_field.stuffing_bytes = stuffing_bytes;
            adaptation_field
        };

        assert_eq!(
            [
                AdaptationField::read(&[0x00]),
                AdaptationField::read(&[0x01, 0x00]),
                AdaptationField::read(&[0x03, 0x00, 0xff, 0xff]),
            ],
            [Ok(stuffed(0)), Ok(stuffed(1)), Ok(stuffed(3))]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn every_field_and_the_stuffing_after_them_are_written_back_as_read() {
        let bytes = [
            0x15, 0xff, 0x00, 0x00, 0x00, 0x01, 0x7e, 0x64, 0xff, 0xff, 0xff, 0xff, 0xff, 0x2b,
            0xfd, 0x02, 0xab, 0xcd, 0x01, 0x80, 0xff, 0xff,
        ];

        let adaptation_field = AdaptationField::read(&bytes).unwrap();
        let mut written = Vec::new();
        adaptation_field.write(&mut written).unwrap();

        assert_eq!(
            adaptation_field,
            AdaptationField {
                discontinuity_indicator: true,
                random_access_indicator: true,
                elementary_stream_priority_indicator: true,
                program_clock_reference: Some(ClockReference::new(2, 100).unwrap()),
                original_program_clock_reference: Some(
                    ClockReference::new((1 << 33) - 1, 299).unwrap()
                ),
                splice_countdown: Some(-3),
                transport_private_data: Some(&[0xab, 0xcd]),
                adaptation_field_extension: Some(&[0x80]),
                stuffing_bytes: 2,
            }
        );
        assert_eq!(written, bytes);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_clock_reference_extension_of_300_or_more_is_refused_before_a_byte_is_written() {
        let mut adaptation_field = AdaptationField::new();
        adaptation_field.program_clock_reference = Some(ClockReference::new(0, 300).unwrap());
        let mut written = alloc::vec![0x47];

        assert_eq!(
            adaptation_field.write(&mut written),
            Err(Error::ClockReferenceExtension {
                field: "program_clock_reference",
                extension: 300
            })
        );
        assert_eq!(written, [0x47]);
    }
}
