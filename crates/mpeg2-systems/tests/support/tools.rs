//! What the tests against TSDuck and ffprobe share: running a tool, reading
//! what it prints, and the PES packets FFmpeg wrote into the 188-byte fixture

use std::collections::BTreeMap;
use std::process::Command;

use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
use mpeg2_systems::ps::demux as ps_demux;
use mpeg2_systems::time::Timestamp;
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
use serde_json::Value;

/// The PTS and the DTS of a PES packet, a DTS absent being the PTS, as
/// ffprobe reports it
pub(crate) type Timing = (Option<u64>, Option<u64>);

/// What ffprobe reads: the `id` and `codec_type` of each stream, and the
/// timing of the packets of each stream by `id`
pub(crate) type Probed = (BTreeMap<u64, String>, BTreeMap<u64, Vec<Timing>>);

/// The timing and the payload of a PES packet
pub(crate) type PesPayload = (Timing, Vec<u8>);

/// The PAT: `transport_stream_id`, and the `program_number` and PID of
/// each entry, `program_number` 0 for the network PID
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProgramAssociation {
    pub(crate) transport_stream_id: u64,
    pub(crate) programs: Vec<(u64, u64)>,
}

/// A PMT: `program_number`, `PCR_PID`, and the `stream_type` and
/// `elementary_PID` of each stream
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProgramMap {
    pub(crate) program_number: u64,
    pub(crate) pcr_pid: Option<u64>,
    pub(crate) elementary_streams: Vec<(u64, u64)>,
}

