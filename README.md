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

## Reading a Transport Stream

No type does I/O: the caller hands the bytes it has read to a demuxer in
pieces of any length, calls `finish` at the end of the input, and takes the
events. Handing input over never fails; what the demuxer cannot read is
reported in an event. With `subscribe_elementary_pids` set, the demuxer
reassembles the PES packets of the elementary streams each PMT names, as a
caller subscribing to them at the PMT's event would. This one hands over a
whole file at once.

```rust
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};

let stream = std::fs::read("tests/fixtures/188.ts")?;
let mut demuxer = Demuxer::new(DemuxerConfig::default().with_subscribe_elementary_pids(true));
demuxer.handle_input(&stream);
demuxer.finish();
let mut payloads = Vec::new();
while let Some(event) = demuxer.poll_event() {
    if let Event::PesPacket { packet, .. } = event {
        payloads.push(packet.payload.to_vec());
    }
}
assert_eq!(payloads.len(), 5);
Ok::<(), Box<dyn std::error::Error>>(())
```

A Program Stream reads the same way, with `ps::demux::Demuxer`. The crate
doc of `mpeg2-systems` carries an example of it, and of reading with the
syntax layer alone, without an allocator; that of `mpeg2-systems-arib`, of
reading ARIB captions.

## Multiplexing PES packets

The multiplexing layer, `ts::mux::Muxer` and `ps::mux::Muxer`, takes the PES
packets of the streams registered with it and builds the stream around them:
the PAT, the PMT and the PCRs of a Transport Stream, or the packs, their
SCRs, the system header and the program stream map of a Program Stream. The
caller gives the access units it has, from an encoder or another demuxer, in
the order of their time across the streams.

```rust
use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
use mpeg2_systems::psi::pmt::ElementaryStream;
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::time::Timestamp;
use mpeg2_systems::ts::{Pid, mux::{Muxer, MuxerConfig}};

let (video, audio) = (Pid::try_from(0x0100)?, Pid::try_from(0x0101)?);
let mut muxer = Muxer::new(MuxerConfig::new())?;
muxer.add_program(1, Pid::try_from(0x1000)?)?;
muxer.add_stream(1, ElementaryStream::new(StreamType::AVC_VIDEO, video))?;
muxer.add_stream(1, ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, audio))?;

// Your encoder's access units: PID, stream_id and PTS, in time order
let frame = vec![0u8; 1_000];
let access_units = [(video, 0xe0, 0), (audio, 0xc0, 0), (audio, 0xc0, 1_920), (video, 0xe0, 3_003)];
let mut stream = Vec::new();
for (pid, stream_id, pts) in access_units {
    let header = OptionalHeader::new().with_pts(Timestamp::try_from(pts)?);
    let packet = PesPacket::new(StreamId::from(stream_id), &frame).with_optional_header(header);
    muxer.handle_pes_packet(pid, &packet)?;
    muxer.poll_output(&mut stream); // write `stream` out and clear it as it grows
}
muxer.finish();
muxer.poll_output(&mut stream);
Ok::<(), Box<dyn std::error::Error>>(())
```

The crate doc of `mpeg2-systems` carries a version that writes to a
`std::io::Write`.

## Writing element by element

The packetizing layer, `ts::mux::PacketWriter` and `ps::mux::PackWriter`,
writes each element it is given, in the order given; when to send the PSI
and the PCRs, and where a pack starts, are up to the caller. The module docs
of `ts::mux` and `ps::mux` carry an example of each writer.

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
