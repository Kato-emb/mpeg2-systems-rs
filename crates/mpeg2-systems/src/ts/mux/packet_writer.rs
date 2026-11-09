//! Writer of Transport Packets: a payload unit, a PES packet or a section,
//! cut into Transport Packets, and the Transport Packet of a PCR on its own

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::Error;
use crate::field::write_or_truncate;
use crate::pes::PesPacket;
use crate::psi::cat::CaSection;
use crate::psi::pat::ProgramAssociationSection;
use crate::psi::pmt::TsProgramMapSection;
use crate::psi::section::Section;
use crate::time::ClockReference;
use crate::ts::adaptation_field::{AdaptationField, MAX_ADAPTATION_FIELD_LENGTH};
use crate::ts::packet::{HEADER_LENGTH, PACKET_LENGTH, TransportPacket};
use crate::ts::{ContinuityCounter, Pid};

/// Writer of Transport Packets, which cuts each PES packet or section into
/// the Transport Packets of a PID and appends them, in the order given, to a
/// buffer the caller owns
///
/// The writer keeps the `continuity_counter` of each PID it has written, and
/// nothing else; a write that fails leaves the buffer and the counters as
/// they were. It does not decide the order of the packets in the stream, the
/// value of a PCR or how far apart PCRs are, how often the PAT and the PMT
/// are sent, or which PID carries what. It does not write null packets or
/// keep a constant bit rate, and puts no more than one section in a
/// Transport Packet.
#[derive(Debug, Clone, Default)]
pub struct PacketWriter {
    /// The `continuity_counter` the next Transport Packet of each PID takes
    // Why not the counter of the last packet written: a PID not in the map
    // then starts at 0 without a second case.
    continuity_counters: BTreeMap<Pid, ContinuityCounter>,
}

impl PacketWriter {
    /// Returns a writer that has written nothing
    #[must_use]
    pub const fn new() -> Self {
        Self {
            continuity_counters: BTreeMap::new(),
        }
    }

    /// Appends a Transport Packet of `pid` that carries
    /// `program_clock_reference` and nothing else to `out`
    ///
    /// PCR, ISO/IEC 13818-1 §2.4.3.4 and §2.4.3.5
    ///
    /// The packet is an adaptation field filling the whole packet, its
    /// `PCR_flag` set, with no payload; how often to send one is up to the
    /// caller. The packet takes the `continuity_counter` of the packet of
    /// `pid` written before it (§2.4.3.3). A write that fails leaves `out`
    /// as it was.
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pid` is not one Table 2-3 lets carry a PCR:
    ///   0x0000, 0x0001 and 0x0010 to 0x1FFE.
    /// * [`Packet`](Error::Packet): [`TransportPacket::write`] refuses the
    ///   packet, as it does a PCR with an extension of 300 or more.
    pub fn write_program_clock_reference(
        &mut self,
        pid: Pid,
        program_clock_reference: ClockReference,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        if !pid.may_carry_pcr() {
            return Err(Error::Pid { pid });
        }
        self.write_pcr_packet(pid, program_clock_reference, out)
    }

    /// Appends `packet` to `out` as Transport Packets of `pid`, and returns
    /// how many it wrote
    ///
    /// PES packet, ISO/IEC 13818-1 §2.4.3.6, carried as §2.4.3.3 lays out
    ///
    /// One PES packet goes into as many Transport Packets as it needs (where
    /// `ps::mux::PackWriter::write_pes_packets` splits a payload into PES
    /// packets). The first has `payload_unit_start_indicator` set and, when
    /// `pcr` is given, the PCR in its adaptation field; the last is filled out
    /// with stuffing bytes in its adaptation field. For video PES packets too
    /// long for `PES_packet_length`, the caller must set
    /// `length_unspecified` (§2.4.3.7).
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pid` is below 0x0010 or that of null packets.
    /// * [`StreamId`](Error::StreamId): the `stream_id` is below 0xBD, not
    ///   that of a PES packet.
    /// * [`PesPacket`](Error::PesPacket): [`PesPacket::write`] refuses the
    ///   packet, as it does one too long for `PES_packet_length` without
    ///   `length_unspecified`, or `length_unspecified` on a stream other than
    ///   video.
    /// * [`Packet`](Error::Packet): [`TransportPacket::write`] refuses a
    ///   packet, as it does a PCR with an extension of 300 or more.
    pub fn write_pes_packet(
        &mut self,
        pid: Pid,
        packet: &PesPacket<'_>,
        pcr: Option<ClockReference>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        if !pid.is_assignable() {
            return Err(Error::Pid { pid });
        }
        let stream_id = packet.stream_id;
        if !stream_id.is_pes_packet() {
            return Err(Error::StreamId { stream_id });
        }
        let adaptation_field = pcr.map(|pcr| {
            let mut adaptation_field = AdaptationField::new();
            adaptation_field.program_clock_reference = Some(pcr);
            adaptation_field
        });
        self.write_payload_unit(
            pid,
            |out| {
                packet
                    .write(out)
                    .map_err(|error| Error::PesPacket { error })
            },
            adaptation_field,
            Stuffing::AdaptationField,
            out,
        )
    }

