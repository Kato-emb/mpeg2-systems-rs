//! Reads a Transport Stream and writes its tables and the PES packets of its
//! programs back out as a Transport Stream of its own
//!
//! ```text
//! cargo run --example ts_remux -- <in.ts> <out.ts>
//! ```
//!
//! The output is written through [`PacketWriter`], which writes each value
//! given as it is, so the input's PCRs and the sections the crate does not
//! interpret are carried over. [`Muxer`](mpeg2_systems::ts::mux::Muxer)
//! instead makes the PSI and the PCRs of its own; the `ps_to_ts` example
//! multiplexes with it.
//!
//! The demuxer subscribes itself to the elementary streams the PMTs name.
//! Each event lends what it reports from the demuxer: the packet writer takes
//! the section or the PES packet while the event is alive, and it is dropped
//! before the next poll.
//!
//! The demuxer reports a table once for each version, so the output carries
//! the PAT and each PMT once; how often to send them again is the caller's
//! choice. Each PCR is written as soon as it is read, in a Transport Packet
//! of its own on its PID, so the output carries the input's PCRs in their
//! order; as the demuxer reports a PES packet once it ends, a PCR the input
//! carried inside a PES packet goes out ahead of that packet. The packet writer
//! writes no null packets, so the bit rate is not kept.

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
use mpeg2_systems::ts::mux::PacketWriter;

/// Bytes handed over per read, the Transport Packets of one UDP datagram
const CHUNK_LENGTH: usize = 7 * 188;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(input), Some(output)) = (arguments.next(), arguments.next()) else {
        return Err("usage: ts_remux <in.ts> <out.ts>".into());
    };
    let reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(input)?);
    let mut writer = BufWriter::new(File::create(output)?);
    remux(reader, &mut writer)?;
    writer.flush()?;
    Ok(())
}

/// Writes the tables, the PCRs and the PES packets of the Transport Stream
/// `reader` holds into `writer` as a Transport Stream
fn remux(mut reader: impl BufRead, mut writer: impl Write) -> Result<(), Box<dyn Error>> {
    let mut demuxer = Demuxer::new(DemuxerConfig::default().with_subscribe_elementary_pids(true));
    let mut packet_writer = PacketWriter::new();
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
                Event::ProgramAssociationSection(section) => {
                    packet_writer.write_program_association_section(&section, &mut out)?;
                }
                Event::TsProgramMapSection { pid, section } => {
                    packet_writer.write_ts_program_map_section(pid, &section, &mut out)?;
                }
                Event::CaSection(section) => {
                    packet_writer.write_ca_section(&section, &mut out)?;
                }
                Event::Section { pid, section } => {
                    packet_writer.write_section(pid, &section, &mut out)?;
                }
                Event::ProgramClockReference {
                    pid,
                    program_clock_reference,
                } => {
                    packet_writer.write_program_clock_reference(
                        pid,
                        program_clock_reference,
                        &mut out,
                    )?;
                }
                Event::PesPacket { pid, packet, .. } => {
                    packet_writer.write_pes_packet(pid, &packet, None, &mut out)?;
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

    writer.write_all(&out)?;
    Ok(())
}
