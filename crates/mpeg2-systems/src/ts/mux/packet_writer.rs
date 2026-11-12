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
/// keep a constant bit rate, does not guarantee that the stream fits the
/// buffers of the T-STD, and puts no more than one section in a Transport
/// Packet.
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
        // Why the counter before the next one rather than the next one: a
        // packet without a payload does not advance the counter (§2.4.3.3),
        // so it repeats that of the packet before it, and the next packet
        // with a payload follows on from it. On a PID not yet written the
        // same counter makes the first packet with a payload follow on too.
        let previous = self
            .continuity_counters
            .get(&pid)
            .copied()
            .unwrap_or(ContinuityCounter::from_bits_truncate(0))
            .previous();
        let mut adaptation_field = AdaptationField::new();
        adaptation_field.program_clock_reference = Some(program_clock_reference);
        adaptation_field.stuffing_bytes =
            MAX_ADAPTATION_FIELD_LENGTH.saturating_sub(adaptation_field.adaptation_field_length());
        let mut packet = TransportPacket::new(pid, previous);
        packet.adaptation_field = Some(adaptation_field);
        packet.write(out).map_err(|error| Error::Packet { error })
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
    /// A PES packet of `stream_id` 0xBC, the `program_stream_map`, is
    /// carried too (§2.5.4): its payload is the map after
    /// `program_stream_map_length`, written as given.
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pid` is below 0x0010 or that of null packets.
    /// * [`StreamId`](Error::StreamId): the `stream_id` is below 0xBC, not
    ///   that of a PES packet
    ///   ([`StreamId::is_pes_packet`](crate::pes::StreamId::is_pes_packet)).
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
        self.write_pes_packet_with(
            pid,
            |out| {
                packet
                    .write(out)
                    .map_err(|error| Error::PesPacket { error })
            },
            adaptation_field,
            false,
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

    /// Appends the PES packet `write_packet` writes as Transport Packets of
    /// `pid`, each with `transport_priority`, the first with
    /// `adaptation_field`, the last filled out with stuffing bytes in its
    /// adaptation field, and returns how many it wrote
    pub(super) fn write_pes_packet_with(
        &mut self,
        pid: Pid,
        write_packet: impl FnOnce(&mut Vec<u8>) -> Result<(), Error>,
        adaptation_field: Option<AdaptationField<'_>>,
        transport_priority: bool,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.write_payload_unit(
            pid,
            write_packet,
            adaptation_field,
            transport_priority,
            Stuffing::AdaptationField,
            out,
        )
    }

    /// Appends the section `write_section` writes as Transport Packets of
    /// `pid`, after a `pointer_field` of 0, and returns how many it wrote
    fn write_section_with(
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
            false,
            Stuffing::Payload,
            out,
        )
    }

    /// Appends the payload unit `write_unit` writes as Transport Packets of
    /// `pid`, each with `transport_priority`, the first with
    /// `adaptation_field`, and returns how many it wrote
    fn write_payload_unit(
        &mut self,
        pid: Pid,
        write_unit: impl FnOnce(&mut Vec<u8>) -> Result<(), Error>,
        mut adaptation_field: Option<AdaptationField<'_>>,
        transport_priority: bool,
        stuffing: Stuffing,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let mut continuity_counter = self
            .continuity_counters
            .get(&pid)
            .copied()
            .unwrap_or(ContinuityCounter::from_bits_truncate(0));
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
                packet.transport_priority = transport_priority;
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

/// Bytes of a Transport Packet after its header
const PAYLOAD_CAPACITY: usize = PACKET_LENGTH - HEADER_LENGTH;

/// `pointer_field` 0: the section starts right after it
const POINTER_FIELD: u8 = 0x00;

/// What fills a Transport Packet the end of a payload unit leaves short
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stuffing {
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

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::pes::OptionalHeader;
    use crate::pes::{self, StreamId};
    use crate::psi::cat::CaSection;
    use crate::psi::pat::{self, Program, ProgramAssociationSection, ProgramLoop};
    use crate::psi::pmt::{self, TsProgramMapSection};
    use crate::psi::section;
    use crate::psi::{TableId, VersionNumber};
    use crate::time::Timestamp;
    use crate::ts::adaptation_field::{self, AdaptationField};
    use crate::ts::packet;
    use crate::ts::packet::TransportPacket;

    /// `stream_id` of the first video stream
    const VIDEO: u8 = 0xE0;
    /// `stream_id` of the first audio stream
    const AUDIO: u8 = 0xC0;

    fn pid(value: u16) -> Pid {
        Pid::try_from(value).unwrap()
    }

    fn counter(value: u8) -> ContinuityCounter {
        ContinuityCounter::try_from(value).unwrap()
    }

    /// `length` bytes counting up, so bytes out of place differ
    fn counting(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect()
    }

    /// A PES packet of `stream_id` with a PTS, carrying `payload`
    fn pes_packet(stream_id: u8, payload: &[u8]) -> PesPacket<'_> {
        let mut header = OptionalHeader::new();
        header.pts = Some(Timestamp::try_from(90_000).unwrap());
        let mut packet = PesPacket::new(StreamId::from(stream_id), payload);
        packet.optional_header = Some(header);
        packet
    }

    /// A PES packet of `stream_id` whose bytes come to `length`, as
    /// [`PesPacket::write`] writes them
    fn pes_packet_of_length(stream_id: u8, payload: &mut Vec<u8>, length: usize) -> PesPacket<'_> {
        let header_length = pes_bytes(&pes_packet(stream_id, &[])).len();
        *payload = counting(length.checked_sub(header_length).unwrap());
        pes_packet(stream_id, payload)
    }

    fn pes_bytes(packet: &PesPacket<'_>) -> Vec<u8> {
        let mut bytes = Vec::new();
        packet.write(&mut bytes).unwrap();
        bytes
    }

    fn section_bytes(section: &Section<'_>) -> Vec<u8> {
        let mut bytes = Vec::new();
        section.write(&mut bytes).unwrap();
        bytes
    }

    /// The Transport Packets `bytes` hold, which must be a whole number of them
    fn packets(bytes: &[u8]) -> Vec<TransportPacket<'_>> {
        assert_eq!(bytes.len() % 188, 0, "not a whole number of packets");
        bytes
            .chunks(188)
            .map(|packet| TransportPacket::read(packet).unwrap())
            .collect()
    }

    /// A Transport Packet of `pid` as the writer writes it
    fn packet<'input>(
        pid: Pid,
        continuity_counter: u8,
        payload_unit_start_indicator: bool,
        adaptation_field: Option<AdaptationField<'input>>,
        payload: &'input [u8],
    ) -> TransportPacket<'input> {
        let mut packet = TransportPacket::new(pid, counter(continuity_counter));
        packet.payload_unit_start_indicator = payload_unit_start_indicator;
        packet.adaptation_field = adaptation_field;
        packet.payload = Some(payload);
        packet
    }

    /// An adaptation field of nothing but `stuffing_bytes`
    fn stuffing(stuffing_bytes: usize) -> AdaptationField<'static> {
        let mut adaptation_field = AdaptationField::new();
        adaptation_field.stuffing_bytes = stuffing_bytes;
        adaptation_field
    }

    #[test]
    fn a_pes_packet_shorter_than_a_payload_is_stuffed_in_the_adaptation_field() {
        let mut payload = Vec::new();
        let packet_to_write = pes_packet_of_length(VIDEO, &mut payload, 100);
        let bytes = pes_bytes(&packet_to_write);
        let mut out = Vec::new();

        let written = PacketWriter::new()
            .write_pes_packet(pid(0x100), &packet_to_write, None, &mut out)
            .unwrap();

        assert_eq!(written, 1);
        assert_eq!(
            packets(&out),
            [packet(pid(0x100), 0, true, Some(stuffing(83)), &bytes)]
        );
    }

    #[test]
    fn a_pes_packet_one_byte_short_of_a_payload_takes_an_empty_adaptation_field() {
        let mut payload = Vec::new();
        let packet_to_write = pes_packet_of_length(VIDEO, &mut payload, 183);
        let bytes = pes_bytes(&packet_to_write);
        let mut out = Vec::new();

        PacketWriter::new()
            .write_pes_packet(pid(0x100), &packet_to_write, None, &mut out)
            .unwrap();

        assert_eq!(
            packets(&out),
            [packet(pid(0x100), 0, true, Some(stuffing(0)), &bytes)]
        );
    }

    #[test]
    fn a_pes_packet_of_a_payload_exactly_takes_no_adaptation_field() {
        let mut payload = Vec::new();
        let packet_to_write = pes_packet_of_length(VIDEO, &mut payload, 184);
        let bytes = pes_bytes(&packet_to_write);
        let mut out = Vec::new();

        PacketWriter::new()
            .write_pes_packet(pid(0x100), &packet_to_write, None, &mut out)
            .unwrap();

        assert_eq!(packets(&out), [packet(pid(0x100), 0, true, None, &bytes)]);
    }

    #[test]
    fn a_pes_packet_one_byte_past_a_payload_continues_into_a_stuffed_packet() {
        let mut payload = Vec::new();
        let packet_to_write = pes_packet_of_length(VIDEO, &mut payload, 185);
        let bytes = pes_bytes(&packet_to_write);
        let (first, second) = bytes.split_at(184);
        let mut out = Vec::new();

        let written = PacketWriter::new()
            .write_pes_packet(pid(0x100), &packet_to_write, None, &mut out)
            .unwrap();

        assert_eq!(written, 2);
        assert_eq!(
            packets(&out),
            [
                packet(pid(0x100), 0, true, None, first),
                packet(pid(0x100), 1, false, Some(stuffing(182)), second),
            ]
        );
    }

    #[test]
    fn a_pcr_given_goes_in_the_adaptation_field_of_the_first_packet() {
        let mut payload = Vec::new();
        let packet_to_write = pes_packet_of_length(VIDEO, &mut payload, 200);
        let bytes = pes_bytes(&packet_to_write);
        let (first, second) = bytes.split_at(176);
        let pcr = ClockReference::new(123_456, 78).unwrap();
        let mut out = Vec::new();

        PacketWriter::new()
            .write_pes_packet(pid(0x100), &packet_to_write, Some(pcr), &mut out)
            .unwrap();

        let mut with_pcr = AdaptationField::new();
        with_pcr.program_clock_reference = Some(pcr);
        assert_eq!(
            packets(&out),
            [
                packet(pid(0x100), 0, true, Some(with_pcr), first),
                packet(pid(0x100), 1, false, Some(stuffing(159)), second),
            ]
        );
    }

    #[test]
    fn a_pcr_on_a_short_pes_packet_shares_its_adaptation_field_with_the_stuffing() {
        let mut payload = Vec::new();
        let packet_to_write = pes_packet_of_length(VIDEO, &mut payload, 100);
        let bytes = pes_bytes(&packet_to_write);
        let pcr = ClockReference::new(0, 0).unwrap();
        let mut out = Vec::new();

        PacketWriter::new()
            .write_pes_packet(pid(0x100), &packet_to_write, Some(pcr), &mut out)
            .unwrap();

        let mut with_pcr = AdaptationField::new();
        with_pcr.program_clock_reference = Some(pcr);
        with_pcr.stuffing_bytes = 76;
        assert_eq!(
            packets(&out),
            [packet(pid(0x100), 0, true, Some(with_pcr), &bytes)]
        );
    }

    #[test]
    fn a_video_pes_packet_longer_than_its_length_field_is_written_unbounded() {
        let payload = counting(70_000);
        let mut packet_to_write = pes_packet(VIDEO, &payload);
        packet_to_write.length_unspecified = true;
        let bytes = pes_bytes(&packet_to_write);
        let mut out = Vec::new();

        let written = PacketWriter::new()
            .write_pes_packet(pid(0x100), &packet_to_write, None, &mut out)
            .unwrap();

        assert_eq!(written, bytes.len().div_ceil(184));
        let joined: Vec<u8> = packets(&out)
            .iter()
            .flat_map(|packet| packet.payload.unwrap().iter().copied())
            .collect();
        assert_eq!(joined, bytes);
    }

    #[test]
    fn an_audio_pes_packet_longer_than_its_length_field_is_refused_leaving_out_as_it_was() {
        let payload = counting(70_000);
        let packet_to_write = pes_packet(AUDIO, &payload);
        let mut out = vec![0x47];
        let mut writer = PacketWriter::new();

        let error = writer
            .write_pes_packet(pid(0x101), &packet_to_write, None, &mut out)
            .unwrap_err();

        assert!(
            matches!(
                error,
                Error::PesPacket {
                    error: pes::Error::TooLong { .. }
                }
            ),
            "{error:?}"
        );
        assert_eq!(out, [0x47]);
    }

    #[test]
    fn a_padding_byte_other_than_0xff_is_refused_as_the_pes_packet_refuses_it() {
        let mut out = vec![0x47];
        let result = PacketWriter::new().write_pes_packet(
            pid(0x100),
            &PesPacket::new(StreamId::PADDING_STREAM, &[0xff, 0x00]),
            None,
            &mut out,
        );
        assert_eq!(
            (result, out),
            (
                Err(Error::PesPacket {
                    error: pes::Error::Forbidden {
                        field: "padding_byte",
                        value: 0x00
                    }
                }),
                vec![0x47]
            )
        );
    }

    #[test]
    fn a_pes_packet_too_long_for_its_length_field_is_written_only_as_video_with_length_unspecified()
    {
        let payload = counting(70_000);
        let video = pes_packet(VIDEO, &payload);
        let mut unbounded_video = video.clone();
        unbounded_video.length_unspecified = true;
        let mut unbounded_audio = pes_packet(AUDIO, &payload);
        unbounded_audio.length_unspecified = true;
        let refusal = |packet: &PesPacket<'_>| Error::PesPacket {
            error: packet.write(&mut Vec::new()).unwrap_err(),
        };
        let mut writer = PacketWriter::new();
        let mut out = Vec::new();
        let results = [
            (pid(0x100), &video),
            (pid(0x100), &unbounded_video),
            (pid(0x101), &unbounded_audio),
        ]
        .map(|(pid, packet)| writer.write_pes_packet(pid, packet, None, &mut out));
        assert_eq!(
            results,
            [
                Err(refusal(&video)),
                Ok(pes_bytes(&unbounded_video).len().div_ceil(184)),
                Err(refusal(&unbounded_audio)),
            ]
        );
    }

    #[test]
    fn a_refused_write_leaves_the_continuity_counter_where_it_was() {
        let too_long = counting(70_000);
        let short = counting(10);
        let mut writer = PacketWriter::new();
        let mut out = Vec::new();

        writer
            .write_pes_packet(pid(0x101), &pes_packet(AUDIO, &too_long), None, &mut out)
            .unwrap_err();
        writer
            .write_pes_packet(pid(0x101), &pes_packet(AUDIO, &short), None, &mut out)
            .unwrap();

        assert_eq!(
            packets(&out).first().unwrap().continuity_counter,
            counter(0)
        );
    }

    #[test]
    fn a_pcr_with_an_extension_of_300_or_more_is_refused() {
        let pcr = ClockReference::new(0, 300).unwrap();
        let payload = counting(10);
        let mut out = Vec::new();

        let error = PacketWriter::new()
            .write_pes_packet(
                pid(0x100),
                &pes_packet(VIDEO, &payload),
                Some(pcr),
                &mut out,
            )
            .unwrap_err();

        assert_eq!(
            error,
            Error::Packet {
                error: packet::Error::AdaptationField(
                    adaptation_field::Error::ClockReferenceExtension {
                        field: "program_clock_reference",
                        extension: 300,
                    }
                )
            }
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_pes_packet_on_a_reserved_pid_is_refused() {
        let payload = counting(10);
        for refused in [Pid::PAT, Pid::CAT, pid(0x000F), Pid::NULL] {
            let mut out = Vec::new();
            let error = PacketWriter::new()
                .write_pes_packet(refused, &pes_packet(VIDEO, &payload), None, &mut out)
                .unwrap_err();
            assert_eq!(error, Error::Pid { pid: refused });
            assert!(out.is_empty());
        }
    }

    #[test]
    fn a_stream_id_that_is_not_of_a_pes_packet_is_refused() {
        for value in [0x00, 0xB9, 0xBA, 0xBB] {
            let stream_id = StreamId::from(value);
            let mut out = Vec::new();
            let error = PacketWriter::new()
                .write_pes_packet(pid(0x100), &PesPacket::new(stream_id, &[1]), None, &mut out)
                .unwrap_err();

            assert_eq!(
                (error, out),
                (Error::StreamId { stream_id }, Vec::new()),
                "{value:#04x}"
            );
        }
    }

    #[test]
    fn the_stream_id_of_the_program_stream_map_and_those_above_it_are_taken() {
        for packet in [
            PesPacket::new(StreamId::PROGRAM_STREAM_MAP, &[1]),
            pes_packet(0xBD, &[1]),
            PesPacket::new(StreamId::PROGRAM_STREAM_DIRECTORY, &[1]),
        ] {
            let mut out = Vec::new();
            let written = PacketWriter::new().write_pes_packet(pid(0x100), &packet, None, &mut out);

            assert_eq!(
                (
                    written,
                    packets(&out).first().and_then(|packet| packet.payload)
                ),
                (Ok(1), Some(&pes_bytes(&packet)[..])),
                "{:#04x}",
                packet.stream_id.get()
            );
        }
    }

    #[test]
    fn the_continuity_counter_runs_on_per_pid_and_wraps_after_15() {
        let payload = counting(10);
        let mut writer = PacketWriter::new();
        let mut out = Vec::new();
        for _ in 0..17 {
            writer
                .write_pes_packet(pid(0x100), &pes_packet(VIDEO, &payload), None, &mut out)
                .unwrap();
            writer
                .write_pes_packet(pid(0x101), &pes_packet(AUDIO, &payload), None, &mut out)
                .unwrap();
        }
        writer
            .write_pes_packet(pid(0x101), &pes_packet(AUDIO, &payload), None, &mut out)
            .unwrap();

        let counters = |of: Pid| -> Vec<u8> {
            packets(&out)
                .iter()
                .filter(|packet| packet.pid == of)
                .map(|packet| packet.continuity_counter.get())
                .collect()
        };
        let expected: Vec<u8> = (0..17).map(|index| index % 16).collect();
        assert_eq!(counters(pid(0x100)), expected);
        assert_eq!(counters(pid(0x101)), [expected, vec![1]].concat());
    }

    #[test]
    fn a_section_starts_after_a_pointer_field_of_0_and_is_stuffed_with_0xff() {
        let body = counting(20);
        let section = Section::new(TableId::from(0x80), &body);
        let mut payload = vec![0x00];
        payload.extend(section_bytes(&section));
        payload.resize(184, 0xFF);
        let mut out = Vec::new();

        let written = PacketWriter::new()
            .write_section(pid(0x1000), &section, &mut out)
            .unwrap();

        assert_eq!(written, 1);
        assert_eq!(
            packets(&out),
            [packet(pid(0x1000), 0, true, None, &payload)]
        );
    }

    #[test]
    fn a_section_longer_than_a_payload_continues_without_a_pointer_field() {
        let body = counting(300);
        let section = Section::new(TableId::from(0x80), &body);
        let bytes = section_bytes(&section);
        let (head, tail) = bytes.split_at(183);
        let mut first = vec![0x00];
        first.extend_from_slice(head);
        let mut second = tail.to_vec();
        second.resize(184, 0xFF);
        let mut out = Vec::new();

        let written = PacketWriter::new()
            .write_section(pid(0x1000), &section, &mut out)
            .unwrap();

        assert_eq!(written, 2);
        assert_eq!(
            packets(&out),
            [
                packet(pid(0x1000), 0, true, None, &first),
                packet(pid(0x1000), 1, false, None, &second),
            ]
        );
    }

    #[test]
    fn a_section_on_the_pid_of_null_packets_is_refused() {
        let section = Section::new(TableId::from(0x80), &[]);
        let mut out = Vec::new();

        let error = PacketWriter::new()
            .write_section(Pid::NULL, &section, &mut out)
            .unwrap_err();

        assert_eq!(error, Error::Pid { pid: Pid::NULL });
        assert!(out.is_empty());
    }

    #[test]
    fn a_section_the_section_writer_refuses_is_refused_leaving_out_as_it_was() {
        let section = Section::new(TableId::FORBIDDEN, &[]);
        let mut out = vec![0x47];

        let error = PacketWriter::new()
            .write_section(Pid::PAT, &section, &mut out)
            .unwrap_err();

        assert_eq!(
            error,
            Error::Section {
                error: section::Error::ForbiddenTableId
            }
        );
        assert_eq!(out, [0x47]);
    }

    /// The payload of the one Transport Packet a short section goes into
    fn section_payload(section: &[u8]) -> Vec<u8> {
        let mut payload = vec![0x00];
        payload.extend_from_slice(section);
        payload.resize(184, 0xFF);
        payload
    }

    #[test]
    fn a_pat_goes_on_the_pat_pid_as_its_section() {
        let mut programs = Vec::new();
        ProgramLoop::write_from(
            [Program::ProgramMap {
                program_number: 1.try_into().unwrap(),
                program_map_pid: pid(0x1000),
            }],
            &mut programs,
        )
        .unwrap();
        let mut pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0).unwrap());
        pat.programs = ProgramLoop::from(&programs[..]);
        let mut bytes = Vec::new();
        pat.write(&mut bytes).unwrap();
        let payload = section_payload(&bytes);
        let mut out = Vec::new();

        let written = PacketWriter::new()
            .write_program_association_section(&pat, &mut out)
            .unwrap();

        assert_eq!(written, 1);
        assert_eq!(packets(&out), [packet(Pid::PAT, 0, true, None, &payload)]);
    }

    #[test]
    fn a_pmt_goes_on_the_pid_given_as_its_section() {
        let mut pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0).unwrap());
        pmt.pcr_pid = Some(pid(0x100));
        let mut bytes = Vec::new();
        pmt.write(&mut bytes).unwrap();
        let payload = section_payload(&bytes);
        let mut out = Vec::new();

        PacketWriter::new()
            .write_ts_program_map_section(pid(0x1000), &pmt, &mut out)
            .unwrap();

        assert_eq!(
            packets(&out),
            [packet(pid(0x1000), 0, true, None, &payload)]
        );
    }

    #[test]
    fn a_cat_goes_on_the_cat_pid_as_its_section() {
        let cat = CaSection::new(VersionNumber::try_from(0).unwrap());
        let mut bytes = Vec::new();
        cat.write(&mut bytes).unwrap();
        let payload = section_payload(&bytes);
        let mut out = Vec::new();

        PacketWriter::new()
            .write_ca_section(&cat, &mut out)
            .unwrap();

        assert_eq!(packets(&out), [packet(Pid::CAT, 0, true, None, &payload)]);
    }

    #[test]
    fn a_table_its_writer_refuses_is_refused_leaving_out_and_the_counter_as_they_were() {
        let mut programs = Vec::new();
        let program = Program::ProgramMap {
            program_number: 1.try_into().unwrap(),
            program_map_pid: pid(0x1000),
        };
        ProgramLoop::write_from([program, program], &mut programs).unwrap();
        let mut pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0).unwrap());
        pat.programs = ProgramLoop::from(&programs[..]);
        let mut pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0).unwrap());
        pmt.pcr_pid = Some(pid(0x0005));
        let mut writer = PacketWriter::new();
        let mut out = vec![0x47];

        let errors = [
            writer.write_program_association_section(&pat, &mut out),
            writer.write_ts_program_map_section(pid(0x1000), &pmt, &mut out),
            writer.write_ts_program_map_section(
                Pid::NULL,
                &TsProgramMapSection::new(1, VersionNumber::try_from(0).unwrap()),
                &mut out,
            ),
        ];

        assert_eq!(
            errors,
            [
                Err(Error::ProgramAssociationSection {
                    error: pat::Error::ProgramNumber { program_number: 1 }
                }),
                Err(Error::TsProgramMapSection {
                    error: pmt::Error::PcrPid { pid: pid(0x0005) }
                }),
                Err(Error::Pid { pid: Pid::NULL }),
            ]
        );
        assert_eq!(out, [0x47]);
        let mut one_program = Vec::new();
        ProgramLoop::write_from([program], &mut one_program).unwrap();
        pat.programs = ProgramLoop::from(&one_program[..]);
        let mut bytes = Vec::new();
        pat.write(&mut bytes).unwrap();
        let payload = section_payload(&bytes);
        let mut written = Vec::new();
        writer
            .write_program_association_section(&pat, &mut written)
            .unwrap();
        assert_eq!(
            packets(&written),
            [packet(Pid::PAT, 0, true, None, &payload)]
        );
    }

    /// The Transport Packet of a PCR on its own: an adaptation field filling the
    /// packet, with no payload
    fn pcr_packet(
        pid: Pid,
        continuity_counter: u8,
        pcr: ClockReference,
    ) -> TransportPacket<'static> {
        let mut adaptation_field = AdaptationField::new();
        adaptation_field.program_clock_reference = Some(pcr);
        adaptation_field.stuffing_bytes = 176;
        let mut packet = TransportPacket::new(pid, counter(continuity_counter));
        packet.adaptation_field = Some(adaptation_field);
        packet
    }

    #[test]
    fn a_pcr_on_its_own_fills_a_packet_with_its_adaptation_field() {
        let pcr = ClockReference::new(27_000_000 / 300, 123).unwrap();
        let mut out = Vec::new();

        PacketWriter::new()
            .write_program_clock_reference(pid(0x100), pcr, &mut out)
            .unwrap();

        assert_eq!(packets(&out), [pcr_packet(pid(0x100), 15, pcr)]);
    }

    #[test]
    fn a_pcr_on_its_own_repeats_the_continuity_counter_of_the_packet_before_it() {
        let pcr = ClockReference::new(0, 0).unwrap();
        let payload = counting(10);
        let bytes = pes_bytes(&pes_packet(VIDEO, &payload));
        let mut writer = PacketWriter::new();
        let mut out = Vec::new();

        writer
            .write_program_clock_reference(pid(0x100), pcr, &mut out)
            .unwrap();
        writer
            .write_pes_packet(pid(0x100), &pes_packet(VIDEO, &payload), None, &mut out)
            .unwrap();
        writer
            .write_program_clock_reference(pid(0x100), pcr, &mut out)
            .unwrap();
        writer
            .write_program_clock_reference(Pid::PAT, pcr, &mut out)
            .unwrap();
        writer
            .write_pes_packet(pid(0x100), &pes_packet(VIDEO, &payload), None, &mut out)
            .unwrap();

        assert_eq!(
            packets(&out),
            [
                pcr_packet(pid(0x100), 15, pcr),
                packet(
                    pid(0x100),
                    0,
                    true,
                    Some(stuffing(bytes.len().abs_diff(183))),
                    &bytes
                ),
                pcr_packet(pid(0x100), 0, pcr),
                pcr_packet(Pid::PAT, 15, pcr),
                packet(
                    pid(0x100),
                    1,
                    true,
                    Some(stuffing(bytes.len().abs_diff(183))),
                    &bytes
                ),
            ]
        );
    }

    #[test]
    fn a_pcr_on_a_pid_that_cannot_carry_one_or_beyond_its_fields_is_refused_leaving_out_as_it_was()
    {
        let pcr = ClockReference::new(0, 0).unwrap();
        let mut writer = PacketWriter::new();
        let mut out = vec![0x47];

        let errors = [
            writer.write_program_clock_reference(Pid::TSDT, pcr, &mut out),
            writer.write_program_clock_reference(Pid::NULL, pcr, &mut out),
            writer.write_program_clock_reference(
                pid(0x100),
                ClockReference::new(0, 300).unwrap(),
                &mut out,
            ),
        ];

        assert_eq!(
            errors,
            [
                Err(Error::Pid { pid: Pid::TSDT }),
                Err(Error::Pid { pid: Pid::NULL }),
                Err(Error::Packet {
                    error: packet::Error::AdaptationField(
                        adaptation_field::Error::ClockReferenceExtension {
                            field: "program_clock_reference",
                            extension: 300,
                        }
                    )
                }),
            ]
        );
        assert_eq!(out, [0x47]);
    }

    #[test]
    fn the_packets_counted_are_those_a_payload_unit_is_cut_into() {
        let mut adaptation_field = AdaptationField::new();
        adaptation_field.program_clock_reference = Some(ClockReference::from_fields(0, 0));
        let mut counted = Vec::new();
        let mut written = Vec::new();
        for length in 1..=600 {
            for field in [None, Some(adaptation_field.clone())] {
                let field_length = field.as_ref().map_or(0, AdaptationField::encoded_len);
                counted.push(packet_count(length, field_length));
                let mut out = Vec::new();
                let unit = vec![0; length];
                written.push(
                    PacketWriter::new()
                        .write_payload_unit(
                            pid(0x0100),
                            |out| {
                                out.extend_from_slice(&unit);
                                Ok(())
                            },
                            field,
                            false,
                            Stuffing::AdaptationField,
                            &mut out,
                        )
                        .unwrap(),
                );
            }
        }
        assert_eq!(counted, written);
    }
}