    /// Appends `section` to `out` as Transport Packets of `pid`, and returns
    /// how many it wrote
    ///
    /// Section, ISO/IEC 13818-1 §2.4.4, carried as §2.4.4.1 lays out
    ///
    /// The first packet has `payload_unit_start_indicator` set and a
    /// `pointer_field` of 0 before the section; the last is filled out with
    /// `0xFF` after the section. Each section starts a packet of its own.
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pid` is that of null packets.
    /// * [`Section`](Error::Section): [`Section::write`] refuses the section.
    pub fn write_section(
        &mut self,
        pid: Pid,
        section: &Section<'_>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.write_section_with(
            pid,
            |out| section.write(out).map_err(|error| Error::Section { error }),
            out,
        )
    }

    /// Appends `section`, the PAT, to `out` as Transport Packets of the PID
    /// of the PAT, and returns how many it wrote
    ///
    /// The packets are those [`write_section`](Self::write_section) writes.
    ///
    /// # Errors
    ///
    /// * [`ProgramAssociationSection`](Error::ProgramAssociationSection):
    ///   [`ProgramAssociationSection::write`] refuses the section.
    pub fn write_program_association_section(
        &mut self,
        section: &ProgramAssociationSection<'_>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.write_section_with(
            Pid::PAT,
            |out| {
                section
                    .write(out)
                    .map_err(|error| Error::ProgramAssociationSection { error })
            },
            out,
        )
    }

    /// Appends `section`, a PMT, to `out` as Transport Packets of `pid`, its
    /// `program_map_PID`, and returns how many it wrote
    ///
    /// The packets are those [`write_section`](Self::write_section) writes.
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pid` is that of null packets.
    /// * [`TsProgramMapSection`](Error::TsProgramMapSection):
    ///   [`TsProgramMapSection::write`] refuses the section.
    pub fn write_ts_program_map_section(
        &mut self,
        pid: Pid,
        section: &TsProgramMapSection<'_>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.write_section_with(
            pid,
            |out| {
                section
                    .write(out)
                    .map_err(|error| Error::TsProgramMapSection { error })
            },
            out,
        )
    }

    /// Appends `section`, the CAT, to `out` as Transport Packets of the PID
    /// of the CAT, and returns how many it wrote
    ///
    /// The packets are those [`write_section`](Self::write_section) writes.
    ///
    /// # Errors
    ///
    /// * [`CaSection`](Error::CaSection): [`CaSection::write`] refuses the
    ///   section.
    pub fn write_ca_section(
        &mut self,
        section: &CaSection<'_>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.write_section_with(
            Pid::CAT,
            |out| {
                section
                    .write(out)
                    .map_err(|error| Error::CaSection { error })
            },
            out,
        )
    }
}

/// Bytes of a Transport Packet after its header
const PAYLOAD_CAPACITY: usize = PACKET_LENGTH - HEADER_LENGTH;

/// `pointer_field` 0: the section starts right after it
const POINTER_FIELD: u8 = 0x00;

/// What fills a Transport Packet the end of a payload unit leaves short
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stuffing {
    /// Stuffing bytes in the adaptation field, the only stuffing allowed for
    /// PES packets (§2.4.3.5)
    AdaptationField,
    /// `0xFF` in the payload after the last byte of a section (§2.4.4.1)
    Payload,
}

/// Returns how many Transport Packets [`PacketWriter::write_payload_unit`]
/// cuts a payload unit of `unit_length` bytes into, the first carrying an
/// adaptation field of `adaptation_field_length` bytes
pub(super) const fn packet_count(unit_length: usize, adaptation_field_length: usize) -> usize {
    let first_capacity = PAYLOAD_CAPACITY.saturating_sub(adaptation_field_length);
    unit_length
        .saturating_sub(first_capacity)
        .div_ceil(PAYLOAD_CAPACITY)
        .saturating_add(1)
}

/// Returns how many bytes of Transport Packets
/// [`PacketWriter::write_section_with`] writes a section of
/// `section_length` bytes into
pub(super) const fn section_packets_length(section_length: usize) -> usize {
    packet_count(
        section_length.saturating_add(size_of_val(&POINTER_FIELD)),
        0,
    )
    .saturating_mul(PACKET_LENGTH)
}

