//! The Transport Stream demuxer over input of any bytes, cut anywhere, with
//! PES packets reassembled on the PIDs the input names
//!
//! The stream is the bytes after the fields the fuzzer chooses, so a
//! Transport Stream after a few bytes of them reads as one, and the seeds are
//! the fixtures as they lie. One run hands the stream over in chunks of the
//! lengths chosen, in turn, takes every event after each, then ends the input
//! and takes the events it leaves. PES packets are reassembled on the PIDs
//! chosen and, as a caller would, on those each program map names. The run
//! checks that:
//!
//! 1. nothing panics
//! 2. once the end of input is drained, `poll_event` stays at `None`
//! 3. the PES reassembly buffer of a PID never holds more than
//!    `pes_buffer_limit` and one Transport Packet; the demuxer checks this
//!    with a `debug_assert`, which a fuzz build turns on

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};

/// What one run takes from the fuzzer
#[derive(Debug, Arbitrary)]
struct Input<'stream> {
    /// Small, so that the reassembly of a PES packet runs past it
    pes_buffer_limit: u16,
    /// The PIDs to subscribe to, of which the low 13 bits are taken
    subscribed: Vec<u16>,
    /// The lengths the stream is cut into, in turn; 0 takes the rest
    chunk_lengths: Vec<u16>,
    stream: &'stream [u8],
}

fuzz_target!(|input: Input<'_>| {
    let mut config = DemuxerConfig::default();
    config.pes_buffer_limit = usize::from(input.pes_buffer_limit);
    let mut demuxer = Demuxer::new(config);
    for pid in input.subscribed {
        if let Ok(pid) = Pid::try_from(pid & 0x1FFF) {
            demuxer.subscribe_pes(pid);
        }
    }
    let mut lengths = input.chunk_lengths.iter().cycle();
    let mut rest = input.stream;
    while !rest.is_empty() {
        let length = match lengths.next().map(|length| usize::from(*length)) {
            None | Some(0) => rest.len(),
            Some(length) => length.min(rest.len()),
        };
        let (chunk, after) = rest.split_at(length);
        demuxer.handle_input(chunk);
        drain(&mut demuxer);
        rest = after;
    }
    demuxer.finish();
    drain(&mut demuxer);
    assert!(
        demuxer.poll_event().is_none(),
        "an event came after the end of input was drained"
    );
});

/// Takes every event, subscribing to the elementary streams a program map
/// names
fn drain(demuxer: &mut Demuxer) {
    while let Some(event) = demuxer.poll_event() {
        let mut pids = Vec::new();
        if let Event::TsProgramMapSection { section, .. } = event {
            pids.extend(
                section
                    .elementary_streams
                    .iter()
                    .flatten()
                    .map(|stream| stream.elementary_pid),
            );
        }
        for pid in pids {
            demuxer.subscribe_pes(pid);
        }
    }
}
