//! This crate's reading of the ARIB caption fixture, checked against TSDuck
//! and ffprobe, which read the same bytes independently
//!
//! The tests run `tstables` and `ffprobe` from `PATH`, so they are ignored
//! unless asked for; once asked for, a tool missing is a failure.

#[cfg(test)]
#[cfg(feature = "alloc")]
#[path = "support/caption.rs"]
mod support;

#[cfg(test)]
#[cfg(feature = "alloc")]
#[expect(
    clippy::indexing_slicing,
    reason = "a serde_json Value indexed by a key it lacks is Null, it does not panic"
)]
mod tests {
    use std::process::Command;

    use mpeg2_systems::descriptor::DescriptorLoop;
    use mpeg2_systems::pes::PesPacket;
    use mpeg2_systems_arib::caption::Dmf;
    use mpeg2_systems_arib::caption::data_group::DataGroup;
    use mpeg2_systems_arib::descriptor::data_component::{
        AdditionalAribCaptionInfo, DataComponentDescriptor, Timing,
    };
    use mpeg2_systems_arib::descriptor::stream_identifier::StreamIdentifierDescriptor;
    use mpeg2_systems_arib::independent_pes::SynchronizedPesData;
    use serde_json::Value;

    use super::support;

    /// The PID, `stream_type`, `component_tag`, `data_component_id` and
    /// `additional_data_component_info` of the caption elementary stream
    type CaptionStream = (u64, u64, u64, u64, Vec<u8>);

    fn fixture_path() -> String {
        format!("{}/tests/fixtures/caption.ts", env!("CARGO_MANIFEST_DIR"))
    }

    /// What `tool` prints on its standard output for `arguments`, read as JSON
    fn run(tool: &str, arguments: &[&str]) -> Value {
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
    fn integer(json: &Value, key: &str) -> u64 {
        let value = &json[key];
        value
            .as_u64()
            .or_else(|| {
                let text = value.as_str()?;
                match text.strip_prefix("0x") {
                    Some(hexadecimal) => u64::from_str_radix(hexadecimal, 16).ok(),
                    None => text.parse().ok(),
                }
            })
            .ok_or_else(|| format!("no integer {key} in {json}"))
            .unwrap()
    }

    /// Every object in `json`, at any depth, whose `#name` is `name`
    ///
    /// TSDuck prints an XML element as an object holding its name as `#name`
    /// and its children as `#nodes`.
    fn named<'json>(json: &'json Value, name: &str) -> Vec<&'json Value> {
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

    /// The caption elementary streams this crate reads in the PMT
    fn caption_streams_read() -> Vec<CaptionStream> {
        let (streams, _) = support::read();
        streams
            .iter()
            .map(|stream| {
                let descriptors: Vec<_> = DescriptorLoop::from(stream.es_info.as_slice())
                    .iter()
                    .map(Result::unwrap)
                    .collect();
                let component_tag = descriptors
                    .iter()
                    .find_map(|raw| StreamIdentifierDescriptor::try_from(raw).ok())
                    .unwrap()
                    .component_tag;
                let data_component = descriptors
                    .iter()
                    .find_map(|raw| DataComponentDescriptor::try_from(raw).ok())
                    .unwrap();
                (
                    stream.pid.into(),
                    stream.stream_type.get().into(),
                    component_tag.into(),
                    data_component.data_component_id.get().into(),
                    data_component.additional_data_component_info.to_vec(),
                )
            })
            .collect()
    }

    /// The caption elementary streams `tstables` reads in the PMT
    fn caption_streams_tstables() -> Vec<CaptionStream> {
        let tables = run(
            "tstables",
            &["--json-output", "-", "--pid", "0x1000", &fixture_path()],
        );
        named(&tables, "component")
            .into_iter()
            .map(|component| {
                let stream_identifier = named(component, "stream_identifier_descriptor");
                let data_component = named(component, "data_component_descriptor");
                let [stream_identifier]: &[_; 1] = stream_identifier.as_slice().try_into().unwrap();
                let [data_component]: &[_; 1] = data_component.as_slice().try_into().unwrap();
                let info: String = named(data_component, "additional_data_component_info")
                    .iter()
                    .flat_map(|info| info["#nodes"].as_array().into_iter().flatten())
                    .filter_map(Value::as_str)
                    .flat_map(str::split_whitespace)
                    .collect();
                let info = info
                    .as_bytes()
                    .chunks(2)
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                (
                    integer(component, "elementary_pid"),
                    integer(component, "stream_type"),
                    integer(stream_identifier, "component_tag"),
                    integer(data_component, "data_component_id"),
                    info,
                )
            })
            .collect()
    }

    #[test]
    #[ignore = "runs tstables from TSDuck"]
    fn the_caption_stream_has_the_descriptors_tstables_reads() {
        let streams = caption_streams_read();

        assert_eq!(streams, caption_streams_tstables());
        // Why not compare DMF and Timing with tstables: TSDuck prints the info
        // undivided, so Table 9-15 of ARIB STD-B24 Volume 1 Part 3 divides it.
        let [(.., info)]: &[_; 1] = streams.as_slice().try_into().unwrap();
        assert_eq!(
            AdditionalAribCaptionInfo::read(info),
            Ok(AdditionalAribCaptionInfo::new(
                Dmf::try_from(0b0011).unwrap(),
                Timing::PROGRAM_SYNCHRONOUS
            ))
        );
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_caption_packets_and_subtitles_are_those_ffprobe_reads() {
        let path = fixture_path();
        // Why not one run for the three: with both -show_packets and
        // -show_frames, ffprobe prints them mixed under packets_and_frames.
        let probed = run(
            "ffprobe",
            &[
                "-v",
                "error",
                "-of",
                "json",
                "-show_streams",
                "-show_packets",
                path.as_str(),
            ],
        );
        let frames = run(
            "ffprobe",
            &["-v", "error", "-of", "json", "-show_frames", path.as_str()],
        );
        let (_, read) = support::read();
        let read: Vec<_> = read
            .iter()
            .map(|packet| {
                let pes = PesPacket::new(packet.stream_id, &packet.payload);
                let data = SynchronizedPesData::try_from(&pes).unwrap();
                let group = DataGroup::read(data.synchronized_pes_data_byte).unwrap();
                (packet.pts.unwrap(), group.data_group_id.get())
            })
            .collect();
        let statements = read.iter().filter(|&&(_, id)| id == 0x01).count();

        let streams: Vec<_> = probed["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|stream| {
                (
                    integer(stream, "id"),
                    stream["codec_name"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(streams, [(0x0130, "arib_caption")]);
        let pts: Vec<_> = probed["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|packet| integer(packet, "pts"))
            .collect();
        assert_eq!(pts, read.iter().map(|&(pts, _)| pts).collect::<Vec<_>>());
        let rects: Vec<_> = frames["frames"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|frame| frame["media_type"] == "subtitle")
            .map(|frame| integer(frame, "num_rects"))
            .collect();
        assert_eq!(rects, vec![1; statements]);
    }
}
