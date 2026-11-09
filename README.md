# mpeg2-systems-rs

[![CI](https://github.com/Kato-emb/mpeg2-systems-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/Kato-emb/mpeg2-systems-rs/actions/workflows/ci.yml)

Sans-IO reading and writing of MPEG-2 Systems streams (ISO/IEC 13818-1) in
Rust: the Transport Stream and the Program Stream.

> **Status:** the layers of ISO/IEC 13818-1 are in place, and so are the
> ARIB STD-B10 descriptors and STD-B24 captions of `mpeg2-systems-arib`.
> Neither crate is published to crates.io.

## Crates

| Crate | Holds |
|---|---|
| `mpeg2-systems` | the syntax layer, which reads and writes one syntax element from the bytes that hold it: `ts` (the Transport Packet and its adaptation field, §2.4.3), `pes` (the PES packet, §2.4.3.6), `psi` (the section and the PAT, PMT and CAT, §2.4.4), `descriptor` (§2.6) and `ps` (the pack header, system header and program stream map, §2.5.3); `ts::demux` and `ps::demux`, which read a stream; and `ts::mux` and `ps::mux`, which write one. Each layer is usable alone |
| `mpeg2-systems-arib` | the ARIB extensions, on the public API of `mpeg2-systems` alone: `descriptor` (the stream identifier and data component descriptors, 0x52 and 0xFD, ARIB STD-B10 §6.2.16 and §6.2.20), `independent_pes` (the synchronized and asynchronous PES data, STD-B24 Volume 3 Chapter 5) and `caption` (the data group, the caption management and caption statement data and their data units, STD-B24 Volume 1 Part 3 Chapter 9) |

## Driving a demuxer

No type does I/O: the caller reads the stream however it can and hands the
bytes to a demuxer in pieces of any length. Handing input over never fails;
what the demuxer cannot read is reported in an event. After each piece the
caller takes the events until there are none, and at the end of the input
calls `finish` and takes the events left. An event borrows the bytes it
reports from the demuxer, so the caller copies out what it needs, such as the
PIDs of a PMT to `subscribe_pes` to, before it polls again.

## Reading a Transport Stream

This one subscribes to the elementary streams each PMT names and counts the
PES packets. The examples run as the doctests of the crate, from
`crates/mpeg2-systems`, where the fixtures they read are.

```rust
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};

let stream = std::fs::read("tests/fixtures/188.ts")?;
let mut demuxer = Demuxer::new(DemuxerConfig::default());
let mut pes_packets = 0;
// Hand over the input in pieces of any length, taking the events after each
for chunk in stream.chunks(7 * 188) {
    demuxer.handle_input(chunk);
    while let Some(event) = demuxer.poll_event() {
        if let Event::PesPacket { .. } = event {
            pes_packets += 1;
        }
        // Copy the PIDs out of a PMT, as the event borrows the demuxer
        let Event::TsProgramMapSection { section, .. } = event else {
            continue;
        };
        let pids: Vec<_> = section
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
// End the input and take the events it leaves
demuxer.finish();
while let Some(event) = demuxer.poll_event() {
    if let Event::PesPacket { .. } = event {
        pes_packets += 1;
    }
}
assert_eq!(pes_packets, 5);
```

## Reading a Program Stream

```rust
use mpeg2_systems::ps::demux::{Demuxer, Event};

let stream = std::fs::read("tests/fixtures/program.ps")?;
let mut demuxer = Demuxer::new();
let (mut packs, mut pes_packets) = (0, 0);
let mut take = |event: Event<'_>| match event {
    Event::PackHeader(_) => packs += 1,
    Event::PesPacket(_) => pes_packets += 1,
    _ => {}
};
for chunk in stream.chunks(2048) {
    demuxer.handle_input(chunk);
    while let Some(event) = demuxer.poll_event() {
        take(event);
    }
}
demuxer.finish();
while let Some(event) = demuxer.poll_event() {
    take(event);
}
assert_eq!((packs, pes_packets), (5, 7));
```

## Multiplexing PES packets

The multiplexing layer takes the PES packets of the streams registered with
it and builds the stream around them: `ts::mux::Muxer` sends the PAT, the PMT
and the PCRs, and `ps::mux::Muxer` writes the packs, their SCRs, the system
header and the program stream map. The caller registers the streams before
the first PES packet, gives the PES packets in the order of their DTS across
the streams, and takes the output into a buffer it owns; at the end it calls
`finish` and takes what is left. Both `Muxer`s are driven the same way. This
one multiplexes an access unit into a Transport Stream and reads it back.

```rust
use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::time::Timestamp;
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
use mpeg2_systems::ts::mux::{Muxer, MuxerConfig};

let (pmt_pid, video_pid) = (Pid::try_from(0x1000)?, Pid::try_from(0x0100)?);
let mut muxer = Muxer::new(MuxerConfig::new())?;
let mut stream = Vec::new();
// Register the program and its stream before the first PES packet
muxer.add_program(1, pmt_pid, None, &[])?;
muxer.add_stream(1, video_pid, StreamType::ISO_IEC_13818_2_VIDEO, &[])?;
// Give an access unit in a PES packet, and take the Transport Packets
let access_unit = vec![0xaa; 1_000];
let mut header = OptionalHeader::new();
header.pts = Some(Timestamp::try_from(90_000)?);
let mut packet = PesPacket::new(StreamId::from(0xe0), &access_unit);
packet.optional_header = Some(header);
muxer.handle_pes_packet(video_pid, &packet)?;
muxer.poll_output(&mut stream);
// End the stream, and take what the muxer still holds
muxer.finish();
muxer.poll_output(&mut stream);

// Read back, subscribing to the PIDs the PMT names
let mut demuxer = Demuxer::new(DemuxerConfig::default());
demuxer.handle_input(&stream);
demuxer.finish();
let (mut pids, mut payloads) = (Vec::new(), Vec::new());
while let Some(event) = demuxer.poll_event() {
    if let Event::TsProgramMapSection { section, .. } = &event {
        pids.extend(section.elementary_streams.iter().flatten().map(|stream| stream.elementary_pid));
    } else if let Event::PesPacket { packet, .. } = &event {
        payloads.push(packet.payload.to_vec());
    }
    drop(event);
    for pid in pids.drain(..) {
        demuxer.subscribe_pes(pid);
    }
}
assert_eq!(payloads, [access_unit]);
```

## Writing element by element

The packetizing layer writes each element it is given, in the order given,
into a buffer the caller owns: `ts::mux::PacketWriter` cuts a PES packet or a
section into the Transport Packets of a PID, or writes a PCR on its own, and
`ps::mux::PackWriter` writes pack headers, system headers, program stream
maps and PES packets. When to send the PSI and the PCRs, and where a pack
starts, are up to the caller. This one writes a PAT, a PMT, a PCR on its own
and an access unit, and reads the stream back.

```rust
use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
use mpeg2_systems::psi::VersionNumber;
use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
use mpeg2_systems::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::time::{ClockReference, Timestamp};
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
use mpeg2_systems::ts::mux::PacketWriter;

let (pmt_pid, video_pid) = (Pid::try_from(0x1000)?, Pid::try_from(0x0100)?);
let mut writer = PacketWriter::new();
let mut stream = Vec::new();
// The PAT and the PMT, built with their own types
let mut programs = Vec::new();
ProgramLoop::write_from(
    [Program::ProgramMap { program_number: 1.try_into()?, program_map_pid: pmt_pid }],
    &mut programs,
)?;
let mut pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0)?);
pat.programs = ProgramLoop::from(&programs[..]);
writer.write_program_association_section(&pat, &mut stream)?;
let mut streams = Vec::new();
ElementaryStreamLoop::write_from(
    [ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video_pid)],
    &mut streams,
)?;
let mut pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0)?);
pmt.pcr_pid = Some(video_pid);
pmt.elementary_streams = ElementaryStreamLoop::from(&streams[..]);
writer.write_ts_program_map_section(pmt_pid, &pmt, &mut stream)?;
// A PCR on its own, then an access unit in a PES packet
writer.write_program_clock_reference(video_pid, ClockReference::new(81_000, 0)?, &mut stream)?;
let access_unit = vec![0xaa; 1_000];
let mut header = OptionalHeader::new();
header.pts = Some(Timestamp::try_from(90_000)?);
let mut packet = PesPacket::new(StreamId::from(0xe0), &access_unit);
packet.optional_header = Some(header);
writer.write_pes_packet(video_pid, &packet, None, &mut stream)?;

// Read back, the access unit is the payload of the one PES packet
let mut demuxer = Demuxer::new(DemuxerConfig::default());
demuxer.subscribe_pes(video_pid);
demuxer.handle_input(&stream);
demuxer.finish();
let mut payloads = Vec::new();
while let Some(event) = demuxer.poll_event() {
    if let Event::PesPacket { packet, .. } = event {
        payloads.push(packet.payload.to_vec());
    }
}
assert_eq!(payloads, [access_unit]);
```

## Reading with the syntax layer alone

The syntax layer reads without an allocator. This one finds the PAT in a
stream and reads it out of its Transport Packet.

```rust
use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection};
use mpeg2_systems::psi::section::{self, Section};
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::packet::TransportPacket;

let stream = std::fs::read("tests/fixtures/188.ts")?;
// The first Transport Packet that starts a section on the PID of the PAT
let packet = stream
    .chunks(188)
    .filter_map(|bytes| TransportPacket::read(bytes).ok())
    .find(|packet| packet.pid == Pid::PAT && packet.payload_unit_start_indicator)
    .ok_or("no PAT")?;
// Skip the pointer_field and the bytes it counts
let (&pointer_field, rest) = packet.payload.and_then(<[u8]>::split_first).ok_or("no payload")?;
let bytes = rest.get(usize::from(pointer_field)..).ok_or("pointer_field past the payload")?;
// Cut the section to its length, read from its first 3 bytes
let length = section::encoded_len(bytes).ok_or("section cut short")?;
let section = Section::read(bytes.get(..length).ok_or("section cut short")?)?;
// Read the PAT from the section
let pat = ProgramAssociationSection::try_from(&section)?;
assert_eq!(pat.transport_stream_id, 1);
for program in pat.programs.iter() {
    if let Program::ProgramMap { program_number, program_map_pid } = program? {
        assert_eq!((program_number.get(), program_map_pid), (1, Pid::try_from(0x1000)?));
    }
}
```

## Reading ARIB captions

This one finds the caption elementary stream in the PMT, reads each of its
PES packets down to its data units, and collects the characters of the
caption statements; decoding the 8-unit code is left to the caller. It runs
as a doctest of `mpeg2-systems-arib`, from `crates/mpeg2-systems-arib`, where
its fixture is.

```rust
use mpeg2_systems::descriptor::Error;
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
use mpeg2_systems_arib::caption::Dmf;
use mpeg2_systems_arib::caption::data_group::DataGroup;
use mpeg2_systems_arib::caption::data_unit::DataUnitParameter;
use mpeg2_systems_arib::caption::management::CaptionManagementData;
use mpeg2_systems_arib::caption::statement::CaptionData;
use mpeg2_systems_arib::descriptor::data_component::{
    AdditionalAribCaptionInfo, DataComponentDescriptor, DataComponentId, Timing,
};
use mpeg2_systems_arib::independent_pes::SynchronizedPesData;

let stream = std::fs::read("tests/fixtures/caption.ts")?;
let mut demuxer = Demuxer::new(DemuxerConfig::default());
let (mut captions, mut languages, mut texts) = (Vec::new(), Vec::new(), Vec::new());
for packet in stream.chunks(188) {
    demuxer.handle_input(packet);
    let mut subscribe = Vec::new();
    while let Some(event) = demuxer.poll_event() {
        // A caption stream: PES private data whose data component is captions
        if let Event::TsProgramMapSection { section, .. } = &event {
            for stream in section.elementary_streams.iter() {
                let stream = stream?;
                if stream.stream_type != StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA {
                    continue;
                }
                for raw in stream.es_info.iter() {
                    let descriptor = match DataComponentDescriptor::try_from(&raw?) {
                        Ok(descriptor) => descriptor,
                        Err(Error::DescriptorTag { .. }) => continue,
                        Err(error) => return Err(error.into()),
                    };
                    if descriptor.data_component_id == DataComponentId::CAPTION_AND_SUPERIMPOSE {
                        let info = AdditionalAribCaptionInfo::read(descriptor.additional_data_component_info)?;
                        captions.push((stream.elementary_pid.get(), info));
                        subscribe.push(stream.elementary_pid);
                    }
                }
            }
        }
        // Its PES packets: a data group each, of management or of a statement
        if let Event::PesPacket { packet, .. } = &event {
            let data = SynchronizedPesData::try_from(packet)?;
            let group = DataGroup::read(data.synchronized_pes_data_byte)?;
            if matches!(group.data_group_id.get(), 0x00 | 0x20) {
                for language in CaptionManagementData::try_from(&group)?.languages.iter() {
                    languages.push(language?.iso_639_language_code);
                }
            } else {
                for unit in CaptionData::try_from(&group)?.data_units.iter() {
                    let unit = unit?;
                    // The 8-unit code characters, which the caller decodes
                    if unit.data_unit_parameter == DataUnitParameter::STATEMENT_BODY {
                        texts.push(unit.data_unit_data_byte.to_vec());
                    }
                }
            }
        }
    }
    // Subscribe once the events, which borrow the demuxer, are taken
    for pid in subscribe {
        demuxer.subscribe_pes(pid);
    }
}
demuxer.finish();
while demuxer.poll_event().is_some() {}
let info = AdditionalAribCaptionInfo::new(Dmf::try_from(0b0011)?, Timing::PROGRAM_SYNCHRONOUS);
assert_eq!(captions, [(0x0130, info)]);
assert_eq!(languages, [*b"jpn"]);
assert_eq!(texts, [[0x24, 0x22], [0x24, 0x24]]);
```

## What is covered

| | Read | Write |
|---|---|---|
| Transport Packet, adaptation field (§2.4.3.2–§2.4.3.5) | yes | yes |
| PES packet (§2.4.3.6) | yes | yes |
| Section, PAT, PMT, CAT (§2.4.4) | yes | yes |
| Descriptors 0x02–0x12, AVC video, MPEG-2 AAC audio, HEVC video (§2.6) | yes | yes |
| Pack header, system header, program stream map (§2.5.3) | yes | yes |
| Transport Stream: framing in 188, 192 and 204 bytes, PSI, PES reassembly, PCR | `ts::demux` | `ts::mux`: `Muxer` (one program at a variable bit rate, the PAT, the PMT and the PCRs sent at intervals) and `PacketWriter` (PES packets, sections, PCRs on their own) |
| Program Stream: packs, system headers, maps, PES packets | `ps::demux` | `ps::mux`: `Muxer` (packs, their SCRs, the system header and the program stream map at the start) and `PackWriter` (pack headers, system headers, maps, PES packets split to fit) |

`mpeg2-systems-arib` reads and writes:

| | Read | Write |
|---|---|---|
| Stream identifier and data component descriptors, `additional_arib_caption_info` (STD-B10 §6.2.16, §6.2.20; STD-B24 Volume 1 Part 3 §9.6.1) | yes | yes |
| Synchronized and asynchronous PES data (STD-B24 Volume 3 Chapter 5) | yes | yes |
| Data group, caption management data, caption statement data, data units (STD-B24 Volume 1 Part 3 §9.2–§9.4) | yes | yes |

Out of scope: null packets for a constant bit rate; more than one program in
a Transport Stream; checking the T-STD or the P-STD, whose buffer bounds the
Program Stream `Muxer` takes from the caller; sending the system header and
the program stream map again; writing padding, pack stuffing or a program
stream directory; pacing the output in real time; subscribing to the
sections of a PID other than those of the PSI; and, in
`mpeg2-systems-arib`, the SI tables of ARIB STD-B10, decoding the 8-unit code
of ARIB STD-B24, and checking the operational rules of ARIB TR-B14 and
TR-B15.

## `no_std` and features

The crate is `no_std`. The `alloc` feature, on by default, brings the
demuxers, the multiplexing and packetizing layers and the writers of the
syntax types; without it the syntax layer reads with no allocator.
`mpeg2-systems-arib` is `no_std` too, and its `alloc` feature, on by
default, brings its writers and the `alloc` feature of `mpeg2-systems`.

## Examples and tests

- `cargo run -p examples --example ts_dump -- <file>` prints every event of a
  Transport Stream; `ps_dump` does the same for a Program Stream.
- `cargo run -p examples --example ts_remux -- <in> <out>` reads a Transport
  Stream and writes it back through the packetizing layer (`PacketWriter`);
  `ps_remux` does the same for a Program Stream (`PackWriter`).
- `cargo run -p examples --example ps_to_ts -- <in> <out>` reads a Program
  Stream and multiplexes its PES packets into a Transport Stream through the
  multiplexing layer (`Muxer`).
- Fixtures: streams FFmpeg wrote, under `crates/mpeg2-systems/tests/fixtures/`,
  and an ARIB caption stream of TSDuck's tables and hand-built PES packets,
  under `crates/mpeg2-systems-arib/tests/fixtures/`.
- Conformance: ignored tests compare this crate's reading, and what its
  multiplexing and packetizing layers write, with TSDuck's and ffprobe's.
- Benchmarks: the per-packet paths, in `benches/`.
- Fuzz: a target for each demuxer, in `fuzz/`.

[CONTRIBUTING.md](CONTRIBUTING.md) says how to run each.

## Minimum supported Rust version

`rust-version` in [Cargo.toml](Cargo.toml); [CONTRIBUTING.md](CONTRIBUTING.md#minimum-supported-rust-version)
states how it is raised.

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
