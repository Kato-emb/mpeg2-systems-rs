use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::pes::{self, PesPacket};
use crate::psi::cat::CaSection;
use crate::psi::pat::ProgramAssociationSection;
use crate::psi::pmt::TsProgramMapSection;
use crate::psi::section::Section;
use crate::ts::Pid;
use crate::ts::demux::tests::packet;
use crate::ts::demux::{Demuxer, DemuxerConfig, Error, Event, PacketSize};

// Why not test through poll_event: a section kept in the buffer changes no
// event, only the memory the demuxer holds, so the test reads the buffer.
#[test]
fn a_section_that_fails_to_read_does_not_grow_the_buffer_input_after_input() {
    // A CAT section of 203 bytes whose CRC_32 fails, across two packets
    let mut section = vec![0x01, 0xB0, 200, 0x00, 0x00, 0xC1, 0x00, 0x00];
    section.resize(203, 0x00);
    let (head, tail) = section.split_at(183);
    let mut stream = Vec::new();
    for _ in 0..5 {
        stream.extend(packet(0x1FFF, false, 0, &[]));
    }
    stream.extend(packet(0x0001, true, 0, &[&[0x00][..], head].concat()));
    stream.extend(packet(0x0001, false, 1, tail));

    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    let mut lengths = Vec::new();
    for _ in 0..4 {
        demuxer.handle_input(&stream);
        while demuxer.poll_event().is_some() {}
        lengths.push(demuxer.analyzer.sections.buffer_len(Pid::CAT).unwrap());
    }
    let first = lengths.first().copied().unwrap();
    assert_eq!(lengths, [first; 4]);
}

#[test]
fn a_pat_across_packets_names_its_program_maps() {
    // A PAT of 45 programs, program_number 1 to 45 on PIDs 0x101 to 0x12D,
    // long enough to cross two packets
    let mut section = vec![0x00, 0xB0, 189, 0x00, 0x01, 0xC1, 0x00, 0x00];
    let pids: Vec<u16> = (0x0101..=0x012D).collect();
    for (program_number, pid) in (1_u16..).zip(&pids) {
        section.extend_from_slice(&program_number.to_be_bytes());
        section.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    }
    section.extend_from_slice(&crate::crc::crc_32(&section).to_be_bytes());
    let (head, tail) = section.split_at(183);
    let mut stream = Vec::new();
    for _ in 0..5 {
        stream.extend(packet(0x1FFF, false, 0, &[]));
    }
    stream.extend(packet(0x0000, true, 0, &[&[0x00][..], head].concat()));
    stream.extend(packet(0x0000, false, 1, tail));

    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    demuxer.handle_input(&stream);
    while demuxer.poll_event().is_some() {}
    let analyzer = &demuxer.analyzer;
    let followed: Vec<u16> = analyzer.sections.pids().map(Pid::get).collect();
    assert_eq!(followed, [&[0x0000][..], &pids].concat());
}

