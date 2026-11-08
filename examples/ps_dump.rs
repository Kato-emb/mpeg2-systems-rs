//! Prints every event the Program Stream demuxer reports for a file
//!
//! ```text
//! cargo run -p examples --example ps_dump -- crates/mpeg2-systems/tests/fixtures/program.ps
//! ```
//!
//! Each event is a numbered heading with its main fields in aligned columns,
//! the entries of a system header or a program stream map as a table under
//! it, and a blank line before the next. A run of bytes shows its length and
//! first bytes, so an event stays short however long its PES packet.

#[path = "support.rs"]
mod support;

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};

use mpeg2_systems::ps::demux::{Demuxer, Event};
use support::{Align, byte_run, descriptor_table, pes_times, print_event, table};

/// Bytes handed over per read, a common size of a pack
const CHUNK_LENGTH: usize = 2048;

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: ps_dump <Program Stream file>")?;
    let mut reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(path)?);
    let mut demuxer = Demuxer::new();
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
    // Why no subscribe step, unlike the Transport Stream demuxer: a Program
    // Stream has no PIDs to choose among, and its PES packets are cut whole
    // from the pack.
    while let Some(event) = demuxer.poll_event() {
        let (kind, fields, lines) = layout(&event);
        print_event(*printed, kind, KIND_WIDTH, &fields, &lines);
        *printed = printed.saturating_add(1);
    }
}

/// Columns the kind of an event takes, that of the longest kind
const KIND_WIDTH: usize = "PROGRAM STREAM MAP".len();

/// The kind of `event`, the fields of its heading and the lines under it
fn layout(event: &Event<'_>) -> (&'static str, String, Vec<String>) {
    match event {
        Event::SyncAcquired => ("SYNC", String::new(), Vec::new()),
        Event::PackHeader(header) => (
            "PACK HEADER",
            format!(
                "system_clock_reference_base {:>10}  extension {:>3}  program_mux_rate {:>8}",
                header.system_clock_reference.base(),
                header.system_clock_reference.extension(),
                header.program_mux_rate
            ),
            Vec::new(),
        ),
        Event::SystemHeader(header) => {
            let mut unread = Vec::new();
            let rows: Vec<Vec<String>> = header
                .stream_bounds
                .iter()
                .filter_map(|bound| {
                    bound
                        .map_err(|error| {
                            unread.push(format!("an entry that does not read: {error}"))
                        })
                        .ok()
                })
                .map(|bound| {
                    vec![
                        format!("{:#04x}", bound.stream_id.get()),
                        u8::from(bound.p_std_buffer_bound_scale).to_string(),
                        bound.p_std_buffer_size_bound.to_string(),
                    ]
                })
                .collect();
            let mut lines = table(
                &[
                    ("stream_id", Align::Left),
                    ("P-STD_buffer_bound_scale", Align::Right),
                    ("P-STD_buffer_size_bound", Align::Right),
                ],
                &rows,
            );
            lines.extend(unread);
            (
                "SYSTEM HEADER",
                format!(
                    "rate_bound {:>8}  audio_bound {:>2}  video_bound {:>2}",
                    header.rate_bound, header.audio_bound, header.video_bound
                ),
                lines,
            )
        }
        Event::ProgramStreamMap(map) => {
            let mut lines = descriptor_table(map.program_stream_info);
            let mut unread = Vec::new();
            let rows: Vec<Vec<String>> = map
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
                        format!("{:#04x}", stream.elementary_stream_id.get()),
                        stream.elementary_stream_info.as_bytes().len().to_string(),
                    ]
                })
                .collect();
            lines.extend(table(
                &[
                    ("stream_type", Align::Left),
                    ("elementary_stream_id", Align::Left),
                    ("elementary_stream_info_length", Align::Right),
                ],
                &rows,
            ));
            lines.extend(unread);
            (
                "PROGRAM STREAM MAP",
                format!("version {:>2}", map.program_stream_map_version.get()),
                lines,
            )
        }
        Event::PesPacket(packet) => (
            "PES",
            format!(
                "stream_id {:#04x}  {}",
                packet.stream_id.get(),
                pes_times(packet)
            ),
            vec![byte_run("payload", packet.payload)],
        ),
        Event::ProgramEnd => ("PROGRAM END", String::new(), Vec::new()),
        Event::Unsynchronized { bytes } => (
            "UNSYNCHRONIZED",
            String::new(),
            vec![byte_run("bytes", bytes)],
        ),
        Event::Unframable { bytes } => {
            ("UNFRAMABLE", String::new(), vec![byte_run("bytes", bytes)])
        }
        Event::Truncated { bytes } => ("TRUNCATED", String::new(), vec![byte_run("bytes", bytes)]),
        Event::Error(error) => ("ERROR", error.to_string(), Vec::new()),
        _ => ("UNKNOWN", String::new(), Vec::new()),
    }
}
