//! Optional PES header: the flags, time stamps and other optional fields
//! between `PES_packet_length` and the payload

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::dsm_trick_mode::DsmTrickMode;
use super::extension::PesExtension;
use super::scrambling_control::PesScramblingControl;
use super::{Error, field_error};
use crate::field::FieldReader;
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
use crate::time::{ClockReference, Timestamp, read_marked_33_bits, read_marked_clock_reference};
#[cfg(feature = "alloc")]
use crate::time::{write_marked_33_bits, write_marked_clock_reference};

/// Bytes from the `'10'` bits to `PES_header_data_length`
const FIXED_LENGTH: usize = 3;

/// Most `stuffing_byte`s a PES packet header may hold
#[cfg(feature = "alloc")]
const MAX_STUFFING_BYTES: usize = 32;

/// The 4 bits before a PTS that no DTS follows
#[cfg(feature = "alloc")]
const PTS_ONLY_PREFIX: u8 = 0b0010;
/// The 4 bits before a PTS that a DTS follows
#[cfg(feature = "alloc")]
const PTS_PREFIX: u8 = 0b0011;
/// The 4 bits before a DTS
#[cfg(feature = "alloc")]
const DTS_PREFIX: u8 = 0b0001;

/// Optional PES header, the fields between `PES_packet_length` and the payload
///
/// ISO/IEC 13818-1 §2.4.3.6
///
/// The specification gives this part of the PES packet no name of its own;
/// it is what [`StreamId::carries_optional_header`](super::StreamId::carries_optional_header)
/// says a packet has. Each optional field is present when its value is
/// `Some`; the flags and `PES_header_data_length` follow from the fields and
/// [`stuffing_bytes`](Self::stuffing_bytes).
///
/// A PTS, a DTS or an ESCR whose `marker_bit`s are not `'1'` is read for its
/// value, as FFmpeg reads it; the bits are written as `'1'`.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::pes::{Error, OptionalHeader};
/// use mpeg2_systems::time::Timestamp;
///
/// // A header that carries a PTS of one second
/// let header = OptionalHeader::new()
///     .with_pts(Timestamp::try_from(90_000).unwrap())
///     .with_data_alignment_indicator(true);
///
/// // Written (with the `alloc` feature) and read back, it holds the same values
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// header.write(&mut written)?;
/// assert_eq!(written, [0x84, 0x80, 0x05, 0x21, 0x00, 0x05, 0xbf, 0x21]);
/// assert_eq!(OptionalHeader::read(&written)?, header);
/// # }
/// # Ok::<(), Error>(())
/// ```
#[doc(alias("PES header", "PTS", "DTS"))]
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OptionalHeader<'input> {
    /// How the payload is scrambled
    pub pes_scrambling_control: PesScramblingControl,
    /// Whether the payload has priority over that of packets without the bit
    pub pes_priority: bool,
    /// Whether the payload starts with the syntax element or sync word the
    /// `data_stream_alignment_descriptor` names
    pub data_alignment_indicator: bool,
    /// Whether the payload is protected by copyright
    pub copyright: bool,
    /// Whether the payload is an original rather than a copy
    pub original_or_copy: bool,
    /// Presentation time stamp (PTS)
    pub pts: Option<Timestamp>,
    /// Decoding time stamp (DTS), present only with a PTS
    pub dts: Option<Timestamp>,
    /// Elementary stream clock reference (ESCR)
    pub escr: Option<ClockReference>,
    /// Rate the P-STD receives the packet's bytes at, in units of 50 bytes
    /// per second (22 bits)
    pub es_rate: Option<u32>,
    /// Trick mode of a video stream
    pub dsm_trick_mode: Option<DsmTrickMode>,
    /// Private copyright information (7 bits)
    pub additional_copy_info: Option<u8>,
    /// CRC of the data bytes of the previous PES packet
    pub previous_pes_packet_crc: Option<u16>,
    /// The fields `PES_extension_flag` announces
    pub pes_extension: Option<PesExtension<'input>>,
    /// `stuffing_byte`s after the optional fields, written as `0xFF`
    pub stuffing_bytes: usize,
}

