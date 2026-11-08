//! Reads a Transport Stream and writes its tables and the PES packets of its
//! programs back out as a Transport Stream of its own
//!
//! ```text
//! cargo run -p examples --example ts_remux -- <in.ts> <out.ts>
//! ```
//!
//! Each event lends what it reports from the demuxer: the muxer takes the
//! section or the PES packet while the event is alive, the PIDs to subscribe
//! to are copied out of it, and it is dropped before the next poll.
//!
//! The demuxer reports a table once for each version, so the output carries
//! the PAT and each PMT once; how often to send them again is the caller's
//! choice. Each PCR is written as soon as it is read, in a Transport Packet
//! of its own on its PID, so the output carries the input's PCRs in their
//! order; as the demuxer reports a PES packet once it ends, a PCR the input
//! carried inside a PES packet goes out ahead of that packet. The muxer
//! writes no null packets, so the bit rate is not kept.

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
use mpeg2_systems::ts::mux::Muxer;

/// Bytes handed over per read, the Transport Packets of one UDP datagram
const CHUNK_LENGTH: usize = 7 * 188;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(input), Some(output)) = (arguments.next(), arguments.next()) else {
        return Err("usage: ts_remux <in.ts> <out.ts>".into());
    };
    let mut reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(input)?);
    let mut writer = BufWriter::new(File::create(output)?);
    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    let mut muxer = Muxer::new();
    let mut out = Vec::new();

    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let length = chunk.len();
        demuxer.handle_input(chunk);
        reader.consume(length);
        remux_events(&mut demuxer, &mut muxer, &mut out)?;
        // Why clear rather than collect the whole output: the buffer keeps
        // its capacity, so the muxer writes each chunk without allocating.
        writer.write_all(&out)?;
        out.clear();
    }

    demuxer.finish();
    remux_events(&mut demuxer, &mut muxer, &mut out)?;
    writer.write_all(&out)?;
    writer.flush()?;
    Ok(())
}

/// Writes what each event the demuxer has reports into `out`
fn remux_events(
    demuxer: &mut Demuxer,
    muxer: &mut Muxer,
    out: &mut Vec<u8>,
) -> Result<(), Box<dyn Error>> {
    while let Some(event) = demuxer.poll_event() {
        if let Event::ProgramAssociationSection(section) = &event {
            muxer.write_program_association_section(section, out)?;
        } else if let Event::CaSection(section) = &event {
            muxer.write_ca_section(section, out)?;
        } else if let Event::Section { pid, section } = &event {
            muxer.write_section(*pid, section, out)?;
        } else if let Event::ProgramClockReference {
            pid,
            program_clock_reference,
        } = &event
        {
            muxer.write_program_clock_reference(*pid, *program_clock_reference, out)?;
        } else if let Event::PesPacket { pid, packet, .. } = &event {
            muxer.write_pes_packet(*pid, packet, None, out)?;
        }
        let Event::TsProgramMapSection { pid, section } = event else {
            continue;
        };
        muxer.write_ts_program_map_section(pid, &section, out)?;
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
    Ok(())
}