#[test]
fn a_whole_pat_version_stops_reassembling_only_the_program_maps_it_no_longer_names() {
    let versions = [
        section_packet(0x0000, 0, &pat_section(0, &[(1, 0x0100), (2, 0x0200)])),
        section_packet(0x0000, 1, &pat_section(1, &[(2, 0x0200), (3, 0x0300)])),
        section_packet(0x0000, 2, &pat_section_of(2, (0, 1), &[(4, 0x0400)])),
    ];
    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    let mut stream = behind_null_packets(&[]);
    let mut followed = Vec::new();
    for packet in versions {
        stream.extend(packet);
        events_of(&mut demuxer, &stream);
        stream.clear();
        followed.push(
            demuxer
                .analyzer
                .sections
                .pids()
                .map(Pid::get)
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        followed,
        [
            vec![0x0000, 0x0100, 0x0200],
            vec![0x0000, 0x0200, 0x0300],
            vec![0x0000, 0x0200, 0x0300, 0x0400],
        ]
    );
}

/// Every event the demuxer gives for `stream`, as `Debug` prints it, so the
/// events can outlive their borrow of the demuxer
pub(super) fn events_of(demuxer: &mut Demuxer, stream: &[u8]) -> Vec<String> {
    demuxer.handle_input(stream);
    drained(demuxer)
}

fn drained(demuxer: &mut Demuxer) -> Vec<String> {
    let mut events = Vec::new();
    while let Some(event) = demuxer.poll_event() {
        events.push(format!("{event:?}"));
    }
    events
}

fn described(event: &Event<'_>) -> String {
    format!("{event:?}")
}

fn acquired() -> String {
    described(&Event::SyncAcquired {
        packet_size: PacketSize::Standard188,
    })
}

/// A long-form section of `table_id`, with `table_id_extension`, at version
/// `version_number`, in force, its `CRC_32` computed
fn table_section(
    table_id: u8,
    table_id_extension: u16,
    version_number: u8,
    body: &[u8],
) -> Vec<u8> {
    section_of_table(table_id, table_id_extension, version_number, (0, 0), body)
}

/// A long-form section as [`table_section`] makes, `section_number` and
/// `last_section_number` given
fn section_of_table(
    table_id: u8,
    table_id_extension: u16,
    version_number: u8,
    (section_number, last_section_number): (u8, u8),
    body: &[u8],
) -> Vec<u8> {
    let length = u16::try_from(body.len().checked_add(9).unwrap()).unwrap();
    let [high, low] = length.to_be_bytes();
    let [extension_high, extension_low] = table_id_extension.to_be_bytes();
    let mut bytes = vec![
        table_id,
        0xB0 | high,
        low,
        extension_high,
        extension_low,
        0xC1 | (version_number << 1),
        section_number,
        last_section_number,
    ];
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(&crate::crc::crc_32(&bytes).to_be_bytes());
    bytes
}

pub(super) fn pat_section(version_number: u8, programs: &[(u16, u16)]) -> Vec<u8> {
    pat_section_of(version_number, (0, 0), programs)
}

/// A PAT section as [`pat_section`] makes, `section_number` and
/// `last_section_number` given
pub(super) fn pat_section_of(
    version_number: u8,
    numbers: (u8, u8),
    programs: &[(u16, u16)],
) -> Vec<u8> {
    let mut body = Vec::new();
    for (program_number, pid) in programs {
        body.extend_from_slice(&program_number.to_be_bytes());
        body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    }
    section_of_table(0x00, 0x0001, version_number, numbers, &body)
}

pub(super) fn pmt_section(
    program_number: u16,
    version_number: u8,
    streams: &[(u8, u16)],
) -> Vec<u8> {
    let mut body = vec![0xE1, 0x01, 0xF0, 0x00];
    for (stream_type, pid) in streams {
        body.push(*stream_type);
        body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        body.extend_from_slice(&[0xF0, 0x00]);
    }
    table_section(0x02, program_number, version_number, &body)
}

fn cat_section(version_number: u8, descriptors: &[u8]) -> Vec<u8> {
    table_section(0x01, 0xFFFF, version_number, descriptors)
}

/// A packet that starts `section` on `pid`
pub(super) fn section_packet(pid: u16, continuity_counter: u8, section: &[u8]) -> Vec<u8> {
    packet(
        pid,
        true,
        continuity_counter,
        &[&[0x00][..], section].concat(),
    )
}

/// Null packets enough to acquire the grid, then `packets`
pub(super) fn behind_null_packets(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut stream = Vec::new();
    for _ in 0..5 {
        stream.extend(packet(0x1FFF, false, 0, &[]));
    }
    for bytes in packets {
        stream.extend_from_slice(bytes);
    }
    stream
}

fn pat_event(bytes: &[u8]) -> String {
    let section = Section::read(bytes).unwrap();
    described(&Event::ProgramAssociationSection(
        ProgramAssociationSection::try_from(&section).unwrap(),
    ))
}

fn pmt_event(pid: u16, bytes: &[u8]) -> String {
    let section = Section::read(bytes).unwrap();
    described(&Event::TsProgramMapSection {
        pid: Pid::try_from(pid).unwrap(),
        section: TsProgramMapSection::try_from(&section).unwrap(),
    })
}

fn cat_event(bytes: &[u8]) -> String {
    let section = Section::read(bytes).unwrap();
    described(&Event::CaSection(CaSection::try_from(&section).unwrap()))
}

fn demux(packets: &[Vec<u8>]) -> Vec<String> {
    events_of(
        &mut Demuxer::new(DemuxerConfig::default()),
        &behind_null_packets(packets),
    )
}

#[test]
fn a_resent_pat_of_the_same_version_reports_once() {
    let pat = pat_section(0, &[(1, 0x0100)]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &pat),
            section_packet(0x0000, 1, &pat)
        ]),
        [acquired(), pat_event(&pat)]
    );
}

