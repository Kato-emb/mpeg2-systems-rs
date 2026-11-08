//! PES extension: the fields `PES_extension_flag` announces in the optional
//! PES header

use super::{Error, field_error};
use crate::field::FieldReader;
#[cfg(feature = "alloc")]
use crate::field::FieldWriter;

/// `program_packet_sequence_counter` and the fields that follow it
///
/// ISO/IEC 13818-1 §2.4.3.7
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramPacketSequenceCounter {
    /// Count of the PES packets of the program, modulo 128 (7 bits)
    pub program_packet_sequence_counter: u8,
    /// Whether the packet came from an ISO/IEC 11172-1 stream (`'1'`) rather
    /// than a Program Stream (`'0'`)
    pub mpeg1_mpeg2_identifier: bool,
    /// Stuffing bytes the original packet header had (6 bits)
    pub original_stuff_length: u8,
}

impl ProgramPacketSequenceCounter {
    /// Creates the fields from their values
    #[must_use]
    pub const fn new(
        program_packet_sequence_counter: u8,
        mpeg1_mpeg2_identifier: bool,
        original_stuff_length: u8,
    ) -> Self {
        Self {
            program_packet_sequence_counter,
            mpeg1_mpeg2_identifier,
            original_stuff_length,
        }
    }
}

/// Size of the input buffer of the P-STD for the stream
///
/// `P-STD_buffer_scale` and `P-STD_buffer_size`, ISO/IEC 13818-1 §2.4.3.7
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PStdBuffer {
    /// Whether the size counts units of 1024 bytes (`'1'`) rather than 128
    pub p_std_buffer_scale: bool,
    /// The size, in the units the scale gives (13 bits)
    pub p_std_buffer_size: u16,
}

impl PStdBuffer {
    /// Creates the fields from their values
    #[must_use]
    pub const fn new(p_std_buffer_scale: bool, p_std_buffer_size: u16) -> Self {
        Self {
            p_std_buffer_scale,
            p_std_buffer_size,
        }
    }
}

/// PES extension: the optional fields `PES_extension_flag` announces
///
/// ISO/IEC 13818-1 §2.4.3.6
///
/// `pack_header()` and the fields `PES_extension_flag_2` announces are kept
/// as their bytes, uninterpreted, so they are written back as read.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PesExtension<'input> {
    /// `PES_private_data`
    pub pes_private_data: Option<&'input [u8; 16]>,
    /// The `pack_header()` after `pack_field_length`, uninterpreted
    pub pack_header_field: Option<&'input [u8]>,
    /// `program_packet_sequence_counter` and the fields that follow it
    pub program_packet_sequence_counter: Option<ProgramPacketSequenceCounter>,
    /// `P-STD_buffer_scale` and `P-STD_buffer_size`
    pub p_std_buffer: Option<PStdBuffer>,
    /// The bytes after `PES_extension_field_length`, uninterpreted
    pub pes_extension_field: Option<&'input [u8]>,
}

