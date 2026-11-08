//! Prints every event the Program Stream demuxer reports for a file
//!
//! ```text
//! cargo run -p examples --example ps_dump -- crates/mpeg2-systems/tests/fixtures/program.ps
//! ```
//!
//! Each event is a numbered heading with its main fields, the entries of a
//! system header or a program stream map under it, and a blank line after. A
//! run of bytes shows its length and first bytes, so an event stays short
//! however long its PES packet.

#[path = "support.rs"]
mod support;

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};

use mpeg2_systems::ps::demux::{Demuxer, Event};
use support::{byte_run, descriptor_lines, pes_times, print_event};

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
        let (heading, lines) = layout(&event);
        print_event(*printed, &heading, &lines);
        *printed = printed.saturating_add(1);
    }
}

/// The heading of `event` and the lines under it
fn layout(event: &Event<'_>) -> (String, Vec<String>) {
    match event {
        Event::SyncAcquired => ("SYNC".to_owned(), Vec::new()),
        Event::PackHeader(header) => (
            "PACK HEADER".to_owned(),
            vec![
                format!(
                    "system_clock_reference_base={}  system_clock_reference_extension={}",
                    header.system_clock_reference.base(),
                    header.system_clock_reference.extension()
                ),
                format!("program_mux_rate={}", header.program_mux_rate),
            ],
        ),
        Event::SystemHeader(header) => (
            format!(
                "SYSTEM HEADER  rate_bound={}  audio_bound={}  video_bound={}",
                header.rate_bound, header.audio_bound, header.video_bound
            ),
            header
                .stream_bounds
                .iter()
                .map(|bound| match bound {
                    Ok(bound) => format!(
                        "stream_id=0x{:02X}  P-STD_buffer_bound_scale={}  P-STD_buffer_size_bound={}",
                        bound.stream_id.get(),
                        u8::from(bound.p_std_buffer_bound_scale),
                        bound.p_std_buffer_size_bound
                    ),
                    Err(error) => format!("an entry that does not read: {error}"),
                })
                .collect(),
        ),
        Event::ProgramStreamMap(map) => {
            let mut lines = descriptor_lines(map.program_stream_info);
            lines.extend(map.elementary_streams.iter().map(|stream| match stream {
                Ok(stream) => format!(
                    "stream_type=0x{:02X}  elementary_stream_id=0x{:02X}  elementary_stream_info_length={}",
                    stream.stream_type.get(),
                    stream.elementary_stream_id.get(),
                    stream.elementary_stream_info.as_bytes().len()
                ),
                Err(error) => format!("an entry that does not read: {error}"),
            }));
            (
                format!(
                    "PROGRAM STREAM MAP  program_stream_map_version={}",
                    map.program_stream_map_version.get()
                ),
                lines,
            )
        }
        Event::PesPacket(packet) => (
            format!(
                "PES  stream_id=0x{:02X}{}",
                packet.stream_id.get(),
                pes_times(packet)
            ),
            vec![byte_run("payload", packet.payload)],
        ),
        Event::ProgramEnd => ("PROGRAM END".to_owned(), Vec::new()),
        Event::Unsynchronized { bytes } => {
            ("UNSYNCHRONIZED".to_owned(), vec![byte_run("bytes", bytes)])
        }
        Event::Unframable { bytes } => ("UNFRAMABLE".to_owned(), vec![byte_run("bytes", bytes)]),
        Event::Truncated { bytes } => ("TRUNCATED".to_owned(), vec![byte_run("bytes", bytes)]),
        Event::Error(error) => (format!("ERROR  {error}"), Vec::new()),
        _ => (
            "an event of a kind this example does not know".to_owned(),
            Vec::new(),
        ),
    }
}