#[test]
fn a_new_pat_version_is_reported_again() {
    let first = pat_section(0, &[(1, 0x0100)]);
    let second = pat_section(1, &[(1, 0x0100)]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &first),
            section_packet(0x0000, 1, &second)
        ]),
        [acquired(), pat_event(&first), pat_event(&second)]
    );
}

#[test]
fn a_pat_announcing_its_next_version_is_not_reported() {
    let current = pat_section(0, &[(1, 0x0100)]);
    let in_force = pat_section(1, &[(2, 0x0200)]);
    let (fields, _) = in_force.split_last_chunk::<4>().unwrap();
    let mut next = fields.to_vec();
    *next.get_mut(5).unwrap() &= 0xFE;
    next.extend_from_slice(&crate::crc::crc_32(&next).to_be_bytes());
    let pmt = pmt_section(2, 0, &[]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &current),
            section_packet(0x0000, 1, &next),
            section_packet(0x0000, 2, &current),
            section_packet(0x0200, 0, &pmt),
        ]),
        [acquired(), pat_event(&current)]
    );
}

#[test]
fn a_pmt_on_a_pid_the_pat_names_is_reported() {
    let pat = pat_section(0, &[(1, 0x0100)]);
    let pmt = pmt_section(1, 0, &[(0x1B, 0x0101), (0x0F, 0x0102)]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &pat),
            section_packet(0x0100, 0, &pmt)
        ]),
        [acquired(), pat_event(&pat), pmt_event(0x0100, &pmt)]
    );
}

#[test]
fn a_resent_pmt_of_the_same_version_reports_once() {
    let pat = pat_section(0, &[(1, 0x0100)]);
    let pmt = pmt_section(1, 0, &[(0x1B, 0x0101)]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &pat),
            section_packet(0x0100, 0, &pmt),
            section_packet(0x0100, 1, &pmt),
        ]),
        [acquired(), pat_event(&pat), pmt_event(0x0100, &pmt)]
    );
}

#[test]
fn a_new_pmt_version_is_reported_again() {
    let pat = pat_section(0, &[(1, 0x0100)]);
    let first = pmt_section(1, 0, &[(0x1B, 0x0101)]);
    let second = pmt_section(1, 1, &[(0x1B, 0x0101)]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &pat),
            section_packet(0x0100, 0, &first),
            section_packet(0x0100, 1, &second),
        ]),
        [
            acquired(),
            pat_event(&pat),
            pmt_event(0x0100, &first),
            pmt_event(0x0100, &second)
        ]
    );
}

#[test]
fn pmt_versions_are_tracked_per_program() {
    let pat = pat_section(0, &[(1, 0x0100), (2, 0x0200)]);
    let first_program = pmt_section(1, 3, &[(0x1B, 0x0101)]);
    let second_program = pmt_section(2, 3, &[(0x1B, 0x0201)]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &pat),
            section_packet(0x0100, 0, &first_program),
            section_packet(0x0200, 0, &second_program),
        ]),
        [
            acquired(),
            pat_event(&pat),
            pmt_event(0x0100, &first_program),
            pmt_event(0x0200, &second_program)
        ]
    );
}

#[test]
fn a_program_dropped_from_the_pat_and_re_added_reports_its_pmt_again() {
    let naming_the_program = |version_number| pat_section(version_number, &[(1, 0x0100)]);
    let without_programs = pat_section(1, &[]);
    let pmt = pmt_section(1, 3, &[(0x1B, 0x0101)]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &naming_the_program(0)),
            section_packet(0x0100, 0, &pmt),
            section_packet(0x0000, 1, &without_programs),
            section_packet(0x0000, 2, &naming_the_program(2)),
            section_packet(0x0100, 1, &pmt),
        ]),
        [
            acquired(),
            pat_event(&naming_the_program(0)),
            pmt_event(0x0100, &pmt),
            pat_event(&without_programs),
            pat_event(&naming_the_program(2)),
            pmt_event(0x0100, &pmt),
        ]
    );
}

