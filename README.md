# mpeg2-systems-rs

[![CI](https://github.com/Kato-emb/mpeg2-systems-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/Kato-emb/mpeg2-systems-rs/actions/workflows/ci.yml)

Sans-IO reading and writing of MPEG-2 Systems streams (ISO/IEC 13818-1) in
Rust: the Transport Stream and the Program Stream.

> **Status:** the layers of ISO/IEC 13818-1 are in place. Nothing is
> published to crates.io. A crate for the extensions of ARIB STD-B24 and
> STD-B10 is in progress.

## Crates

| Crate | Holds |
|---|---|
| `mpeg2-systems` | the syntax layer, which reads and writes one syntax element from the bytes that hold it: `ts` (the Transport Packet and its adaptation field, §2.4.3), `pes` (the PES packet, §2.4.3.6), `psi` (the section and the PAT, PMT and CAT, §2.4.4), `descriptor` (§2.6) and `ps` (the pack header, system header and program stream map, §2.5.3); `ts::demux` and `ps::demux`, which read a stream; and `ts::mux` and `ps::mux`, which write one. Each layer is usable alone |
| `mpeg2-systems-arib` | planned: the extensions of ARIB STD-B24 and STD-B10 |

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

## Writing a Transport Stream

A muxer appends to a buffer the caller owns. This one writes a PAT, a PMT, a
PCR on its own and an access unit, and reads the stream back.

```rust
use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
use mpeg2_systems::psi::VersionNumber;
use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
use mpeg2_systems::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::time::{ClockReference, Timestamp};
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
use mpeg2_systems::ts::mux::Muxer;

let (pmt_pid, video_pid) = (Pid::try_from(0x1000)?, Pid::try_from(0x0100)?);
let mut muxer = Muxer::new();
let mut stream = Vec::new();
// The PAT and the PMT, built with their own types
let mut programs = Vec::new();
ProgramLoop::write_from(
    [Program::ProgramMap { program_number: 1.try_into()?, program_map_pid: pmt_pid }],
    &mut programs,
)?;
let mut pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0)?);
pat.programs = ProgramLoop::from(&programs[..]);
muxer.write_program_association_section(&pat, &mut stream)?;
let mut streams = Vec::new();
ElementaryStreamLoop::write_from(
    [ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video_pid)],
    &mut streams,
)?;
let mut pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0)?);
pmt.pcr_pid = Some(video_pid);
pmt.elementary_streams = ElementaryStreamLoop::from(&streams[..]);
muxer.write_ts_program_map_section(pmt_pid, &pmt, &mut stream)?;
// A PCR on its own, then an access unit in a PES packet
muxer.write_program_clock_reference(video_pid, ClockReference::new(81_000, 0)?, &mut stream)?;
let access_unit = vec![0xaa; 1_000];
let mut header = OptionalHeader::new();
header.pts = Some(Timestamp::try_from(90_000)?);
let mut packet = PesPacket::new(StreamId::from(0xe0), &access_unit);
packet.optional_header = Some(header);
muxer.write_pes_packet(video_pid, &packet, None, &mut stream)?;

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
use mpeg2_systems::psi::section::{Section, section_length};
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
let length = section_length(bytes).ok_or("section cut short")?;
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

## What is covered

| | Read | Write |
|---|---|---|
| Transport Packet, adaptation field (§2.4.3.2–§2.4.3.5) | yes | yes |
| PES packet (§2.4.3.6) | yes | yes |
| Section, PAT, PMT, CAT (§2.4.4) | yes | yes |
| Descriptors 0x02–0x12, AVC video, MPEG-2 AAC audio, HEVC video (§2.6) | yes | yes |
| Pack header, system header, program stream map (§2.5.3) | yes | yes |
| Transport Stream: framing in 188, 192 and 204 bytes, PSI, PES reassembly, PCR | `ts::demux` | `ts::mux`: PES packets, sections, PCRs on their own |
| Program Stream: packs, system headers, maps, PES packets | `ps::demux` | `ps::mux`: pack headers, system headers, maps, PES packets split to fit |

Planned: the ARIB extensions, in a crate of their own.

Out of scope: working out the bounds of a system header; writing a program
stream directory, padding or pack stuffing; null packets for a constant bit
rate; checking the T-STD or the P-STD; working out the time of a PCR; and
subscribing to the sections of a PID other than those of the PSI.

## `no_std` and features

The crate is `no_std`. The `alloc` feature, on by default, brings the
demuxers, the muxers and the writers of the syntax types; without it the
syntax layer reads with no allocator.

## Examples and tests

- `cargo run -p examples --example ts_dump -- <file>` prints every event of a
  Transport Stream; `ps_dump` does the same for a Program Stream.
- `cargo run -p examples --example ts_remux -- <in> <out>` reads a Transport
  Stream and writes it back through the muxer; `ps_remux` does the same for a
  Program Stream.
- Fixtures: streams FFmpeg wrote, under `crates/mpeg2-systems/tests/fixtures/`.
- Conformance: ignored tests compare this crate's reading, and what its
  muxers write, with TSDuck's and ffprobe's.
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