pub(crate) fn fixture_path(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// What `tool` prints on its standard output for `arguments`, read as JSON
pub(crate) fn run(tool: &str, arguments: &[&str]) -> Value {
    let output = Command::new(tool)
        .args(arguments)
        .output()
        .map_err(|error| format!("{tool} did not run ({error}); put it on PATH"))
        .unwrap();
    assert!(
        output.status.success(),
        "{tool} {arguments:?}: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// An integer a tool printed as a number, or as a string in decimal or in
/// hexadecimal after `0x`
pub(crate) fn as_integer(json: &Value) -> Option<u64> {
    let Some(text) = json.as_str() else {
        return json.as_u64();
    };
    match text.strip_prefix("0x") {
        Some(hexadecimal) => u64::from_str_radix(hexadecimal, 16).ok(),
        None => text.parse().ok(),
    }
}

pub(crate) fn integer(json: &Value, key: &str) -> u64 {
    as_integer(&json[key])
        .ok_or_else(|| format!("no integer {key} in {json}"))
        .unwrap()
}

/// Every object in `json`, at any depth, whose `#name` is `name`, in the
/// order the tool printed them
///
/// TSDuck prints an XML element as an object holding its name as `#name`.
pub(crate) fn named<'json>(json: &'json Value, name: &str) -> Vec<&'json Value> {
    let children: Vec<&Value> = match json {
        Value::Array(elements) => elements.iter().collect(),
        Value::Object(members) => members.values().collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Vec::new(),
    };
    (json["#name"] == name)
        .then_some(json)
        .into_iter()
        .chain(children.into_iter().flat_map(|child| named(child, name)))
        .collect()
}

/// Hands `stream` whole to a demuxer subscribed to `pids` and returns
/// what `take` keeps of each event
pub(crate) fn ts_events<T>(
    stream: &[u8],
    pids: &[u64],
    mut take: impl FnMut(Event<'_>) -> Option<T>,
) -> Vec<T> {
    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    for &pid in pids {
        demuxer.subscribe_pes(Pid::try_from(u16::try_from(pid).unwrap()).unwrap());
    }
    demuxer.handle_input(stream);
    demuxer.finish();
    let mut kept = Vec::new();
    while let Some(event) = demuxer.poll_event() {
        kept.extend(take(event));
    }
    kept
}

pub(crate) fn timing(packet: &PesPacket<'_>) -> Timing {
    let header = packet.optional_header.as_ref();
    let pts = header.and_then(|header| header.pts).map(Timestamp::get);
    let dts = header.and_then(|header| header.dts).map(Timestamp::get);
    (pts, dts.or(pts))
}

pub(crate) fn tables_tstables(path: &str) -> (Vec<ProgramAssociation>, Vec<ProgramMap>) {
    let tables = run("tstables", &["--json-output", "-", path]);
    let mut associations: Vec<_> = named(&tables, "PAT")
        .into_iter()
        .map(|pat| {
            let mut programs: Vec<_> = pat
                .get("network_pid")
                .and_then(as_integer)
                .map(|pid| (0, pid))
                .into_iter()
                .collect();
            programs.extend(named(pat, "service").into_iter().map(|service| {
                (
                    integer(service, "service_id"),
                    integer(service, "program_map_pid"),
                )
            }));
            ProgramAssociation {
                transport_stream_id: integer(pat, "transport_stream_id"),
                programs,
            }
        })
        .collect();
    let mut maps: Vec<_> = named(&tables, "PMT")
        .into_iter()
        .map(|pmt| ProgramMap {
            program_number: integer(pmt, "service_id"),
            // Why not take PCR_PID as it is: TSDuck prints the null PID for
            // a program without a PCR, which the crate reads as none.
            pcr_pid: Some(integer(pmt, "pcr_pid")).filter(|&pid| pid != u64::from(Pid::NULL.get())),
            elementary_streams: named(pmt, "component")
                .into_iter()
                .map(|component| {
                    (
                        integer(component, "stream_type"),
                        integer(component, "elementary_pid"),
                    )
                })
                .collect(),
        })
        .collect();
    // Why not compare every table tstables prints: it may print a table
    // each time the stream repeats it, where the demuxer reports a table
    // only when it changes.
    associations.dedup();
    maps.dedup();
    (associations, maps)
}

/// What ffprobe reads from the file at `path`, with or without the
/// packets that do not start a PES packet
///
/// `format` names the demuxer, when not left to ffprobe's probe.
pub(crate) fn ffprobe(path: &str, format: Option<&str>, all_packets: bool) -> Probed {
    // Why not let ffprobe fill in the timestamps (its default): it
    // computes one for every packet its parser cuts out of a PES packet,
    // which would hide which packets carried a PTS of their own.
    let mut arguments = vec!["-v", "error", "-fflags", "+nofillin"];
    if let Some(format) = format {
        arguments.extend(["-f", format]);
    }
    arguments.extend(["-of", "json", "-show_streams", "-show_packets", path]);
    let probed = run("ffprobe", &arguments);
    let streams: Vec<_> = probed["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stream| {
            let codec_type = stream["codec_type"].as_str().unwrap();
            (integer(stream, "id"), codec_type.to_owned())
        })
        .collect();
    let mut packets: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
    for packet in probed["packets"].as_array().unwrap() {
        // Why not count every packet as a PES packet: ffprobe's parser
        // cuts a PES packet into the frames it holds, and only the first
        // frame to start in a PES packet takes its position and its
        // timestamps; the others have no `pos`.
        if !all_packets && packet.get("pos").is_none() {
            continue;
        }
        let index = usize::try_from(integer(packet, "stream_index")).unwrap();
        // Why round the circle: ffprobe counts the time stamps on past the
        // wrap of their 33 bits, and those before a wrap near the start
        // below 0, where the stream carries them as written.
        let time_stamp = |key: &str| {
            packet
                .get(key)
                .and_then(Value::as_i64)
                .map(|value| value.rem_euclid(1 << 33).unsigned_abs())
        };
        let timing = (time_stamp("pts"), time_stamp("dts"));
        packets.entry(streams[index].0).or_default().push(timing);
    }
    (streams.into_iter().collect(), packets)
}

/// The timing and the payload of each PES packet of the video (PID
/// 0x100) and of the audio (PID 0x101) FFmpeg wrote into `188.ts`:
/// MPEG-2 video pictures and MPEG-1 Layer II audio frames, elementary
/// streams ffprobe can parse
pub(crate) fn pes_packets_of_188() -> (Vec<PesPayload>, Vec<PesPayload>) {
    let source = std::fs::read(fixture_path("188.ts")).unwrap();
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for (pid, packet) in ts_events(&source, &[0x100, 0x101], |event| {
        let Event::PesPacket { pid, packet, .. } = event else {
            return None;
        };
        Some((pid.get(), (timing(&packet), packet.payload.to_vec())))
    }) {
        if pid == 0x100 { &mut video } else { &mut audio }.push(packet);
    }
    (video, audio)
}

/// The `stream_id`, the timing and the payload of each PES packet of the
/// MPEG-2 video and the MPEG-1 Layer II audio in `program.ps`, elementary
/// streams ffprobe can parse
pub(crate) fn pes_packets_of_program_stream() -> Vec<(u8, PesPayload)> {
    let source = std::fs::read(fixture_path("program.ps")).unwrap();
    let mut demuxer = ps_demux::Demuxer::new();
    demuxer.handle_input(&source);
    demuxer.finish();
    let mut packets = Vec::new();
    while let Some(event) = demuxer.poll_event() {
        if let ps_demux::Event::PesPacket(packet) = event
            && (packet.stream_id.is_video() || packet.stream_id.is_audio())
        {
            packets.push((
                packet.stream_id.get(),
                (timing(&packet), packet.payload.to_vec()),
            ));
        }
    }
    packets
}

pub(crate) fn pes_packet(stream_id: u8, (pts, dts): Timing, payload: &[u8]) -> PesPacket<'_> {
    let mut header = OptionalHeader::new();
    header.pts = pts.map(|pts| Timestamp::try_from(pts).unwrap());
    header.dts = dts.map(|dts| Timestamp::try_from(dts).unwrap());
    let mut packet = PesPacket::new(StreamId::from(stream_id), payload);
    packet.optional_header = Some(header);
    packet
}