#[test]
fn a_pmt_on_a_pid_the_pat_no_longer_names_is_not_reported() {
    let naming_the_program = pat_section(0, &[(1, 0x0100)]);
    let without_programs = pat_section(1, &[]);
    let pmt = pmt_section(1, 3, &[(0x1B, 0x0101)]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &naming_the_program),
            section_packet(0x0000, 1, &without_programs),
            section_packet(0x0100, 0, &pmt),
        ]),
        [
            acquired(),
            pat_event(&naming_the_program),
            pat_event(&without_programs)
        ]
    );
}

#[test]
fn a_pmt_that_cannot_be_read_is_reported_as_an_error() {
    let pat = pat_section(0, &[(1, 0x0100)]);
    let pmt = table_section(0x02, 1, 0, &[0xE1, 0x01]);
    let section = Section::read(&pmt).unwrap();
    let error = TsProgramMapSection::try_from(&section).unwrap_err();
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &pat),
            section_packet(0x0100, 0, &pmt)
        ]),
        [
            acquired(),
            pat_event(&pat),
            described(&Event::Error(Error::TsProgramMapSection {
                pid: Pid::try_from(0x0100).unwrap(),
                error
            }))
        ]
    );
}

#[test]
fn a_cat_in_the_stream_is_reported() {
    let cat = cat_section(4, &[0x09, 0x04, 0x0B, 0x00, 0x10, 0x20]);
    assert_eq!(
        demux(&[section_packet(0x0001, 0, &cat)]),
        [acquired(), cat_event(&cat)]
    );
}

#[test]
fn a_resent_cat_of_the_same_version_reports_once() {
    let cat = cat_section(0, &[0x09, 0x02, 0x0B, 0x00]);
    assert_eq!(
        demux(&[
            section_packet(0x0001, 0, &cat),
            section_packet(0x0001, 1, &cat)
        ]),
        [acquired(), cat_event(&cat)]
    );
}

#[test]
fn a_new_cat_version_is_reported_again() {
    let first = cat_section(0, &[0x09, 0x02, 0x0B, 0x00]);
    let second = cat_section(1, &[0x09, 0x02, 0x0C, 0x00]);
    assert_eq!(
        demux(&[
            section_packet(0x0001, 0, &first),
            section_packet(0x0001, 1, &second)
        ]),
        [acquired(), cat_event(&first), cat_event(&second)]
    );
}

#[test]
fn a_reported_pat_version_survives_sync_loss_and_the_end_of_input() {
    let pat = pat_section(0, &[(1, 0x0100)]);
    let unsynchronized = [0x00; 188];
    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    let mut stream = behind_null_packets(&[section_packet(0x0000, 0, &pat)]);
    stream.extend_from_slice(&unsynchronized);
    stream.extend_from_slice(&unsynchronized);
    stream.extend_from_slice(&behind_null_packets(&[section_packet(0x0000, 0, &pat)]));
    let mut events = events_of(&mut demuxer, &stream);
    demuxer.finish();
    events.extend(drained(&mut demuxer));
    events.extend(events_of(
        &mut demuxer,
        &behind_null_packets(&[section_packet(0x0000, 1, &pat)]),
    ));
    assert_eq!(
        events,
        [
            acquired(),
            pat_event(&pat),
            described(&Event::Error(Error::SyncByteMissing { found: 0x00 })),
            described(&Event::Unsynchronized {
                bytes: &unsynchronized
            }),
            described(&Event::Error(Error::SyncLost)),
            described(&Event::Unsynchronized {
                bytes: &unsynchronized
            }),
            acquired(),
        ]
    );
}

#[test]
fn a_pmt_of_a_program_the_pat_does_not_name_is_reported_as_a_section() {
    let pat = pat_section(0, &[(1, 0x0100)]);
    let pmt = pmt_section(2, 0, &[(0x1B, 0x0101)]);
    let section = Section::read(&pmt).unwrap();
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &pat),
            section_packet(0x0100, 0, &pmt)
        ]),
        [
            acquired(),
            pat_event(&pat),
            described(&Event::Section {
                pid: Pid::try_from(0x0100).unwrap(),
                section
            })
        ]
    );
}

