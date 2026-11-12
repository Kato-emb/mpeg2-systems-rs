//! Reads a Transport Stream and counts the PES packets of each elementary
//! stream its PMTs name
//!
//! ```text
//! cargo run --example ts_demux -- crates/mpeg2-systems/tests/fixtures/188.ts
//! ```
//!
//! The demuxer subscribes itself to the elementary streams the PMTs name, so
//! the PES packets of each come as events.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};

/// Bytes handed over per read, the Transport Packets of one UDP datagram
const CHUNK_LENGTH: usize = 7 * 188;

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: ts_demux <Transport Stream file>")?;
    let reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(path)?);
    demux(reader, io::stdout().lock())
}

/// Reads the Transport Stream `reader` holds, and writes into `summary` the
/// `stream_type` of each elementary stream, how many PES packets it has and
/// how many bytes their payloads hold, and what the demuxer could not read
fn demux(mut reader: impl BufRead, mut summary: impl Write) -> Result<(), Box<dyn Error>> {
    let mut demuxer = Demuxer::new(DemuxerConfig::default().with_subscribe_elementary_pids(true));
    let mut streams = BTreeMap::<u16, (u8, usize, usize)>::new();

    loop {
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
                Event::TsProgramMapSection { section, .. } => {
                    for stream in section.elementary_streams.iter() {
                        let stream = stream?;
                        streams.entry(stream.elementary_pid.get()).or_default().0 =
                            stream.stream_type.get();
                    }
                }
                Event::PesPacket { pid, packet, .. } => {
                    let (_, packets, bytes) = streams.entry(pid.get()).or_default();
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

    for (pid, (stream_type, packets, bytes)) in streams {
        writeln!(
            summary,
            "PID {pid:#06x} stream_type {stream_type:#04x}: {packets} PES packets, {bytes} bytes"
        )?;
    }
    Ok(())
}