impl<'input> PesExtension<'input> {
    /// Creates a PES extension with no field present
    #[expect(
        clippy::new_without_default,
        reason = "the specification defines no default PES extension"
    )]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pes_private_data: None,
            pack_header_field: None,
            program_packet_sequence_counter: None,
            p_std_buffer: None,
            pes_extension_field: None,
        }
    }

    /// Returns the number of bytes the extension occupies
    pub(super) fn encoded_len(&self) -> usize {
        let runs = [self.pack_header_field, self.pes_extension_field]
            .iter()
            .flatten()
            .fold(0_usize, |length, run| {
                length.saturating_add(1).saturating_add(run.len())
            });

        1_usize
            .saturating_add(self.pes_private_data.map_or(0, |data| data.len()))
            .saturating_add(runs)
            .saturating_add(self.program_packet_sequence_counter.map_or(0, |_| 2))
            .saturating_add(self.p_std_buffer.map_or(0, |_| 2))
    }

    /// Reads the fields `PES_extension_flag` announces
    pub(super) fn read(reader: &mut FieldReader<'input>) -> Result<Self, Error> {
        let flags = reader
            .read_u8::<8>()
            .map_err(field_error("PES extension flags"))?;
        let is_set = |bit: u8| flags & bit != 0;
        let mut extension = Self::new();

        if is_set(0b1000_0000) {
            let data = reader
                .read_bytes(16)
                .map_err(field_error("PES_private_data"))?;
            extension.pes_private_data = data.first_chunk::<16>();
        }
        if is_set(0b0100_0000) {
            let mut read = || {
                let length = reader.read_u8::<8>()?;
                reader.read_bytes(usize::from(length))
            };
            extension.pack_header_field = Some(read().map_err(field_error("pack_header"))?);
        }
        if is_set(0b0010_0000) {
            let mut read = || {
                reader.read_bits::<1>()?;
                let counter = reader.read_u8::<7>()?;
                reader.read_bits::<1>()?;
                let identifier = reader.read_bool()?;
                let stuff = reader.read_u8::<6>()?;
                Ok(ProgramPacketSequenceCounter::new(
                    counter, identifier, stuff,
                ))
            };
            extension.program_packet_sequence_counter =
                Some(read().map_err(field_error("program_packet_sequence_counter"))?);
        }
        if is_set(0b0001_0000) {
            let mut read = || {
                reader.read_bits::<2>()?;
                let scale = reader.read_bool()?;
                let size = reader.read_u16::<13>()?;
                Ok(PStdBuffer::new(scale, size))
            };
            extension.p_std_buffer = Some(read().map_err(field_error("P-STD_buffer"))?);
        }
        if is_set(0b0000_0001) {
            let mut read = || {
                reader.read_bits::<1>()?;
                let length = reader.read_u8::<7>()?;
                reader.read_bytes(usize::from(length))
            };
            extension.pes_extension_field =
                Some(read().map_err(field_error("PES_extension_field"))?);
        }

        Ok(extension)
    }

    /// Writes the fields `PES_extension_flag` announces
    #[cfg(feature = "alloc")]
    pub(super) fn write(&self, writer: &mut FieldWriter<'_>) -> Result<(), Error> {
        let length = |field: &'static str, run: &[u8], limit: u8| {
            u8::try_from(run.len())
                .ok()
                .filter(|length| *length <= limit)
                .ok_or(Error::TooLong {
                    field,
                    length: run.len(),
                    limit: usize::from(limit),
                })
        };
        let pack_header_field = self
            .pack_header_field
            .map(|run| length("pack_field_length", run, u8::MAX).map(|length| (run, length)))
            .transpose()?;
        let pes_extension_field = self
            .pes_extension_field
            .map(|run| length("PES_extension_field_length", run, 0x7f).map(|length| (run, length)))
            .transpose()?;

        let flags = [
            self.pes_private_data.is_some(),
            self.pack_header_field.is_some(),
            self.program_packet_sequence_counter.is_some(),
            self.p_std_buffer.is_some(),
            true,
            true,
            true,
            self.pes_extension_field.is_some(),
        ];
        for flag in flags {
            writer
                .write_bool(flag)
                .map_err(field_error("PES extension flags"))?;
        }
        if let Some(data) = self.pes_private_data {
            writer
                .write_bytes(data)
                .map_err(field_error("PES_private_data"))?;
        }
        if let Some((run, length)) = pack_header_field {
            let mut write = || {
                writer.write_bits::<8>(length)?;
                writer.write_bytes(run)
            };
            write().map_err(field_error("pack_header"))?;
        }
        if let Some(counter) = self.program_packet_sequence_counter {
            let mut write = || {
                writer.write_bool(true)?;
                writer.write_bits::<7>(counter.program_packet_sequence_counter)?;
                writer.write_bool(true)?;
                writer.write_bool(counter.mpeg1_mpeg2_identifier)?;
                writer.write_bits::<6>(counter.original_stuff_length)
            };
            write().map_err(field_error("program_packet_sequence_counter"))?;
        }
        if let Some(buffer) = self.p_std_buffer {
            let mut write = || {
                writer.write_bits::<2>(0b01_u8)?;
                writer.write_bool(buffer.p_std_buffer_scale)?;
                writer.write_bits::<13>(buffer.p_std_buffer_size)
            };
            write().map_err(field_error("P-STD_buffer"))?;
        }
        if let Some((run, length)) = pes_extension_field {
            let mut write = || {
                writer.write_bool(true)?;
                writer.write_bits::<7>(length)?;
                writer.write_bytes(run)
            };
            write().map_err(field_error("PES_extension_field"))?;
        }

        Ok(())
    }
}
