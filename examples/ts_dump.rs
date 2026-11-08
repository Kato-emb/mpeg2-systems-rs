//! Prints every event the Transport Stream demuxer reports for a file
//!
//! ```text
//! cargo run -p examples --example ts_dump -- crates/mpeg2-systems/tests/fixtures/188.ts
//! ```
//!
//! The PIDs of the elementary streams are learnt from the PMTs the stream
//! carries: each `TsProgramMapSection` event names them, and the demuxer is
//! subscribed to them once that event is dropped, as the event borrows it.
//!
//! Each event is a numbered heading with its main fields, the table entries
//! under it, and a blank line after. A run of bytes shows its length and
//! first bytes, so an event stays short however long its PES packet.

#[path = "support.rs"]
mod support;

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};

use mpeg2_systems::psi::pat::Program;
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
use support::{byte_run, descriptor_lines, pes_times, print_event};

/// Bytes handed over per read, the Transport Packets of one UDP datagram
const CHUNK_LENGTH: usize = 7 * 188;

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: ts_dump <Transport Stream file>")?;
    let mut reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(path)?);
    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    let mut printed = 0;

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
        print_events(&mut demuxer, &mut printed);
    }

    demuxer.finish();
    print_events(&mut demuxer, &mut printed);
    Ok(())
}

/// Prints each event the demuxer has, numbering them on from `printed`
fn print_events(demuxer: &mut Demuxer, printed: &mut usize) {
    while let Some(event) = demuxer.poll_event() {
        let (heading, lines) = layout(&event);
        print_event(*printed, &heading, &lines);
        *printed = printed.saturating_add(1);
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

/// `pid` as four hexadecimal digits: `0x0100`
fn pid(pid: Pid) -> String {
    format!("0x{:04X}", pid.get())
}

/// The heading of `event` and the lines under it
fn layout(event: &Event<'_>) -> (String, Vec<String>) {
    match event {
        Event::SyncAcquired { packet_size } => (
            format!("SYNC  packet_size={}", packet_size.stride()),
            Vec::new(),
        ),
        Event::ProgramClockReference {
            pid: on,
            program_clock_reference,
        } => (
            format!(
                "PCR  PID={}  program_clock_reference_base={}  program_clock_reference_extension={}",
                pid(*on),
                program_clock_reference.base(),
                program_clock_reference.extension()
            ),
            Vec::new(),
        ),
        Event::ProgramAssociationSection(section) => (
            format!(
                "PAT  transport_stream_id=0x{:04X}  version_number={}  section_number={}/{}",
                section.transport_stream_id,
                section.version_number.get(),
                section.section_number,
                section.last_section_number
            ),
            section
                .programs
                .iter()
                .map(|program| match program {
                    Ok(Program::Network { network_pid }) => {
                        format!("program 0 -> network_PID {}", pid(network_pid))
                    }
                    Ok(Program::ProgramMap {
                        program_number,
                        program_map_pid,
                    }) => format!(
                        "program {program_number} -> program_map_PID {}",
                        pid(program_map_pid)
                    ),
                    Ok(other) => format!("{other:?}"),
                    Err(error) => format!("an entry that does not read: {error}"),
                })
                .collect(),
        ),
        Event::TsProgramMapSection { pid: on, section } => {
            let mut lines = descriptor_lines(section.program_info);
            lines.extend(
                section
                    .elementary_streams
                    .iter()
                    .map(|stream| match stream {
                        Ok(stream) => format!(
                            "stream_type=0x{:02X}  elementary_PID={}  ES_info_length={}",
                            stream.stream_type.get(),
                            pid(stream.elementary_pid),
                            stream.es_info.as_bytes().len()
                        ),
                        Err(error) => format!("an entry that does not read: {error}"),
                    }),
            );
            (
                format!(
                    "PMT  PID={}  program_number={}  version_number={}  PCR_PID={}",
                    pid(*on),
                    section.program_number,
                    section.version_number.get(),
                    section.pcr_pid.map_or_else(|| "none".to_owned(), pid)
                ),
                lines,
            )
        }
        Event::CaSection(section) => (
            format!(
                "CAT  version_number={}  section_number={}/{}",
                section.version_number.get(),
                section.section_number,
                section.last_section_number
            ),
            descriptor_lines(section.descriptors),
        ),
        Event::Section { pid: on, section } => {
            let syntax = section.syntax.map_or_else(String::new, |syntax| {
                format!(
                    "  version_number={}  section_number={}/{}",
                    syntax.version_number.get(),
                    syntax.section_number,
                    syntax.last_section_number
                )
            });
            (
                format!(
                    "SECTION  PID={}  table_id=0x{:02X}{syntax}",
                    pid(*on),
                    section.table_id.get()
                ),
                vec![byte_run("body", section.body)],
            )
        }
        Event::PesPacket {
            pid: on,
            packet,
            start_offset,
        } => (
            format!(
                "PES  PID={}  stream_id=0x{:02X}  offset={start_offset}{}",
                pid(*on),
                packet.stream_id.get(),
                pes_times(packet)
            ),
            vec![byte_run("payload", packet.payload)],
        ),
        Event::UnreadablePesPacket {
            pid: on,
            bytes,
            error,
            start_offset,
        } => (
            format!(
                "UNREADABLE PES  PID={}  offset={start_offset}  {error}",
                pid(*on)
            ),
            vec![byte_run("bytes", bytes)],
        ),
        Event::ScrambledPesPacket {
            pid: on,
            bytes,
            transport_scrambling_control,
            start_offset,
        } => (
            format!(
                "SCRAMBLED PES  PID={}  offset={start_offset}  transport_scrambling_control={:02b}",
                pid(*on),
                transport_scrambling_control.get()
            ),
            vec![byte_run("bytes", bytes)],
        ),
        Event::TruncatedPesPacket {
            pid: on,
            bytes,
            start_offset,
        } => (
            format!("TRUNCATED PES  PID={}  offset={start_offset}", pid(*on)),
            vec![byte_run("bytes", bytes)],
        ),
        Event::Unsynchronized { bytes } => {
            ("UNSYNCHRONIZED".to_owned(), vec![byte_run("bytes", bytes)])
        }
        Event::Error(error) => (format!("ERROR  {error}"), Vec::new()),
        _ => (
            "an event of a kind this example does not know".to_owned(),
            Vec::new(),
        ),
    }
}
