use alloc::vec;
use alloc::vec::Vec;

use super::{Demuxer, DemuxerConfig};

/// A Transport Packet laid out by hand, its payload padded with 0xFF
pub(super) fn packet(
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

// Why not test through poll_event: bytes the framer keeps after reporting
// them change no event, only the memory the demuxer holds.
#[test]
fn input_handed_over_while_an_event_is_queued_does_not_grow_the_buffer_without_bound() {
    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    for _ in 0..5 {
        demuxer.handle_input(&packet(0x1FFF, false, 0, &[]));
    }
    // Two short sections per packet, so one stays queued after each poll
    let sections = [
        &[0x00, 0x80, 0x30, 2, 0xAA, 0xAA][..],
        &[0x80, 0x30, 2, 0xBB, 0xBB][..],
    ]
    .concat();
    let mut lengths = Vec::new();
    for continuity_counter in 0..16 {
        demuxer.handle_input(&packet(0x0001, true, continuity_counter, &sections));
        let _ = demuxer.poll_event();
        let _ = demuxer.poll_event();
        lengths.push(demuxer.framer.buffer_len());
    }
    assert!(
        lengths.iter().all(|&length| length <= 2 * 188),
        "{lengths:?}"
    );
}
