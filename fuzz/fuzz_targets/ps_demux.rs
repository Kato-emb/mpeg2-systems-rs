//! The Program Stream demuxer over input of any bytes, cut anywhere
//!
//! The stream is the bytes after the chunk lengths the fuzzer chooses, so a
//! Program Stream after a few bytes of them reads as one, and the seeds are
//! the fixtures as they lie. One run hands the stream over in chunks of the
//! lengths chosen, in turn, takes every event after each, then ends the input
//! and takes the events it leaves. Each PES packet of `stream_id` 0xFF is
//! read as a program stream directory, and its entries are walked. It checks
//! that:
//!
//! 1. nothing panics
//! 2. once the end of input is drained, `poll_event` stays at `None`

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mpeg2_systems::ps::ProgramStreamDirectory;
use mpeg2_systems::ps::demux::{Demuxer, Event};

/// What one run takes from the fuzzer
#[derive(Debug, Arbitrary)]
struct Input<'stream> {
    /// The lengths the stream is cut into, in turn; 0 takes the rest
    chunk_lengths: Vec<u16>,
    stream: &'stream [u8],
}

fuzz_target!(|input: Input<'_>| {
    let mut demuxer = Demuxer::new();
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

/// Takes every event the demuxer has, reading each program stream directory
fn drain(demuxer: &mut Demuxer) {
    while let Some(event) = demuxer.poll_event() {
        if let Event::PesPacket(packet) = event
            && let Ok(directory) = ProgramStreamDirectory::try_from(&packet)
        {
            directory.access_units.iter().for_each(drop);
        }
    }
}
