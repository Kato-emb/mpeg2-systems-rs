//! Prints every event the Transport Stream demuxer reports for a file, one
//! per line
//!
//! ```text
//! cargo run -p examples --example ts_dump -- crates/mpeg2-systems/tests/fixtures/188.ts
//! ```
//!
//! The PIDs of the elementary streams are learnt from the PMTs the stream
//! carries: each `TsProgramMapSection` event names them, and the demuxer is
//! subscribed to them once that event is dropped, as the event borrows it.

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};

use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};

/// Bytes handed over per read, the Transport Packets of one UDP datagram
const CHUNK_LENGTH: usize = 7 * 188;

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: ts_dump <Transport Stream file>")?;
    let mut reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(path)?);
    let mut demuxer = Demuxer::new(DemuxerConfig::default());

    loop {
        // Why fill_buf rather than read into an array of our own: the demuxer
        // takes a borrowed slice, so the reader's buffer goes in as it is.
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let length = chunk.len();
        demuxer.handle_input(chunk);
        reader.consume(length);
        print_events(&mut demuxer);
    }

    demuxer.finish();
    print_events(&mut demuxer);
    Ok(())
}

/// Prints each event the demuxer has
fn print_events(demuxer: &mut Demuxer) {
    while let Some(event) = demuxer.poll_event() {
        #[expect(clippy::print_stdout, reason = "the events are the output")]
        {
            println!("{event:?}");
        }
        let Event::TsProgramMapSection { section, .. } = event else {
            continue;
        };
        let pids: Vec<Pid> = section
            .elementary_streams
            .iter()
            .flatten()
            .map(|stream| stream.elementary_pid)
            .collect();
        for pid in pids {
            demuxer.subscribe_pes(pid);
        }
    }
}