#[test]
fn a_pmt_that_cannot_be_read_is_reported_once_per_version() {
    let pat = pat_section(0, &[(1, 0x0100)]);
    let pmt = table_section(0x02, 1, 0, &[0xE1, 0x01]);
    let section = Section::read(&pmt).unwrap();
    let error = TsProgramMapSection::try_from(&section).unwrap_err();
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &pat),
            section_packet(0x0100, 0, &pmt),
            section_packet(0x0100, 1, &pmt),
        ]),
        [
            acquired(),
            pat_event(&pat),
            described(&Event::Error(Error::TsProgramMapSection {
                pid: Pid::try_from(0x0100).unwrap(),
                error
            }))
        ]
    );
}

#[test]
fn the_programs_of_a_version_stay_named_until_the_next_version_is_whole() {
    let first = pat_section(0, &[(1, 0x0100), (2, 0x0200)]);
    let next_without_program_1 = pat_section_of(1, (0, 1), &[(2, 0x0200)]);
    let next_with_program_1 = pat_section_of(1, (1, 1), &[(1, 0x0100)]);
    let pmt_0 = pmt_section(1, 0, &[(0x1B, 0x0101)]);
    let pmt_1 = pmt_section(1, 1, &[(0x1B, 0x0101)]);
    assert_eq!(
        demux(&[
            section_packet(0x0000, 0, &first),
            section_packet(0x0100, 0, &pmt_0),
            section_packet(0x0000, 1, &next_without_program_1),
            section_packet(0x0100, 1, &pmt_1),
            section_packet(0x0100, 2, &pmt_1),
            section_packet(0x0000, 2, &next_with_program_1),
            section_packet(0x0100, 3, &pmt_1),
        ]),
        [
            acquired(),
            pat_event(&first),
            pmt_event(0x0100, &pmt_0),
            pat_event(&next_without_program_1),
            pmt_event(0x0100, &pmt_1),
            pat_event(&next_with_program_1),
        ]
    );
}

#[test]
fn a_section_a_whole_version_lacks_is_new_again_when_its_version_comes_back() {
    let two_sections = [
        pat_section_of(0, (0, 1), &[(1, 0x0100)]),
        pat_section_of(0, (1, 1), &[(2, 0x0200)]),
    ];
    let pmt = pmt_section(2, 0, &[(0x1B, 0x0201)]);
    let mut continuity_counters = (0_u8..16).cycle();
    let mut on_pat =
        |section: &[u8]| section_packet(0x0000, continuity_counters.next().unwrap(), section);
    let mut packets: Vec<Vec<u8>> = two_sections.iter().map(|section| on_pat(section)).collect();
    packets.push(section_packet(0x0200, 0, &pmt));
    for version_number in 1..32 {
        packets.push(on_pat(&pat_section(version_number, &[(1, 0x0100)])));
    }
    packets.extend(two_sections.iter().map(|section| on_pat(section)));
    packets.push(section_packet(0x0200, 1, &pmt));
    let pmts: Vec<String> = demux(&packets)
        .into_iter()
        .filter(|event| event.starts_with("TsProgramMapSection"))
        .collect();
    assert_eq!(pmts, [pmt_event(0x0200, &pmt), pmt_event(0x0200, &pmt)]);
}

/// The PID the PES tests subscribe to
const PES_PID: u16 = 0x0100;

/// A PES packet of `private_stream_2`, which has no optional header, with
/// `PES_packet_length` as given and `body` after it
fn pes(pes_packet_length: u16, body: &[u8]) -> Vec<u8> {
    let [high, low] = pes_packet_length.to_be_bytes();
    [&[0x00, 0x00, 0x01, 0xBF, high, low][..], body].concat()
}

/// The payload of a [`packet`] that carries `head`: `head`, then 0xFF to
/// the end of the packet
fn stuffed(head: &[u8]) -> Vec<u8> {
    let mut payload = head.to_vec();
    payload.resize(184, 0xFF);
    payload
}

/// A demuxer subscribed to [`PES_PID`], with the limit given
fn subscribed(pes_buffer_limit: usize) -> Demuxer {
    let mut demuxer = Demuxer::new(DemuxerConfig {
        pes_buffer_limit,
        ..DemuxerConfig::default()
    });
    demuxer.subscribe_pes(Pid::try_from(PES_PID).unwrap());
    demuxer
}

