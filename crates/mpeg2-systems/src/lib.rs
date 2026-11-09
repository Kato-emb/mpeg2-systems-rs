//! Sans-IO reading and writing of MPEG-2 Systems streams (ISO/IEC 13818-1):
//! the Transport Stream and the Program Stream
//!
//! No type does I/O. A reader is handed the bytes the caller has read, and a
//! writer appends bytes to a buffer the caller owns, so the crate is `no_std`
//! and runs wherever the caller's I/O does.
//!
//! # Layers
//!
//! - The syntax layer reads and writes one syntax element from bytes that hold
//!   it: a Transport Packet ([`ts::packet`]), an adaptation field
//!   ([`ts::adaptation_field`]), a PES packet ([`pes`]), a section and the
//!   PSI tables ([`psi`]), the descriptors ([`descriptor`]), and the pack
//!   header, system header and program stream map ([`ps`]). Reading needs no
//!   allocation.
//! - With the `alloc` feature, [`ts::demux`] and [`ps::demux`] read a stream
//!   handed over in pieces of any length, and [`ts::mux`] and [`ps::mux`]
//!   write one.
//!
//! Each layer is usable alone: a reader of its own is made of the syntax
//! layer, as the demuxers are.
//!
//! # Driving a demuxer
//!
//! Handing input to a demuxer never fails: what it cannot read is reported in
//! an event. After each piece of input, the caller takes the events until
//! there are none, and at the end of the input calls `finish` and takes the
//! events left. An event borrows the bytes it reports from the demuxer, so
//! the demuxer cannot be used until the event is dropped; a Transport Stream
//! demuxer reassembles the PES packets of the PIDs subscribed to, which the
//! caller learns from the PMTs it reads.
//!
//! # Examples
//!
//! Reading a Transport Stream, subscribing to the elementary streams each
//! PMT names, and counting the PES packets:
//!
//! ```
//! # #[cfg(feature = "alloc")]
//! # {
//! use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
//!
//! let stream = std::fs::read("tests/fixtures/188.ts")?;
//! let mut demuxer = Demuxer::new(DemuxerConfig::default());
//! let mut pes_packets = 0;
//! // Hand over the input in pieces of any length, taking the events after each
//! for chunk in stream.chunks(7 * 188) {
//!     demuxer.handle_input(chunk);
//!     while let Some(event) = demuxer.poll_event() {
//!         if let Event::PesPacket { .. } = event {
//!             pes_packets += 1;
//!         }
//!         // Copy the PIDs out of a PMT, as the event borrows the demuxer
//!         let Event::TsProgramMapSection { section, .. } = event else {
//!             continue;
//!         };
//!         let pids: Vec<_> = section
//!             .elementary_streams
//!             .iter()
//!             .flatten()
//!             .map(|stream| stream.elementary_pid)
//!             .collect();
//!         for pid in pids {
//!             demuxer.subscribe_pes(pid);
//!         }
//!     }
//! }
//! // End the input and take the events it leaves
//! demuxer.finish();
//! while let Some(event) = demuxer.poll_event() {
//!     if let Event::PesPacket { .. } = event {
//!         pes_packets += 1;
//!     }
//! }
//! assert_eq!(pes_packets, 5);
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Reading a Program Stream and counting its packs and PES packets:
//!
//! ```
//! # #[cfg(feature = "alloc")]
//! # {
//! use mpeg2_systems::ps::demux::{Demuxer, Event};
//!
//! let stream = std::fs::read("tests/fixtures/program.ps")?;
//! let mut demuxer = Demuxer::new();
//! let (mut packs, mut pes_packets) = (0, 0);
//! let mut take = |event: Event<'_>| match event {
//!     Event::PackHeader(_) => packs += 1,
//!     Event::PesPacket(_) => pes_packets += 1,
//!     _ => {}
//! };
//! for chunk in stream.chunks(2048) {
//!     demuxer.handle_input(chunk);
//!     while let Some(event) = demuxer.poll_event() {
//!         take(event);
//!     }
//! }
//! demuxer.finish();
//! while let Some(event) = demuxer.poll_event() {
//!     take(event);
//! }
//! assert_eq!((packs, pes_packets), (5, 7));
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Writing a Transport Stream of one program, a PAT, a PMT, a PCR on its own
//! and an access unit, and reading it back:
//!
//! ```
//! # #[cfg(feature = "alloc")]
//! # {
//! use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
//! use mpeg2_systems::psi::VersionNumber;
//! use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
//! use mpeg2_systems::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
//! use mpeg2_systems::stream_type::StreamType;
//! use mpeg2_systems::time::{ClockReference, Timestamp};
//! use mpeg2_systems::ts::Pid;
//! use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
//! use mpeg2_systems::ts::mux::PacketWriter;
//!
//! let (pmt_pid, video_pid) = (Pid::try_from(0x1000)?, Pid::try_from(0x0100)?);
//! let mut writer = PacketWriter::new();
//! let mut stream = Vec::new();
//! // The PAT and the PMT, built with their own types
//! let mut programs = Vec::new();
//! ProgramLoop::write_from(
//!     [Program::ProgramMap { program_number: 1.try_into()?, program_map_pid: pmt_pid }],
//!     &mut programs,
//! )?;
//! let mut pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0)?);
//! pat.programs = ProgramLoop::from(&programs[..]);
//! writer.write_program_association_section(&pat, &mut stream)?;
//! let mut streams = Vec::new();
//! ElementaryStreamLoop::write_from(
//!     [ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video_pid)],
//!     &mut streams,
//! )?;
//! let mut pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0)?);
//! pmt.pcr_pid = Some(video_pid);
//! pmt.elementary_streams = ElementaryStreamLoop::from(&streams[..]);
//! writer.write_ts_program_map_section(pmt_pid, &pmt, &mut stream)?;
//! // A PCR on its own, then an access unit in a PES packet
//! writer.write_program_clock_reference(video_pid, ClockReference::new(81_000, 0)?, &mut stream)?;
//! let access_unit = vec![0xaa; 1_000];
//! let mut header = OptionalHeader::new();
//! header.pts = Some(Timestamp::try_from(90_000)?);
//! let mut packet = PesPacket::new(StreamId::from(0xe0), &access_unit);
//! packet.optional_header = Some(header);
//! writer.write_pes_packet(video_pid, &packet, None, &mut stream)?;
//!
//! // Read back, the access unit is the payload of the one PES packet
//! let mut demuxer = Demuxer::new(DemuxerConfig::default());
//! demuxer.subscribe_pes(video_pid);
//! demuxer.handle_input(&stream);
//! demuxer.finish();
//! let mut payloads = Vec::new();
//! while let Some(event) = demuxer.poll_event() {
//!     if let Event::PesPacket { packet, .. } = event {
//!         payloads.push(packet.payload.to_vec());
//!     }
//! }
//! assert_eq!(payloads, [access_unit]);
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Reading the PAT out of a stream with the syntax layer alone, which
//! allocates nothing:
//!
//! ```
//! use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection};
//! use mpeg2_systems::psi::section::{self, Section};
//! use mpeg2_systems::ts::Pid;
//! use mpeg2_systems::ts::packet::TransportPacket;
//!
//! let stream = std::fs::read("tests/fixtures/188.ts")?;
//! // The first Transport Packet that starts a section on the PID of the PAT
//! let packet = stream
//!     .chunks(188)
//!     .filter_map(|bytes| TransportPacket::read(bytes).ok())
//!     .find(|packet| packet.pid == Pid::PAT && packet.payload_unit_start_indicator)
//!     .ok_or("no PAT")?;
//! // Skip the pointer_field and the bytes it counts
//! let (&pointer_field, rest) = packet.payload.and_then(<[u8]>::split_first).ok_or("no payload")?;
//! let bytes = rest.get(usize::from(pointer_field)..).ok_or("pointer_field past the payload")?;
//! // Cut the section to its length, read from its first 3 bytes
//! let length = section::encoded_len(bytes).ok_or("section cut short")?;
//! let section = Section::read(bytes.get(..length).ok_or("section cut short")?)?;
//! // Read the PAT from the section
//! let pat = ProgramAssociationSection::try_from(&section)?;
//! assert_eq!(pat.transport_stream_id, 1);
//! for program in pat.programs.iter() {
//!     if let Program::ProgramMap { program_number, program_map_pid } = program? {
//!         assert_eq!((program_number.get(), program_map_pid), (1, Pid::try_from(0x1000)?));
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! The programs `examples/` holds dump a stream event by event and remux one
//! through the muxers: `cargo run -p examples --example ts_dump -- <file>`.
//!
//! # Modules
//!
//! - [`ts`]: the Transport Packet, its adaptation field, the PID and the
//!   continuity counter, and the Transport Stream demuxer and muxer
//! - [`pes`]: the PES packet and its optional header
//! - [`psi`]: the section, and the PAT, the PMT and the CAT
//! - [`descriptor`]: the descriptor loop, and the descriptors read into their
//!   fields
//! - [`ps`]: the pack header, the system header and the program stream map,
//!   and the Program Stream demuxer and muxer
//! - [`time`]: the PTS and DTS, and the PCR and SCR
//! - [`crc`]: the `CRC_32` of a section
//! - [`field`]: the bit-field reader and writer the syntax types are made of
//! - [`stream_type`]: the `stream_type` of an elementary stream
//!
//! # Features
//!
//! - `alloc` (default): the demuxers and the muxers, and the writers of the
//!   syntax types. Without it, the syntax layer reads with no allocator.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod crc;
pub mod descriptor;
pub mod field;
#[cfg(feature = "alloc")]
mod input_buffer;
pub mod pes;
pub mod ps;
pub mod psi;
pub mod stream_type;
pub mod time;
pub mod ts;
