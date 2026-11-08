//! Reads a Program Stream and writes its packs back out as a Program Stream
//! of its own
//!
//! ```text
//! cargo run -p examples --example ps_remux -- <in.ps> <out.ps>
//! ```
//!
//! Each event lends what it reports from the demuxer: the muxer takes the
//! pack header, the map or the PES packet while the event is alive, and the
//! event is dropped before the next poll.
//!
//! A pack starts where the input starts one. The demuxer reports a system
//! header and a program stream map only when they change, so the output
//! carries each once for each change; how often to send them again is the
//! caller's choice.

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use mpeg2_systems::ps::demux::{Demuxer, Event};
use mpeg2_systems::ps::mux::Muxer;

/// Bytes handed over per read, a common size of a pack
const CHUNK_LENGTH: usize = 2048;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(input), Some(output)) = (arguments.next(), arguments.next()) else {
        return Err("usage: ps_remux <in.ps> <out.ps>".into());
    };
    let mut reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(input)?);
    let mut writer = BufWriter::new(File::create(output)?);
    let mut demuxer = Demuxer::new();
    let muxer = Muxer::new();
    let mut out = Vec::new();

    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let length = chunk.len();
        demuxer.handle_input(chunk);
        reader.consume(length);
        remux_events(&mut demuxer, &muxer, &mut out)?;
        writer.write_all(&out)?;
        out.clear();
    }

    demuxer.finish();
    remux_events(&mut demuxer, &muxer, &mut out)?;
    // Why not write MPEG_program_end_code where the input has one: the
    // output is one Program Stream, ended once after the last pack.
    muxer.write_program_end_code(&mut out);
    writer.write_all(&out)?;
    writer.flush()?;
    Ok(())
}

/// Writes what each event the demuxer has reports into `out`
fn remux_events(
    demuxer: &mut Demuxer,
    muxer: &Muxer,
    out: &mut Vec<u8>,
) -> Result<(), Box<dyn Error>> {
    while let Some(event) = demuxer.poll_event() {
        if let Event::PackHeader(header) = &event {
            muxer.write_pack_header(header, out)?;
        } else if let Event::SystemHeader(header) = &event {
            // Why the system header's own writer: the muxer leaves the system
            // header to the caller.
            header.write(out)?;
        } else if let Event::ProgramStreamMap(map) = &event {
            muxer.write_program_stream_map(map, out)?;
        } else if let Event::PesPacket(packet) = &event {
            muxer.write_pes_packets(packet, out)?;
        }
    }
    Ok(())
}
