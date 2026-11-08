use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::psi::cat::CaSection;
use crate::psi::pat::ProgramAssociationSection;
use crate::psi::pmt::TsProgramMapSection;
use crate::psi::section::Section;
use crate::ts::Pid;
use crate::ts::demux::tests::packet;
use crate::ts::demux::{Demuxer, DemuxerConfig, Error, Event, PacketSize};

// Why not test through poll_event: a section kept in the buffer changes no
// event, only the memory the demuxer holds, so the test reads the buffer. Why
// no finish between the inputs: the end of input clears the buffer, which
// would hide a buffer that grows.
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
        let cat = demuxer.analyzer.reassemblers.get(&Pid::CAT).unwrap();
        lengths.push(cat.buffer_len());
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
    let followed: Vec<u16> = analyzer.reassemblers.keys().map(|pid| pid.get()).collect();
    assert_eq!(followed, [&[0x0000][..], &pids].concat());
}

/// Every event the demuxer gives for `stream`, as `Debug` prints it, so the
/// events can outlive their borrow of the demuxer
fn events_of(demuxer: &mut Demuxer, stream: &[u8]) -> Vec<String> {
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
        0x00,
        0x00,
    ];
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(&crate::crc::crc_32(&bytes).to_be_bytes());
    bytes
}

fn pat_section(version_number: u8, programs: &[(u16, u16)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (program_number, pid) in programs {
        body.extend_from_slice(&program_number.to_be_bytes());
        body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    }
    table_section(0x00, 0x0001, version_number, &body)
}

fn pmt_section(program_number: u16, version_number: u8, streams: &[(u8, u16)]) -> Vec<u8> {
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
fn section_packet(pid: u16, continuity_counter: u8, section: &[u8]) -> Vec<u8> {
    packet(
        pid,
        true,
        continuity_counter,
        &[&[0x00][..], section].concat(),
    )
}

/// Null packets enough to acquire the grid, then `packets`
fn behind_null_packets(packets: &[Vec<u8>]) -> Vec<u8> {
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
            described(&Event::Error(Error::SyncLost)),
            described(&Event::Unsynchronized {
                bytes: &unsynchronized
            }),
            acquired(),
        ]
    );
}
