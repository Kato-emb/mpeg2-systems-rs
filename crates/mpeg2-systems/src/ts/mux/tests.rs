use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::pes::OptionalHeader;
use crate::psi::cat::CaSection;
use crate::psi::pat::{self, Program, ProgramAssociationSection, ProgramLoop};
use crate::psi::pmt::{self, TsProgramMapSection};
use crate::psi::{TableId, VersionNumber};
use crate::time::Timestamp;
use crate::ts::adaptation_field::{self, AdaptationField};
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
    let mut out = Vec::new();
    let error = PacketWriter::new()
        .write_pes_packet(
            pid(0x100),
            &PesPacket::new(StreamId::from(0xBC), &[1]),
            None,
            &mut out,
        )
        .unwrap_err();

    assert_eq!(
        error,
        Error::StreamId {
            stream_id: StreamId::from(0xBC)
        }
    );
    assert!(out.is_empty());
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
fn pcr_packet(pid: Pid, continuity_counter: u8, pcr: ClockReference) -> TransportPacket<'static> {
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
fn a_pcr_on_a_pid_that_cannot_carry_one_or_beyond_its_fields_is_refused_leaving_out_as_it_was() {
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