/// The events of `packets`, behind null packets and before one more, then
/// those of the end of input
fn demux_to_the_end(demuxer: &mut Demuxer, packets: &[Vec<u8>]) -> Vec<String> {
    let mut stream = behind_null_packets(packets);
    stream.extend(packet(0x1FFF, false, 0, &[]));
    let mut events = events_of(demuxer, &stream);
    demuxer.finish();
    events.extend(drained(demuxer));
    events
}

/// Input offset of the first packet [`behind_null_packets`] puts after them
const FIRST_PACKET_OFFSET: u64 = 5 * 188;

#[test]
fn a_subscribed_pid_reports_its_reassembled_pes_packet() {
    let bytes = pes(3, &[0xAA, 0xBB, 0xCC]);
    assert_eq!(
        demux_to_the_end(&mut subscribed(4096), &[packet(PES_PID, true, 0, &bytes)]),
        [
            acquired(),
            described(&Event::PesPacket {
                pid: Pid::try_from(PES_PID).unwrap(),
                packet: PesPacket::read(&bytes).unwrap(),
                start_offset: FIRST_PACKET_OFFSET,
            }),
        ]
    );
}

#[test]
fn an_unsubscribed_pid_reports_no_pes_packet() {
    assert_eq!(
        demux_to_the_end(
            &mut Demuxer::new(DemuxerConfig::default()),
            &[packet(PES_PID, true, 0, &pes(3, &[0xAA, 0xBB, 0xCC]))]
        ),
        [acquired()]
    );
}

#[test]
fn unsubscribing_reports_nothing_for_the_pid_it_left() {
    let mut demuxer = subscribed(4096);
    let mut events = events_of(
        &mut demuxer,
        &behind_null_packets(&[packet(PES_PID, true, 0, &pes(400, &[0x01, 0x02]))]),
    );
    demuxer.unsubscribe_pes(Pid::try_from(PES_PID).unwrap());
    events.extend(demux_to_the_end(
        &mut demuxer,
        &[
            packet(PES_PID, false, 1, &[0x04, 0x05]),
            packet(PES_PID, true, 2, &pes(3, &[0xAA, 0xBB, 0xCC])),
        ],
    ));
    assert_eq!(events, [acquired()]);
}

#[test]
fn the_end_of_input_reports_a_pes_packet_cut_short_as_truncated() {
    let head = pes(400, &[0x01, 0x02, 0x03]);
    assert_eq!(
        demux_to_the_end(&mut subscribed(4096), &[packet(PES_PID, true, 0, &head)]),
        [
            acquired(),
            described(&Event::TruncatedPesPacket {
                pid: Pid::try_from(PES_PID).unwrap(),
                bytes: &stuffed(&head),
                start_offset: FIRST_PACKET_OFFSET,
            }),
        ]
    );
}

#[test]
fn a_continuity_break_reports_the_pes_packet_in_flight_as_truncated() {
    let head = pes(400, &[0x01, 0x02, 0x03]);
    let pid = Pid::try_from(PES_PID).unwrap();
    assert_eq!(
        demux_to_the_end(
            &mut subscribed(4096),
            &[
                packet(PES_PID, true, 0, &head),
                packet(PES_PID, false, 2, &[0x04, 0x05]),
            ]
        ),
        [
            acquired(),
            described(&Event::Error(Error::Discontinuity {
                pid,
                expected: crate::ts::ContinuityCounter::try_from(1).unwrap(),
                found: crate::ts::ContinuityCounter::try_from(2).unwrap(),
            })),
            described(&Event::TruncatedPesPacket {
                pid,
                bytes: &stuffed(&head),
                start_offset: FIRST_PACKET_OFFSET,
            }),
        ]
    );
}

#[test]
fn losing_sync_drops_the_pes_packet_in_flight_without_a_word() {
    let unsynchronized = [0x00; 188];
    let mut stream = behind_null_packets(&[packet(PES_PID, true, 0, &pes(400, &[0x01]))]);
    stream.extend_from_slice(&unsynchronized);
    stream.extend_from_slice(&unsynchronized);
    let mut demuxer = subscribed(4096);
    let mut events = events_of(&mut demuxer, &stream);
    demuxer.finish();
    events.extend(drained(&mut demuxer));
    assert_eq!(
        events,
        [
            acquired(),
            described(&Event::Error(Error::SyncByteMissing { found: 0x00 })),
            described(&Event::Unsynchronized {
                bytes: &unsynchronized
            }),
            described(&Event::Error(Error::SyncLost)),
            described(&Event::Unsynchronized {
                bytes: &unsynchronized
            }),
        ]
    );
}