impl<'input> OptionalHeader<'input> {
    /// Creates an optional header, not scrambled, with no flag set and no
    /// field present
    #[expect(
        clippy::new_without_default,
        reason = "the specification defines no default PES header"
    )]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pes_scrambling_control: PesScramblingControl::NOT_SCRAMBLED,
            pes_priority: false,
            data_alignment_indicator: false,
            copyright: false,
            original_or_copy: false,
            pts: None,
            dts: None,
            escr: None,
            es_rate: None,
            dsm_trick_mode: None,
            additional_copy_info: None,
            previous_pes_packet_crc: None,
            pes_extension: None,
            stuffing_bytes: 0,
        }
    }

    /// Returns the value with the given `pes_scrambling_control`
    #[must_use]
    pub const fn with_pes_scrambling_control(
        mut self,
        pes_scrambling_control: PesScramblingControl,
    ) -> Self {
        self.pes_scrambling_control = pes_scrambling_control;
        self
    }

    /// Returns the value with the given `pes_priority`
    #[must_use]
    pub const fn with_pes_priority(mut self, pes_priority: bool) -> Self {
        self.pes_priority = pes_priority;
        self
    }

    /// Returns the value with the given `data_alignment_indicator`
    #[must_use]
    pub const fn with_data_alignment_indicator(mut self, data_alignment_indicator: bool) -> Self {
        self.data_alignment_indicator = data_alignment_indicator;
        self
    }

    /// Returns the value with the given `copyright`
    #[must_use]
    pub const fn with_copyright(mut self, copyright: bool) -> Self {
        self.copyright = copyright;
        self
    }

    /// Returns the value with the given `original_or_copy`
    #[must_use]
    pub const fn with_original_or_copy(mut self, original_or_copy: bool) -> Self {
        self.original_or_copy = original_or_copy;
        self
    }

    /// Returns the value with `pts` present, set to the one given
    #[must_use]
    pub const fn with_pts(mut self, pts: Timestamp) -> Self {
        self.pts = Some(pts);
        self
    }

    /// Returns the value with `dts` present, set to the one given
    #[must_use]
    pub const fn with_dts(mut self, dts: Timestamp) -> Self {
        self.dts = Some(dts);
        self
    }

    /// Returns the value with `escr` present, set to the one given
    #[must_use]
    pub const fn with_escr(mut self, escr: ClockReference) -> Self {
        self.escr = Some(escr);
        self
    }

    /// Returns the value with `es_rate` present, set to the one given
    #[must_use]
    pub const fn with_es_rate(mut self, es_rate: u32) -> Self {
        self.es_rate = Some(es_rate);
        self
    }

    /// Returns the value with `dsm_trick_mode` present, set to the one given
    #[must_use]
    pub const fn with_dsm_trick_mode(mut self, dsm_trick_mode: DsmTrickMode) -> Self {
        self.dsm_trick_mode = Some(dsm_trick_mode);
        self
    }

    /// Returns the value with `additional_copy_info` present, set to the one given
    #[must_use]
    pub const fn with_additional_copy_info(mut self, additional_copy_info: u8) -> Self {
        self.additional_copy_info = Some(additional_copy_info);
        self
    }

    /// Returns the value with `previous_pes_packet_crc` present, set to the one given
    #[must_use]
    pub const fn with_previous_pes_packet_crc(mut self, previous_pes_packet_crc: u16) -> Self {
        self.previous_pes_packet_crc = Some(previous_pes_packet_crc);
        self
    }

    /// Returns the value with `pes_extension` present, set to the one given
    #[must_use]
    pub const fn with_pes_extension(mut self, pes_extension: PesExtension<'input>) -> Self {
        self.pes_extension = Some(pes_extension);
        self
    }

    /// Returns the value with the given `stuffing_bytes`
    #[must_use]
    pub const fn with_stuffing_bytes(mut self, stuffing_bytes: usize) -> Self {
        self.stuffing_bytes = stuffing_bytes;
        self
    }

    /// Returns the `PES_header_data_length` the fields and the stuffing add up to
    #[must_use]
    pub fn pes_header_data_length(&self) -> usize {
        let five_byte_fields = [self.pts, self.dts].iter().flatten().count();
        let fixed = five_byte_fields
            .saturating_mul(5)
            .saturating_add(self.escr.map_or(0, |_| 6))
            .saturating_add(self.es_rate.map_or(0, |_| 3))
            .saturating_add(self.dsm_trick_mode.map_or(0, |_| 1))
            .saturating_add(self.additional_copy_info.map_or(0, |_| 1))
            .saturating_add(self.previous_pes_packet_crc.map_or(0, |_| 2));

        fixed
            .saturating_add(
                self.pes_extension
                    .as_ref()
                    .map_or(0, PesExtension::encoded_len),
            )
            .saturating_add(self.stuffing_bytes)
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        FIXED_LENGTH.saturating_add(self.pes_header_data_length())
    }

    /// Reads an optional header from `bytes`, which hold it whole, from the
    /// `'10'` bits to the last stuffing byte, and nothing more
    ///
    /// # Errors
    ///
    /// * [`OptionalHeaderStart`](Error::OptionalHeaderStart): the header does
    ///   not start with `'10'`.
    /// * [`PesHeaderDataLength`](Error::PesHeaderDataLength):
    ///   `PES_header_data_length` does not match `bytes`.
    /// * [`ForbiddenPtsDtsFlags`](Error::ForbiddenPtsDtsFlags):
    ///   `PTS_DTS_flags` is `'01'`.
    /// * [`Field`](Error::Field): a field runs past the end of the header.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        let mut reader = FieldReader::new(bytes);
        let mut header = Self::new();
        let bits = reader.read_u8::<2>().map_err(field_error("'10'"))?;
        if bits != 0b10 {
            return Err(Error::OptionalHeaderStart { bits });
        }
        header.pes_scrambling_control = PesScramblingControl(
            reader
                .read_u8::<2>()
                .map_err(field_error("PES_scrambling_control"))?,
        );
        let mut flag = |name| reader.read_bool().map_err(field_error(name));
        header.pes_priority = flag("PES_priority")?;
        header.data_alignment_indicator = flag("data_alignment_indicator")?;
        header.copyright = flag("copyright")?;
        header.original_or_copy = flag("original_or_copy")?;
        let pts_flag = flag("PTS_DTS_flags")?;
        let dts_flag = flag("PTS_DTS_flags")?;
        let escr_flag = flag("ESCR_flag")?;
        let es_rate_flag = flag("ES_rate_flag")?;
        let dsm_trick_mode_flag = flag("DSM_trick_mode_flag")?;
        let additional_copy_info_flag = flag("additional_copy_info_flag")?;
        let pes_crc_flag = flag("PES_CRC_flag")?;
        let pes_extension_flag = flag("PES_extension_flag")?;
        let pes_header_data_length = reader
            .read_u8::<8>()
            .map_err(field_error("PES_header_data_length"))?;
        if reader.remaining_bytes() != usize::from(pes_header_data_length) {
            return Err(Error::PesHeaderDataLength {
                pes_header_data_length,
                available_bytes: reader.remaining_bytes(),
            });
        }

        match (pts_flag, dts_flag) {
            (false, true) => return Err(Error::ForbiddenPtsDtsFlags),
            (true, has_dts) => {
                header.pts = Some(read_timestamp(&mut reader, "PTS")?);
                if has_dts {
                    header.dts = Some(read_timestamp(&mut reader, "DTS")?);
                }
            }
            (false, false) => {}
        }
        if escr_flag {
            let mut read = || {
                reader.read_bits::<2>()?;
                read_marked_clock_reference(&mut reader)
            };
            header.escr = Some(read().map_err(field_error("ESCR"))?);
        }
        if es_rate_flag {
            let mut es_rate = || {
                reader.read_bits::<1>()?;
                let rate = reader.read_u32::<22>()?;
                reader.read_bits::<1>()?;
                Ok(rate)
            };
            header.es_rate = Some(es_rate().map_err(field_error("ES_rate"))?);
        }
        if dsm_trick_mode_flag {
            header.dsm_trick_mode = Some(DsmTrickMode::from(
                reader
                    .read_u8::<8>()
                    .map_err(field_error("trick_mode_control"))?,
            ));
        }
        if additional_copy_info_flag {
            let mut copy_info = || {
                reader.read_bits::<1>()?;
                reader.read_u8::<7>()
            };
            header.additional_copy_info =
                Some(copy_info().map_err(field_error("additional_copy_info"))?);
        }
        if pes_crc_flag {
            header.previous_pes_packet_crc = Some(
                reader
                    .read_u16::<16>()
                    .map_err(field_error("previous_PES_packet_CRC"))?,
            );
        }
        if pes_extension_flag {
            header.pes_extension = Some(PesExtension::read(&mut reader)?);
        }
        header.stuffing_bytes = reader.remaining_bytes();

        Ok(header)
    }

    /// Appends the optional header to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`ForbiddenPtsDtsFlags`](Error::ForbiddenPtsDtsFlags): a DTS without
    ///   a PTS.
    /// * [`EscrExtension`](Error::EscrExtension): an ESCR extension of 300 or
    ///   more.
    /// * [`TooLong`](Error::TooLong): more than 32 stuffing bytes, or the
    ///   header, `pack_header_field` or `pes_extension_field` is longer than
    ///   its length field can state.
    /// * [`Forbidden`](Error::Forbidden): `es_rate` of 0, or a trick mode
    ///   with `rep_cntrl` 0.
    /// * [`Field`](Error::Field): `es_rate`, `additional_copy_info` or a
    ///   field of the extension is wider than its field.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write_or_truncate(out, |out| self.write_fields(out))
    }

    /// Appends the fields, leaving whatever it appended on failure
    #[cfg(feature = "alloc")]
    fn write_fields(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.dts.is_some() && self.pts.is_none() {
            return Err(Error::ForbiddenPtsDtsFlags);
        }
        if self.stuffing_bytes > MAX_STUFFING_BYTES {
            return Err(Error::TooLong {
                field: "stuffing_byte",
                length: self.stuffing_bytes,
                limit: MAX_STUFFING_BYTES,
            });
        }
        if self.es_rate == Some(0) {
            return Err(Error::Forbidden {
                field: "ES_rate",
                value: 0,
            });
        }
        if self
            .dsm_trick_mode
            .and_then(DsmTrickMode::rep_cntrl)
            .is_some_and(|rep_cntrl| rep_cntrl == 0)
        {
            return Err(Error::Forbidden {
                field: "rep_cntrl",
                value: 0,
            });
        }
        if let Some(escr) = self.escr.filter(|escr| !escr.has_valid_extension()) {
            return Err(Error::EscrExtension {
                extension: escr.extension(),
            });
        }
        let length = self.pes_header_data_length();
        let pes_header_data_length = u8::try_from(length).map_err(|_| Error::TooLong {
            field: "PES_header_data_length",
            length,
            limit: usize::from(u8::MAX),
        })?;

        let mut writer = FieldWriter::new(out);
        let flags = [
            true,
            false,
            self.pes_scrambling_control.get() & 0b10 != 0,
            self.pes_scrambling_control.get() & 0b01 != 0,
            self.pes_priority,
            self.data_alignment_indicator,
            self.copyright,
            self.original_or_copy,
            self.pts.is_some(),
            self.dts.is_some(),
            self.escr.is_some(),
            self.es_rate.is_some(),
            self.dsm_trick_mode.is_some(),
            self.additional_copy_info.is_some(),
            self.previous_pes_packet_crc.is_some(),
            self.pes_extension.is_some(),
        ];
        for flag in flags {
            writer
                .write_bool(flag)
                .map_err(field_error("PES header flags"))?;
        }
        writer
            .write_bits::<8>(pes_header_data_length)
            .map_err(field_error("PES_header_data_length"))?;
        match (self.pts, self.dts) {
            (Some(pts), Some(dts)) => {
                write_timestamp(&mut writer, PTS_PREFIX, pts, "PTS")?;
                write_timestamp(&mut writer, DTS_PREFIX, dts, "DTS")?;
            }
            (Some(pts), None) => write_timestamp(&mut writer, PTS_ONLY_PREFIX, pts, "PTS")?,
            (None, _) => {}
        }
        if let Some(escr) = self.escr {
            let mut write_escr = || {
                writer.write_bits::<2>(0b11_u8)?;
                write_marked_clock_reference(&mut writer, escr)
            };
            write_escr().map_err(field_error("ESCR"))?;
        }
        if let Some(es_rate) = self.es_rate {
            let mut write_rate = || {
                writer.write_bool(true)?;
                writer.write_bits::<22>(es_rate)?;
                writer.write_bool(true)
            };
            write_rate().map_err(field_error("ES_rate"))?;
        }
        if let Some(trick_mode) = self.dsm_trick_mode {
            writer
                .write_bits::<8>(trick_mode.get())
                .map_err(field_error("trick_mode_control"))?;
        }
        if let Some(copy_info) = self.additional_copy_info {
            let mut write_copy_info = || {
                writer.write_bool(true)?;
                writer.write_bits::<7>(copy_info)
            };
            write_copy_info().map_err(field_error("additional_copy_info"))?;
        }
        if let Some(crc) = self.previous_pes_packet_crc {
            writer
                .write_bits::<16>(crc)
                .map_err(field_error("previous_PES_packet_CRC"))?;
        }
        if let Some(extension) = &self.pes_extension {
            extension.write(&mut writer)?;
        }
        writer.finish().map_err(field_error("PES header"))?;
        out.resize(out.len().saturating_add(self.stuffing_bytes), 0xff);

        Ok(())
    }
}

