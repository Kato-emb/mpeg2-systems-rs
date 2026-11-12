//! Reads a Program Stream and counts the PES packets of each `stream_id`
//!
//! ```text
//! cargo run --example ps_demux -- crates/mpeg2-systems/tests/fixtures/program.ps
//! ```

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

use mpeg2_systems::ps::demux::{Demuxer, Event};

/// Bytes handed over per read, a common size of a pack
const CHUNK_LENGTH: usize = 2048;

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: ps_demux <Program Stream file>")?;
    let reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(path)?);
    demux(reader, io::stdout().lock())
}

/// Reads the Program Stream `reader` holds, and writes into `summary` how
/// many PES packets each `stream_id` has and how many bytes their payloads
/// hold, and what the demuxer could not read
fn demux(mut reader: impl BufRead, mut summary: impl Write) -> Result<(), Box<dyn Error>> {
    // Why no subscribe step, unlike the Transport Stream demuxer: a Program
    // Stream has no PIDs to choose among, and its PES packets are cut whole
    // from the pack.
    let mut demuxer = Demuxer::new();
    let mut streams = BTreeMap::<u8, (usize, usize)>::new();

    loop {
        // Why fill_buf rather than read into an array of our own: the demuxer
        // takes a borrowed slice, so the reader's buffer goes in as it is.
        let chunk = reader.fill_buf()?;
        let length = chunk.len();
        if length == 0 {
            demuxer.finish();
        } else {
            demuxer.handle_input(chunk);
        }
        reader.consume(length);

        while let Some(event) = demuxer.poll_event() {
            match event {
                Event::PesPacket(packet) => {
                    let (packets, bytes) = streams.entry(packet.stream_id.get()).or_default();
                    *packets += 1;
                    *bytes += packet.payload.len();
                }
                Event::Error(error) => writeln!(summary, "error: {error}")?,
                _ => {}
            }
        }

        if length == 0 {
            break;
        }
    }

    for (stream_id, (packets, bytes)) in streams {
        writeln!(
            summary,
            "stream_id {stream_id:#04x}: {packets} PES packets, {bytes} bytes"
        )?;
    }
    Ok(())
}
