//! Reads a Program Stream and multiplexes its PES packets into a Transport
//! Stream of one program
//!
//! ```text
//! cargo run -p examples --example ps_to_ts -- <in.ps> <out.ts>
//! ```
//!
//! Each event lends what it reports from the demuxer: the streams of a
//! system header or a program stream map are copied out of it, the muxer
//! takes the PES packet while the event is alive, and it is dropped before
//! the next poll.
//!
//! The muxer takes its streams before the first PES packet, so they are
//! those of the program stream map ahead of the first PES packet, or, without
//! one, the `stream_id`s the system header bounds. A `stream_type` the map
//! does not give is guessed from the `stream_id`: ISO/IEC 13818-2 video for
//! 0xE0 to 0xEF, and ISO/IEC 13818-3 audio for 0xC0 to 0xDF. Neither a
//! stream with a `stream_id_extension`, nor `padding_stream`, nor a stream
//! the system header bounds by any other `stream_id`, such as all audio
//! streams, is registered; their PES packets are left out, and how many is
//! printed at the end. The streams take PIDs from 0x0100 in the order they
//! are registered, in program 1 with its PMT on PID 0x1000, and the muxer
//! chooses the PCR PID.
//!
//! The PES packets go to the muxer in the order of the packs; they are not
//! sorted into the order of their DTS across the streams.

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use mpeg2_systems::pes::StreamId;
use mpeg2_systems::ps::demux::{Demuxer, Event};
use mpeg2_systems::psi::pmt::ElementaryStream;
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::mux::{Muxer, MuxerConfig};

/// Bytes handed over per read, a common size of a pack
const CHUNK_LENGTH: usize = 2048;
/// `program_number` of the one program
const PROGRAM_NUMBER: u16 = 1;
/// PID of the PMT
const PROGRAM_MAP_PID: u16 = 0x1000;
/// PID of the first stream registered, the next ones following it
const FIRST_ELEMENTARY_PID: u16 = 0x0100;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(input), Some(output)) = (arguments.next(), arguments.next()) else {
        return Err("usage: ps_to_ts <in.ps> <out.ts>".into());
    };
    let reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(input)?);
    let mut writer = BufWriter::new(File::create(output)?);
    let left_out = convert(reader, &mut writer)?;
    writer.flush()?;
    eprintln!("PES packets left out: {left_out}");
    Ok(())
}

/// Writes the Transport Stream of the Program Stream `reader` holds into
/// `writer`, and returns how many PES packets it left out
fn convert(mut reader: impl BufRead, mut writer: impl Write) -> Result<usize, Box<dyn Error>> {
    let mut demuxer = Demuxer::new();
    let mut muxer = Muxer::new(MuxerConfig::new())?;
    let mut mapped: Option<Vec<(StreamId, StreamType)>> = None;
    let mut bounded: Option<Vec<(StreamId, StreamType)>> = None;
    // Why not take an empty `pids` for a program not yet registered: one
    // registered from a map or a header that names no stream is empty too.
    let mut registered = false;
    let mut pids: Vec<(StreamId, Pid)> = Vec::new();
    let mut left_out: usize = 0;
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
            if let Event::ProgramStreamMap(map) = &event
                && !registered
            {
                let mut streams = Vec::new();
                for stream in map.elementary_streams.iter() {
                    let stream = stream?;
                    if stream.elementary_stream_id_extension.is_none()
                        && stream.elementary_stream_id != StreamId::PADDING_STREAM
                    {
                        streams.push((stream.elementary_stream_id, stream.stream_type));
                    }
                }
                mapped = Some(streams);
            } else if let Event::SystemHeader(header) = &event
                && !registered
            {
                let mut streams = Vec::new();
                for bound in header.stream_bounds.iter() {
                    let stream_id = bound?.stream_id;
                    if let Some(stream_type) = guessed_stream_type(stream_id) {
                        streams.push((stream_id, stream_type));
                    }
                }
                bounded = Some(streams);
            } else if let Event::PesPacket(packet) = &event {
                if !registered {
                    registered = true;
                    muxer.add_program(PROGRAM_NUMBER, Pid::try_from(PROGRAM_MAP_PID)?)?;
                    let streams = mapped.take().or_else(|| bounded.take()).unwrap_or_default();
                    for (pid, (stream_id, stream_type)) in (FIRST_ELEMENTARY_PID..).zip(streams) {
                        let pid = Pid::try_from(pid)?;
                        muxer
                            .add_stream(PROGRAM_NUMBER, ElementaryStream::new(stream_type, pid))?;
                        pids.push((stream_id, pid));
                    }
                }
                match pids.iter().find(|(id, _)| *id == packet.stream_id) {
                    Some(&(_, pid)) => {
                        muxer.handle_pes_packet(pid, packet)?;
                        muxer.poll_output(&mut out);
                    }
                    None => left_out = left_out.saturating_add(1),
                }
            }
        }
        if length == 0 {
            break;
        }
        writer.write_all(&out)?;
        out.clear();
    }

    muxer.finish();
    muxer.poll_output(&mut out);
    writer.write_all(&out)?;
    Ok(left_out)
}

