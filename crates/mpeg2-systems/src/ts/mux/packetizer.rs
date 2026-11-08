//! Cutting a payload unit, a PES packet or a section, into Transport
//! Packets, and making the Transport Packet of a PCR on its own

use alloc::vec::Vec;

use super::{Error, Muxer};
use crate::field::write_or_truncate;
use crate::time::ClockReference;
use crate::ts::adaptation_field::{AdaptationField, MAX_ADAPTATION_FIELD_LENGTH};
use crate::ts::packet::{HEADER_LENGTH, PACKET_LENGTH, TransportPacket};
use crate::ts::{ContinuityCounter, Pid};

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

impl Muxer {
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
            // the one buffer that grows to fit without the muxer holding one
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