/// Reads a PTS or a DTS: 4 bits of prefix and 33 marked bits, 5 bytes
fn read_timestamp(reader: &mut FieldReader<'_>, field: &'static str) -> Result<Timestamp, Error> {
    let mut read = || {
        reader.read_bits::<4>()?;
        read_marked_33_bits(reader)
    };
    read()
        .map(Timestamp::from_33_bits)
        .map_err(field_error(field))
}

/// Writes a PTS or a DTS: 4 bits of prefix and 33 marked bits, 5 bytes
#[cfg(feature = "alloc")]
fn write_timestamp(
    writer: &mut FieldWriter<'_>,
    prefix: u8,
    timestamp: Timestamp,
    field: &'static str,
) -> Result<(), Error> {
    let mut write = || {
        writer.write_bits::<4>(prefix)?;
        write_marked_33_bits(writer, timestamp.get())
    };
    write().map_err(field_error(field))
}

/// Splits `body`, the bytes of a PES packet after `PES_packet_length`, into
/// its optional header, as long as `PES_header_data_length` gives, and the
/// bytes after it
///
/// # Errors
///
/// * [`PesHeaderDataLength`](Error::PesHeaderDataLength):
///   `PES_header_data_length` counts bytes past the end of `body`.
pub(super) fn split(body: &[u8]) -> Result<(&[u8], &[u8]), Error> {
    let pes_header_data_length = body.get(FIXED_LENGTH.wrapping_sub(1)).copied().unwrap_or(0);
    body.split_at_checked(usize::from(pes_header_data_length).saturating_add(FIXED_LENGTH))
        .ok_or(Error::PesHeaderDataLength {
            pes_header_data_length,
            available_bytes: body.len().saturating_sub(FIXED_LENGTH),
        })
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::OptionalHeader;
    use crate::field;
    use crate::pes::Error;
    use crate::pes::dsm_trick_mode::DsmTrickMode;
    use crate::pes::extension::{PStdBuffer, PesExtension, ProgramPacketSequenceCounter};
    use crate::pes::scrambling_control::PesScramblingControl;
    use crate::time::{ClockReference, Timestamp};

    /// An optional header with every flag set, two stuffing bytes after its
    /// fields
    const EVERY_FIELD: [u8; 56] = [
        0x9d, 0xff, 0x35, 0x39, 0x8d, 0x15, 0xcf, 0x13, 0x15, 0x1d, 0x95, 0x86, 0x43, 0xe4, 0x00,
        0x04, 0x00, 0x0e, 0x57, 0xd5, 0x79, 0xbd, 0x15, 0xd5, 0xbe, 0xef, 0xff, 0x00, 0x01, 0x02,
        0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x03, 0xaa,
        0xbb, 0xcc, 0xaa, 0xc5, 0x72, 0x34, 0x82, 0x80, 0x01, 0xff, 0xff,
    ];

    const PRIVATE_DATA: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];

    fn timestamp(value: u64) -> Timestamp {
        Timestamp::try_from(value).unwrap()
    }

    #[test]
    #[expect(
        clippy::unusual_byte_groupings,
        reason = "the digits are grouped by the fields of the trick mode byte"
    )]
    fn every_optional_field_is_read_into_its_value() {
        let mut extension = PesExtension::new();
        extension.pes_private_data = Some(&PRIVATE_DATA);
        extension.pack_header_field = Some(&[0xaa, 0xbb, 0xcc]);
        extension.program_packet_sequence_counter =
            Some(ProgramPacketSequenceCounter::new(0x2a, true, 5));
        extension.p_std_buffer = Some(PStdBuffer::new(true, 0x1234));
        extension.pes_extension_field = Some(&[0x80, 0x01]);
        let mut expected = OptionalHeader::new();
        expected.pes_scrambling_control = PesScramblingControl::try_from(0b01).unwrap();
        expected.pes_priority = true;
        expected.data_alignment_indicator = true;
        expected.original_or_copy = true;
        expected.pts = Some(timestamp(0x1_2345_6789));
        expected.dts = Some(timestamp(0x0_8765_4321));
        expected.escr = Some(ClockReference::new(0x1_0000_0001, 299).unwrap());
        expected.es_rate = Some(0x2a_bcde);
        expected.dsm_trick_mode = Some(DsmTrickMode::from(0b000_10_1_01));
        expected.additional_copy_info = Some(0x55);
        expected.previous_pes_packet_crc = Some(0xbeef);
        expected.pes_extension = Some(extension);
        expected.stuffing_bytes = 2;

        assert_eq!(OptionalHeader::read(&EVERY_FIELD), Ok(expected));
    }

    #[test]
    fn a_header_built_from_new_and_every_with_method_equals_one_with_every_field_assigned() {
        let mut extension = PesExtension::new();
        extension.pes_private_data = Some(&PRIVATE_DATA);
        extension.pack_header_field = Some(&[0xaa, 0xbb, 0xcc]);
        extension.program_packet_sequence_counter =
            Some(ProgramPacketSequenceCounter::new(0x2a, true, 5));
        extension.p_std_buffer = Some(PStdBuffer::new(true, 0x1234));
        extension.pes_extension_field = Some(&[0x80, 0x01]);
        let mut assigned = OptionalHeader::new();
        assigned.pes_scrambling_control = PesScramblingControl::try_from(0b01).unwrap();
        assigned.pes_priority = true;
        assigned.data_alignment_indicator = true;
        assigned.copyright = true;
        assigned.original_or_copy = true;
        assigned.pts = Some(timestamp(0x1_2345_6789));
        assigned.dts = Some(timestamp(0x0_8765_4321));
        assigned.escr = Some(ClockReference::new(0x1_0000_0001, 299).unwrap());
        assigned.es_rate = Some(0x2a_bcde);
        assigned.dsm_trick_mode = Some(DsmTrickMode::from(0x15));
        assigned.additional_copy_info = Some(0x55);
        assigned.previous_pes_packet_crc = Some(0xbeef);
        assigned.pes_extension = Some(extension);
        assigned.stuffing_bytes = 2;

        let built = OptionalHeader::new()
            .with_pes_scrambling_control(PesScramblingControl::try_from(0b01).unwrap())
            .with_pes_priority(true)
            .with_data_alignment_indicator(true)
            .with_copyright(true)
            .with_original_or_copy(true)
            .with_pts(timestamp(0x1_2345_6789))
            .with_dts(timestamp(0x0_8765_4321))
            .with_escr(ClockReference::new(0x1_0000_0001, 299).unwrap())
            .with_es_rate(0x2a_bcde)
            .with_dsm_trick_mode(DsmTrickMode::from(0x15))
            .with_additional_copy_info(0x55)
            .with_previous_pes_packet_crc(0xbeef)
            .with_pes_extension(
                PesExtension::new()
                    .with_pes_private_data(&PRIVATE_DATA)
                    .with_pack_header_field(&[0xaa, 0xbb, 0xcc])
                    .with_program_packet_sequence_counter(ProgramPacketSequenceCounter::new(
                        0x2a, true, 5,
                    ))
                    .with_p_std_buffer(PStdBuffer::new(true, 0x1234))
                    .with_pes_extension_field(&[0x80, 0x01]),
            )
            .with_stuffing_bytes(2);

        assert_eq!(built, assigned);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn every_optional_field_is_written_back_as_read() {
        let mut written = Vec::new();
        OptionalHeader::read(&EVERY_FIELD)
            .unwrap()
            .write(&mut written)
            .unwrap();

        assert_eq!(written, EVERY_FIELD);
    }

    #[test]
    fn a_pts_with_marker_bits_of_zero_is_read_for_its_value() {
        let cleared = OptionalHeader::read(&[0x80, 0x80, 0x05, 0x20, 0x00, 0x04, 0xbf, 0x20]);
        let set = OptionalHeader::read(&[0x80, 0x80, 0x05, 0x21, 0x00, 0x05, 0xbf, 0x21]);

        assert_eq!(
            (cleared.clone().map(|header| header.pts), cleared),
            (Ok(Some(timestamp(90_000))), set)
        );
    }

    #[test]
    fn a_dts_without_a_pts_is_forbidden() {
        assert_eq!(
            OptionalHeader::read(&[0x80, 0x40, 0x05, 0x11, 0x00, 0x05, 0xbf, 0x21]),
            Err(Error::ForbiddenPtsDtsFlags)
        );
    }

    #[test]
    fn a_header_that_does_not_start_with_10_is_refused() {
        assert_eq!(
            OptionalHeader::read(&[0x40, 0x00, 0x00]),
            Err(Error::OptionalHeaderStart { bits: 0b01 })
        );
    }

    #[test]
    fn a_field_its_flag_announces_but_that_runs_past_the_header_names_itself() {
        assert_eq!(
            OptionalHeader::read(&[0x80, 0x80, 0x02, 0x21, 0x00]),
            Err(Error::Field {
                field: "PTS",
                error: field::Error::Truncated {
                    required_bits: 15,
                    remaining_bits: 8
                }
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn values_the_specification_forbids_are_refused_before_a_byte_is_written() {
        let mut escr = OptionalHeader::new();
        escr.escr = Some(ClockReference::new(0, 300).unwrap());
        let mut dts_alone = OptionalHeader::new();
        dts_alone.dts = Some(timestamp(0));
        let mut wide_rate = OptionalHeader::new();
        wide_rate.es_rate = Some(1 << 22);
        let mut zero_rate = OptionalHeader::new();
        zero_rate.es_rate = Some(0);
        let mut slow_motion = OptionalHeader::new();
        slow_motion.dsm_trick_mode = Some(DsmTrickMode::from(0b0010_0000));
        let mut slow_reverse = OptionalHeader::new();
        slow_reverse.dsm_trick_mode = Some(DsmTrickMode::from(0b1000_0000));
        let mut stuffed = OptionalHeader::new();
        stuffed.stuffing_bytes = 33;
        let mut written = Vec::new();

        assert_eq!(
            [
                escr.write(&mut written),
                dts_alone.write(&mut written),
                wide_rate.write(&mut written),
                zero_rate.write(&mut written),
                slow_motion.write(&mut written),
                slow_reverse.write(&mut written),
                stuffed.write(&mut written),
            ],
            [
                Err(Error::EscrExtension { extension: 300 }),
                Err(Error::ForbiddenPtsDtsFlags),
                Err(Error::Field {
                    field: "ES_rate",
                    error: field::Error::ValueTooWide {
                        width: 22,
                        value: 1 << 22
                    }
                }),
                Err(Error::Forbidden {
                    field: "ES_rate",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "rep_cntrl",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "rep_cntrl",
                    value: 0
                }),
                Err(Error::TooLong {
                    field: "stuffing_byte",
                    length: 33,
                    limit: 32
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
