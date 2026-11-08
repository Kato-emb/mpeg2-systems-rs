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
//! Each event is a numbered heading with its main fields in aligned columns,
//! the entries it lists as a table under it, and a blank line before the
//! next. A run of bytes shows its length and first bytes, so an event stays
//! short however long its PES packet.

#[path = "support.rs"]
mod support;

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};

use mpeg2_systems::psi::pat::Program;
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
use support::{Align, byte_run, descriptor_table, pes_times, print_event, table};

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
        let (kind, on, fields, lines) = layout(&event);
        let pid_column = on.map_or_else(|| " ".repeat(10), |on| format!("pid {:#06x}", on.get()));
        print_event(
            *printed,
            kind,
            KIND_WIDTH,
            &format!("{pid_column}  {fields}"),
            &lines,
        );
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

/// Columns the kind of an event takes, that of the longest kind
const KIND_WIDTH: usize = "UNSYNCHRONIZED".len();

/// The kind of `event`, the PID it is on, the fields of its heading and the
/// lines under it
fn layout(event: &Event<'_>) -> (&'static str, Option<Pid>, String, Vec<String>) {
    match event {
        Event::SyncAcquired { packet_size } => (
            "SYNC",
            None,
            format!("packet_size {}", packet_size.stride()),
            Vec::new(),
        ),
        Event::ProgramClockReference {
            pid,
            program_clock_reference,
        } => (
            "PCR",
            Some(*pid),
            format!(
                "program_clock_reference_base {:>10}  extension {:>3}",
                program_clock_reference.base(),
                program_clock_reference.extension()
            ),
            Vec::new(),
        ),
        Event::ProgramAssociationSection(section) => {
            let mut unread = Vec::new();
            let rows: Vec<Vec<String>> = section
                .programs
                .iter()
                .filter_map(|program| {
                    program
                        .map_err(|error| {
                            unread.push(format!("an entry that does not read: {error}"))
                        })
                        .ok()
                })
                .map(|program| match program {
                    Program::Network { network_pid } => {
                        vec!["0".to_owned(), format!("{:#06x}", network_pid.get())]
                    }
                    Program::ProgramMap {
                        program_number,
                        program_map_pid,
                    } => vec![
                        program_number.to_string(),
                        format!("{:#06x}", program_map_pid.get()),
                    ],
                    other => vec![format!("{other:?}")],
                })
                .collect();
            let mut lines = table(
                &[("program", Align::Right), ("program_map_PID", Align::Left)],
                &rows,
            );
            lines.extend(unread);
            (
                "PAT",
                Some(Pid::PAT),
                format!(
                    "transport_stream_id {:>5}  version {:>2}  section {}/{}",
                    section.transport_stream_id,
                    section.version_number.get(),
                    section.section_number,
                    section.last_section_number
                ),
                lines,
            )
        }
        Event::TsProgramMapSection { pid, section } => {
            let mut lines = descriptor_table(section.program_info);
            let mut unread = Vec::new();
            let rows: Vec<Vec<String>> = section
                .elementary_streams
                .iter()
                .filter_map(|stream| {
                    stream
                        .map_err(|error| {
                            unread.push(format!("an entry that does not read: {error}"))
                        })
                        .ok()
                })
                .map(|stream| {
                    vec![
                        format!("{:#04x}", stream.stream_type.get()),
                        format!("{:#06x}", stream.elementary_pid.get()),
                        stream.es_info.as_bytes().len().to_string(),
                    ]
                })
                .collect();
            lines.extend(table(
                &[
                    ("stream_type", Align::Left),
                    ("elementary_PID", Align::Left),
                    ("ES_info_length", Align::Right),
                ],
                &rows,
            ));
            lines.extend(unread);
            (
                "PMT",
                Some(*pid),
                format!(
                    "program_number {:>5}  version {:>2}  section {}/{}  PCR_PID {}",
                    section.program_number,
                    section.version_number.get(),
                    section.section_number,
                    section.last_section_number,
                    section
                        .pcr_pid
                        .map_or_else(|| "none".to_owned(), |pid| format!("{:#06x}", pid.get()))
                ),
                lines,
            )
        }
        Event::CaSection(section) => (
            "CAT",
            Some(Pid::CAT),
            format!(
                "version {:>2}  section {}/{}",
                section.version_number.get(),
                section.section_number,
                section.last_section_number
            ),
            descriptor_table(section.descriptors),
        ),
        Event::Section { pid, section } => {
            let syntax = section.syntax.map_or_else(String::new, |syntax| {
                format!(
                    "  version {:>2}  section {}/{}",
                    syntax.version_number.get(),
                    syntax.section_number,
                    syntax.last_section_number
                )
            });
            (
                "SECTION",
                Some(*pid),
                format!("table_id {:#04x}{syntax}", section.table_id.get()),
                vec![byte_run("body", section.body)],
            )
        }
        Event::PesPacket {
            pid,
            packet,
            start_offset,
        } => (
            "PES",
            Some(*pid),
            format!(
                "stream_id {:#04x}  offset {start_offset:>12}  {}",
                packet.stream_id.get(),
                pes_times(packet)
            ),
            vec![byte_run("payload", packet.payload)],
        ),
        Event::UnreadablePesPacket {
            pid,
            bytes,
            error,
            start_offset,
        } => (
            "UNREADABLE PES",
            Some(*pid),
            format!("offset {start_offset:>12}  {error}"),
            vec![byte_run("bytes", bytes)],
        ),
        Event::ScrambledPesPacket {
            pid,
            bytes,
            transport_scrambling_control,
            start_offset,
        } => (
            "SCRAMBLED PES",
            Some(*pid),
            format!(
                "offset {start_offset:>12}  transport_scrambling_control {:02b}",
                transport_scrambling_control.get()
            ),
            vec![byte_run("bytes", bytes)],
        ),
        Event::TruncatedPesPacket {
            pid,
            bytes,
            start_offset,
        } => (
            "TRUNCATED PES",
            Some(*pid),
            format!("offset {start_offset:>12}"),
            vec![byte_run("bytes", bytes)],
        ),
        Event::Unsynchronized { bytes } => (
            "UNSYNCHRONIZED",
            None,
            String::new(),
            vec![byte_run("bytes", bytes)],
        ),
        Event::Error(error) => ("ERROR", None, error.to_string(), Vec::new()),
        _ => ("UNKNOWN", None, String::new(), Vec::new()),
    }
}
