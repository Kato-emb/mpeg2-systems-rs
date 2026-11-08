use alloc::vec;
use alloc::vec::Vec;

use crate::ts::Pid;
use crate::ts::demux::{Demuxer, DemuxerConfig};

/// A Transport Packet laid out by hand, its payload padded with 0xFF
fn packet(
    pid: u16,
    payload_unit_start_indicator: bool,
    continuity_counter: u8,
    payload: &[u8],
) -> Vec<u8> {
    let [high, low] = pid.to_be_bytes();
    let flag = if payload_unit_start_indicator {
        0x40
    } else {
        0x00
    };
    let mut bytes = vec![0x47, high | flag, low, 0x10 | continuity_counter];
    bytes.extend_from_slice(payload);
    bytes.resize(188, 0xFF);
    bytes
}

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
        demuxer.finish();
        while demuxer.poll_event().is_some() {}
        let cat = demuxer.analyzer.reassemblers.get(&Pid::CAT).unwrap();
        lengths.push(cat.buffer_len());
    }
    assert_eq!(lengths, [0; 4]);
}

#[test]
fn a_pat_across_packets_names_its_program_maps_and_is_drained_once_polled() {
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
    let learned: Vec<u16> = analyzer
        .program_map_pids
        .iter()
        .map(|pid| pid.get())
        .collect();
    let followed: Vec<u16> = analyzer.reassemblers.keys().map(|pid| pid.get()).collect();
    let pat = analyzer.reassemblers.get(&Pid::PAT).unwrap();
    assert_eq!(
        (learned, followed, pat.buffer_len()),
        (pids.clone(), [&[0x0000][..], &pids].concat(), 0)
    );
}