#[test]
fn reassembled_bytes_that_do_not_read_as_a_pes_packet_are_reported_with_the_error() {
    let scrambled = [0xAA, 0xBB, 0xCC, 0xDD, 0x00, 0x00, 0x99];
    assert_eq!(
        demux_to_the_end(
            &mut subscribed(4096),
            &[packet(PES_PID, true, 0, &scrambled)]
        ),
        [
            acquired(),
            described(&Event::UnreadablePesPacket {
                pid: Pid::try_from(PES_PID).unwrap(),
                bytes: &stuffed(&scrambled),
                error: pes::Error::PacketStartCodePrefix {
                    packet_start_code_prefix: 0xAA_BBCC,
                },
                start_offset: FIRST_PACKET_OFFSET,
            }),
        ]
    );
}

#[test]
fn a_subscribed_pid_whose_buffer_grows_past_the_limit_reports_an_overrun() {
    assert_eq!(
        demux_to_the_end(
            &mut subscribed(300),
            &[
                packet(PES_PID, true, 0, &pes(0, &[0x01, 0x02, 0x03])),
                packet(PES_PID, false, 1, &[]),
            ]
        ),
        [
            acquired(),
            described(&Event::Error(Error::PesBufferOverrun {
                pid: Pid::try_from(PES_PID).unwrap(),
                discarded_bytes: 2 * 184,
            })),
        ]
    );
}

#[test]
fn a_packet_with_transport_error_indicator_set_ends_the_pes_packet_in_flight_as_truncated() {
    let head = pes(0, &[0x01, 0x02, 0x03]);
    let mut errored = packet(PES_PID, false, 1, &[0x04, 0x05]);
    *errored.get_mut(1).unwrap() |= 0x80;
    let pid = Pid::try_from(PES_PID).unwrap();
    assert_eq!(
        demux_to_the_end(
            &mut subscribed(4096),
            &[
                packet(PES_PID, true, 0, &head),
                errored,
                packet(PES_PID, false, 2, &[0x06, 0x07]),
            ]
        ),
        [
            acquired(),
            described(&Event::Error(Error::TransportErrorIndicator { pid })),
            described(&Event::TruncatedPesPacket {
                pid,
                bytes: &stuffed(&head),
                start_offset: FIRST_PACKET_OFFSET,
            }),
            described(&Event::Error(Error::Discontinuity {
                pid,
                expected: crate::ts::ContinuityCounter::try_from(1).unwrap(),
                found: crate::ts::ContinuityCounter::try_from(2).unwrap(),
            })),
        ]
    );
}

#[test]
fn a_pes_packet_in_scrambled_transport_packets_is_gathered_to_the_next_unit_start_unread() {
    let scrambled_start = stuffed(&[0xAA, 0xBB, 0xCC, 0xDD, 0x00, 0x01]);
    let continuation = [0x5A; 184];
    let mut first = packet(PES_PID, true, 0, &scrambled_start);
    *first.get_mut(3).unwrap() |= 0xC0;
    let mut second = packet(PES_PID, false, 1, &continuation);
    *second.get_mut(3).unwrap() |= 0xC0;
    let next = pes(1, &[0x0A]);
    let pid = Pid::try_from(PES_PID).unwrap();
    assert_eq!(
        demux_to_the_end(
            &mut subscribed(4096),
            &[first, second, packet(PES_PID, true, 2, &next)]
        ),
        [
            acquired(),
            described(&Event::ScrambledPesPacket {
                pid,
                bytes: &[&scrambled_start[..], &continuation[..]].concat(),
                transport_scrambling_control:
                    crate::ts::packet::TransportScramblingControl::try_from(0b11).unwrap(),
                start_offset: FIRST_PACKET_OFFSET,
            }),
            described(&Event::PesPacket {
                pid,
                packet: PesPacket::read(&next).unwrap(),
                start_offset: FIRST_PACKET_OFFSET + 2 * 188,
            }),
        ]
    );
}
