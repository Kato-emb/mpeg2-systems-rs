//! Prints every event the Program Stream demuxer reports for a file, one per
//! line
//!
//! ```text
//! cargo run -p examples --example ps_dump -- crates/mpeg2-systems/tests/fixtures/program.ps
//! ```

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};

use mpeg2_systems::ps::demux::Demuxer;

/// Bytes handed over per read, a common size of a pack
const CHUNK_LENGTH: usize = 2048;

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: ps_dump <Program Stream file>")?;
    let mut reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(path)?);
    let mut demuxer = Demuxer::new();

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
    // Why no subscribe step, unlike the Transport Stream demuxer: a Program
    // Stream has no PIDs to choose among, and its PES packets are cut whole
    // from the pack.
    while let Some(event) = demuxer.poll_event() {
        #[expect(clippy::print_stdout, reason = "the events are the output")]
        {
            println!("{event:?}");
        }
    }
}