impl PacketWriter {
    /// Appends a Transport Packet of `pid` that carries
    /// `program_clock_reference` in an adaptation field filling the packet,
    /// and no payload
    pub(super) fn write_pcr_packet(
        &self,
        pid: Pid,
        program_clock_reference: ClockReference,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        // Why the counter before the next one rather than the next one: a
        // packet without a payload does not advance the counter (§2.4.3.3),
        // so it repeats that of the packet before it, and the next packet
        // with a payload follows on from it. On a PID not yet written the
        // same counter makes the first packet with a payload follow on too.
        let previous = self
            .continuity_counters
            .get(&pid)
            .copied()
            .unwrap_or(ContinuityCounter(0))
            .previous();
        let mut adaptation_field = AdaptationField::new();
        adaptation_field.program_clock_reference = Some(program_clock_reference);
        adaptation_field.stuffing_bytes =
            MAX_ADAPTATION_FIELD_LENGTH.saturating_sub(adaptation_field.adaptation_field_length());
        let mut packet = TransportPacket::new(pid, previous);
        packet.adaptation_field = Some(adaptation_field);
        packet.write(out).map_err(|error| Error::Packet { error })
    }

    /// Appends the section `write_section` writes as Transport Packets of
    /// `pid`, after a `pointer_field` of 0, and returns how many it wrote
    pub(super) fn write_section_with(
        &mut self,
        pid: Pid,
        write_section: impl FnOnce(&mut Vec<u8>) -> Result<(), Error>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        if pid == Pid::NULL {
            return Err(Error::Pid { pid });
        }
        self.write_payload_unit(
            pid,
            |out| {
                out.push(POINTER_FIELD);
                write_section(out)
            },
            None,
            Stuffing::Payload,
            out,
        )
    }

    /// Appends the payload unit `write_unit` writes as Transport Packets of
    /// `pid`, the first with `adaptation_field`, and returns how many it wrote
    pub(super) fn write_payload_unit(
        &mut self,
        pid: Pid,
        write_unit: impl FnOnce(&mut Vec<u8>) -> Result<(), Error>,
        mut adaptation_field: Option<AdaptationField<'_>>,
        stuffing: Stuffing,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let mut continuity_counter = self
            .continuity_counters
            .get(&pid)
            .copied()
            .unwrap_or(ContinuityCounter(0));
        let mut written: usize = 0;
        let start = out.len();
        write_or_truncate(out, |out| {
            // Why write the unit into `out` and copy it out packet by packet,
            // then remove it: the packets need its bytes whole, and `out` is
            // the one buffer that grows to fit without the writer holding one
            // of its own.
            write_unit(out)?;
            let end = out.len();
            let mut offset = start;
            loop {
                let adaptation_field_length = adaptation_field
                    .as_ref()
                    .map_or(0, AdaptationField::encoded_len);
                let capacity = PAYLOAD_CAPACITY.saturating_sub(adaptation_field_length);
                let length = end.saturating_sub(offset).min(capacity);
                let gap = capacity.saturating_sub(length);
                let mut payload = [0xFF; PAYLOAD_CAPACITY];
                let (unit_bytes, _) = out.split_at(offset).1.split_at(length);
                payload.split_at_mut(length).0.copy_from_slice(unit_bytes);
                let payload_length = match stuffing {
                    Stuffing::Payload => capacity,
                    Stuffing::AdaptationField => {
                        if gap > 0 {
                            // Why grow the stuffing up to a length rather than
                            // by the gap: a new adaptation field takes one
                            // byte of the gap for its `adaptation_field_length`.
                            let filling = adaptation_field_length.saturating_add(gap);
                            let stuffed = adaptation_field.get_or_insert_with(AdaptationField::new);
                            stuffed.stuffing_bytes = stuffed
                                .stuffing_bytes
                                .saturating_add(filling.saturating_sub(stuffed.encoded_len()));
                        }
                        length
                    }
                };

                let mut packet = TransportPacket::new(pid, continuity_counter);
                packet.payload_unit_start_indicator = written == 0;
                packet.adaptation_field = adaptation_field.take();
                packet.payload = Some(payload.split_at(payload_length).0);
                packet.write(out).map_err(|error| Error::Packet { error })?;
                continuity_counter = continuity_counter.next();
                written = written.saturating_add(1);
                offset = offset.saturating_add(length);
                if offset >= end {
                    break;
                }
            }
            out.drain(start..end);
            Ok(())
        })?;
        self.continuity_counters.insert(pid, continuity_counter);
        Ok(written)
    }
}

#[cfg(test)]
mod tests;