/// The `stream_type` of a stream the system header bounds, guessed from its
/// `stream_id`
fn guessed_stream_type(stream_id: StreamId) -> Option<StreamType> {
    if stream_id.is_video() {
        Some(StreamType::ISO_IEC_13818_2_VIDEO)
    } else if stream_id.is_audio() {
        Some(StreamType::ISO_IEC_13818_3_AUDIO)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::process::Command;

    use mpeg2_systems::pes::{PesPacket, StreamId};
    use mpeg2_systems::ps::demux as ps_demux;
    use mpeg2_systems::time::Timestamp;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
    use serde_json::Value;

    use super::convert;

    /// The PTS, the DTS and the payload of a PES packet
    type Read = (Option<u64>, Option<u64>, Vec<u8>);

    /// The Program Stream FFmpeg wrote, among the fixtures of the crate
    const PROGRAM_STREAM: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../crates/mpeg2-systems/tests/fixtures/program.ps"
    );

    fn program_stream() -> Vec<u8> {
        std::fs::read(PROGRAM_STREAM).unwrap()
    }

    /// The Transport Stream of the Program Stream, and how many PES packets
    /// it left out
    fn converted() -> (Vec<u8>, usize) {
        let mut transport_stream = Vec::new();
        let left_out = convert(&program_stream()[..], &mut transport_stream).unwrap();
        (transport_stream, left_out)
    }

    fn read(packet: &PesPacket<'_>) -> Read {
        let header = packet.optional_header.as_ref();
        (
            header.and_then(|header| header.pts).map(Timestamp::get),
            header.and_then(|header| header.dts).map(Timestamp::get),
            packet.payload.to_vec(),
        )
    }

    #[test]
    fn the_transport_stream_carries_the_pes_packets_of_the_program_stream_but_padding() {
        let mut from_program_stream: BTreeMap<u8, Vec<Read>> = BTreeMap::new();
        let mut padding = 0;
        let mut demuxer = ps_demux::Demuxer::new();
        demuxer.handle_input(&program_stream());
        demuxer.finish();
        while let Some(event) = demuxer.poll_event() {
            if let ps_demux::Event::PesPacket(packet) = event {
                if packet.stream_id == StreamId::PADDING_STREAM {
                    padding += 1;
                    continue;
                }
                from_program_stream
                    .entry(packet.stream_id.get())
                    .or_default()
                    .push(read(&packet));
            }
        }

        let (transport_stream, left_out) = converted();
        let mut from_transport_stream: BTreeMap<u8, Vec<Read>> = BTreeMap::new();
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        demuxer.handle_input(&transport_stream);
        demuxer.finish();
        loop {
            // Why not subscribe where the PMT is read: its event borrows the
            // demuxer until it is dropped at the end of the block.
            let pids: Vec<_> = {
                let Some(event) = demuxer.poll_event() else {
                    break;
                };
                if let Event::PesPacket { packet, .. } = &event {
                    from_transport_stream
                        .entry(packet.stream_id.get())
                        .or_default()
                        .push(read(packet));
                }
                let Event::TsProgramMapSection { section, .. } = event else {
                    continue;
                };
                section
                    .elementary_streams
                    .iter()
                    .flatten()
                    .map(|stream| stream.elementary_pid)
                    .collect()
            };
            for pid in pids {
                demuxer.subscribe_pes(pid);
            }
        }

        assert_eq!(
            (from_transport_stream, left_out),
            (from_program_stream, padding)
        );
    }

    /// The `codec_type` and `codec_name` of each stream ffprobe reads in
    /// the file at `path`, and the PTS of each packet that starts a PES
    /// packet
    fn ffprobe(path: &str) -> Vec<(String, String, Vec<Option<i64>>)> {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-fflags",
                "+nofillin",
                "-of",
                "json",
                "-show_streams",
                "-show_packets",
                path,
            ])
            .output()
            .map_err(|error| format!("ffprobe did not run ({error}); put it on PATH"))
            .unwrap();
        assert!(
            output.status.success(),
            "ffprobe {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let probed: Value = serde_json::from_slice(&output.stdout).unwrap();
        let array = |key: &str| probed.get(key).and_then(Value::as_array).unwrap();
        let mut streams: Vec<_> = array("streams")
            .iter()
            .map(|stream| {
                let text = |key: &str| stream.get(key).and_then(Value::as_str).unwrap().to_owned();
                (text("codec_type"), text("codec_name"), Vec::new())
            })
            .collect();
        for packet in array("packets") {
            // Why not count every packet as a PES packet: ffprobe's parser
            // cuts a PES packet into the frames it holds, and only the first
            // frame to start in a PES packet has a `pos`.
            if packet.get("pos").is_none() {
                continue;
            }
            let index = packet.get("stream_index").and_then(Value::as_u64).unwrap();
            let index = usize::try_from(index).unwrap();
            streams
                .get_mut(index)
                .unwrap()
                .2
                .push(packet.get("pts").and_then(Value::as_i64));
        }
        streams
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn ffprobe_reads_the_transport_stream_as_it_reads_the_program_stream() {
        let directory = std::env::temp_dir();
        let path = directory.join("ps_to_ts.ts");
        std::fs::write(&path, converted().0).unwrap();

        assert_eq!(ffprobe(path.to_str().unwrap()), ffprobe(PROGRAM_STREAM));
    }
}
