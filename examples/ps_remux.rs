//! Reads a Program Stream and writes its packs back out as a Program Stream
//! of its own
//!
//! ```text
//! cargo run --example ps_remux -- <in.ps> <out.ps>
//! ```
//!
//! The output is written through [`PackWriter`], which writes each value
//! given as it is, so the input's packs, SCRs and system headers are carried
//! over. [`Muxer`](mpeg2_systems::ps::mux::Muxer) instead makes the packs,
//! the SCRs, the system header and the program stream map of its own; the
//! `ts_to_ps` example multiplexes with it.
//!
//! Each event lends what it reports from the demuxer: the pack writer takes
//! the pack header, the map or the PES packet while the event is alive, and
//! the event is dropped before the next poll.
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
use mpeg2_systems::ps::mux::PackWriter;

/// Bytes handed over per read, a common size of a pack
const CHUNK_LENGTH: usize = 2048;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(input), Some(output)) = (arguments.next(), arguments.next()) else {
        return Err("usage: ps_remux <in.ps> <out.ps>".into());
    };
    let reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(input)?);
    let mut writer = BufWriter::new(File::create(output)?);
    remux(reader, &mut writer)?;
    writer.flush()?;
    Ok(())
}

/// Writes the packs of the Program Stream `reader` holds into `writer` as a
/// Program Stream
fn remux(mut reader: impl BufRead, mut writer: impl Write) -> Result<(), Box<dyn Error>> {
    let mut demuxer = Demuxer::new();
    let pack_writer = PackWriter::new();
    let mut out = Vec::new();

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
                Event::PackHeader(header) => {
                    pack_writer.write_pack_header(&header, &mut out)?;
                }
                Event::SystemHeader(header) => {
                    pack_writer.write_system_header(&header, &mut out)?;
                }
                Event::ProgramStreamMap(map) => {
                    pack_writer.write_program_stream_map(&map, &mut out)?;
                }
                Event::PesPacket(packet) => {
                    pack_writer.write_pes_packets(&packet, &mut out)?;
                }
                _ => {}
            }
        }
        if length == 0 {
            break;
        }
        writer.write_all(&out)?;
        out.clear();
    }

    pack_writer.write_program_end_code(&mut out);
    writer.write_all(&out)?;
    Ok(())
}
