//! Writing a Transport Stream: PES packets and sections cut into Transport
//! Packets
//!
//! A Transport Stream is written payload unit by payload unit into a buffer
//! the caller owns. [`Muxer`] cuts each PES packet or section into the
//! Transport Packets of a PID, and keeps the `continuity_counter` of each PID.
//! What to write, and when, is up to the caller: the PAT and the PMT built
//! with their own types and sent again as often as the caller chooses, the
//! PCR taken from the caller's clock. The crate does not write null packets,
//! keep a constant bit rate, check the T-STD, or put more than one section in
//! a Transport Packet.
//!
//! ```
//! use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
//! use mpeg2_systems::psi::VersionNumber;
//! use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
//! use mpeg2_systems::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
//! use mpeg2_systems::stream_type::StreamType;
//! use mpeg2_systems::time::{ClockReference, Timestamp};
//! use mpeg2_systems::ts::Pid;
//! use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
//! use mpeg2_systems::ts::mux::Muxer;
//!
//! let (pmt_pid, video_pid) = (Pid::try_from(0x1000)?, Pid::try_from(0x0100)?);
//! let mut muxer = Muxer::new();
//! let mut stream = Vec::new();
//!
//! // The PAT, naming program 1 and its PMT
//! let mut programs = Vec::new();
//! ProgramLoop::write_from(
//!     [Program::ProgramMap {
//!         program_number: 1.try_into()?,
//!         program_map_pid: pmt_pid,
//!     }],
//!     &mut programs,
//! )?;
//! let mut pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0)?);
//! pat.programs = ProgramLoop::from(&programs[..]);
//! muxer.write_program_association_section(&pat, &mut stream)?;
//! // The PMT, its PCR on the video PID
//! let mut streams = Vec::new();
//! ElementaryStreamLoop::write_from(
//!     [ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video_pid)],
//!     &mut streams,
//! )?;
//! let mut pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0)?);
//! pmt.pcr_pid = Some(video_pid);
//! pmt.elementary_streams = ElementaryStreamLoop::from(&streams[..]);
//! muxer.write_ts_program_map_section(pmt_pid, &pmt, &mut stream)?;
//! // An access unit, with the PCR of the program
//! let access_unit = vec![0xaa; 1_000];
//! let mut header = OptionalHeader::new();
//! header.pts = Some(Timestamp::try_from(90_000)?);
//! let mut packet = PesPacket::new(StreamId::from(0xe0), &access_unit);
//! packet.optional_header = Some(header);
//! let pcr = ClockReference::new(81_000, 0)?;
//! assert_eq!(muxer.write_pes_packet(video_pid, &packet, Some(pcr), &mut stream)?, 6);
//!
//! // Read back
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
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use crate::field::write_or_truncate;
use crate::pes::{self, PesPacket, StreamId};
use crate::psi::cat::{self, CaSection};
use crate::psi::pat::{self, ProgramAssociationSection};
use crate::psi::pmt::{self, TsProgramMapSection};
use crate::psi::section::{self, Section};
use crate::time::ClockReference;
use crate::ts::adaptation_field::{AdaptationField, MAX_ADAPTATION_FIELD_LENGTH};
use crate::ts::packet::{self, HEADER_LENGTH, PACKET_LENGTH, TransportPacket};
use crate::ts::{ContinuityCounter, Pid};

/// Bytes of a Transport Packet after its header
const PAYLOAD_CAPACITY: usize = PACKET_LENGTH - HEADER_LENGTH;

/// `pointer_field` 0: the section starts right after it
const POINTER_FIELD: u8 = 0x00;

/// What fills a Transport Packet the end of a payload unit leaves short
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stuffing {
    /// Stuffing bytes in the adaptation field, the only stuffing allowed for
    /// PES packets (§2.4.3.5)
    AdaptationField,
    /// `0xFF` in the payload after the last byte of a section (§2.4.4.1)
    Payload,
}

/// Failure to write a PES packet or a section as Transport Packets
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The PID given cannot carry what is written: a PES packet only goes on
    /// a PID from 0x0010 to 0x1FFE, and nothing on the PID of null packets
    Pid {
        /// The PID given
        pid: Pid,
    },
    /// The `stream_id` given is not that of a PES packet
    StreamId {
        /// The `stream_id` given
        stream_id: StreamId,
    },
    /// The PES packet could not be written
    PesPacket {
        /// How the PES packet failed
        error: pes::Error,
    },
    /// The section could not be written
    Section {
        /// How the section failed
        error: section::Error,
    },
    /// The program association section could not be written
    ProgramAssociationSection {
        /// How the section failed
        error: pat::Error,
    },
    /// The TS program map section could not be written
    TsProgramMapSection {
        /// How the section failed
        error: pmt::Error,
    },
    /// The CA section could not be written
    CaSection {
        /// How the section failed
        error: cat::Error,
    },
    /// A Transport Packet could not be written
    Packet {
        /// How the Transport Packet failed
        error: packet::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pid { pid } => write!(
                formatter,
                "PID {:#06x} cannot carry what is written",
                pid.get()
            ),
            Self::StreamId { stream_id } => write!(
                formatter,
                "stream_id {:#04x} is not that of a PES packet",
                stream_id.get()
            ),
            Self::PesPacket { .. } => write!(formatter, "a PES packet could not be written"),
            Self::Section { .. } => write!(formatter, "a section could not be written"),
            Self::ProgramAssociationSection { .. } => {
                write!(
                    formatter,
                    "a program association section could not be written"
                )
            }
            Self::TsProgramMapSection { .. } => {
                write!(formatter, "a TS program map section could not be written")
            }
            Self::CaSection { .. } => write!(formatter, "a CA section could not be written"),
            Self::Packet { .. } => write!(formatter, "a Transport Packet could not be written"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::PesPacket { error } => Some(error),
            Self::Section { error } => Some(error),
            Self::ProgramAssociationSection { error } => Some(error),
            Self::TsProgramMapSection { error } => Some(error),
            Self::CaSection { error } => Some(error),
            Self::Packet { error } => Some(error),
            Self::Pid { .. } | Self::StreamId { .. } => None,
        }
    }
}

/// Transport Stream muxer, which cuts PES packets and sections into the
/// Transport Packets of a PID, in a buffer the caller owns
///
/// The muxer keeps the `continuity_counter` of each PID it has written, and
/// nothing else; a write that fails leaves the buffer and the counters as
/// they were. Which PIDs carry what, which packets carry a PCR, and when the
/// PAT and the PMT are sent again are up to the caller.
#[derive(Debug, Clone, Default)]
pub struct Muxer {
    /// The `continuity_counter` the next Transport Packet of each PID takes
    // Why not the counter of the last packet written: a PID not in the map
    // then starts at 0 without a second case.
    continuity_counters: BTreeMap<Pid, ContinuityCounter>,
}

impl Muxer {
    /// Returns a muxer that has written nothing
    #[must_use]
    pub const fn new() -> Self {
        Self {
            continuity_counters: BTreeMap::new(),
        }
    }

    /// Appends a Transport Packet of `pid` that carries
    /// `program_clock_reference` and nothing else to `out`
    ///
    /// PCR, ISO/IEC 13818-1 §2.4.3.4 and §2.4.3.5
    ///
    /// The packet is an adaptation field filling the whole packet, its
    /// `PCR_flag` set, with no payload, so a PCR can be sent as often as
    /// §2.7.2 asks however far apart the PES packets of `pid` are; how often
    /// is up to the caller. The packet takes the `continuity_counter` of the
    /// packet of `pid` written before it, as a packet without a payload does
    /// not advance it. A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pid` is not one Table 2-3 lets carry a PCR:
    ///   0x0000, 0x0001 and 0x0010 to 0x1FFE.
    /// * [`Packet`](Error::Packet): [`TransportPacket::write`] refuses the
    ///   packet, as it does a PCR with an extension of 300 or more.
    pub fn write_program_clock_reference(
        &mut self,
        pid: Pid,
        program_clock_reference: ClockReference,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        if !pid.may_carry_pcr() {
            return Err(Error::Pid { pid });
        }
        // Why the counter before the next one rather than the next one: a
        // packet without a payload does not advance the counter (§2.4.3.3),
        // so it repeats that of the packet before it, and the next packet
        // with a payload follows on from it. On a PID not yet written the
        // same counter makes the first packet with a payload follow on too.
        let previous = self
            .continuity_counters
            .get(&pid)
            .copied()
            .unwrap_or(ContinuityCounter(0))
            .previous();
        let mut adaptation_field = AdaptationField::new();
        adaptation_field.program_clock_reference = Some(program_clock_reference);
        adaptation_field.stuffing_bytes =
            MAX_ADAPTATION_FIELD_LENGTH.saturating_sub(adaptation_field.adaptation_field_length());
        let mut packet = TransportPacket::new(pid, previous);
        packet.adaptation_field = Some(adaptation_field);
        packet.write(out).map_err(|error| Error::Packet { error })
    }

    /// Appends `packet` to `out` as Transport Packets of `pid`, and returns
    /// how many it wrote
    ///
    /// PES packet, ISO/IEC 13818-1 §2.4.3.6, carried as §2.4.3.3 lays out
    ///
    /// One PES packet goes into as many Transport Packets as it needs (where
    /// the Program Stream muxer's `write_pes_packets` splits a payload into
    /// PES packets). The first has `payload_unit_start_indicator` set and,
    /// when `pcr` is given, the PCR in its adaptation field; the last is
    /// filled out with stuffing bytes in its adaptation field. A video PES
    /// packet too long for `PES_packet_length` is written with
    /// `length_unspecified` set, as a Transport Stream allows.
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pid` is below 0x0010 or that of null packets.
    /// * [`StreamId`](Error::StreamId): the `stream_id` is below 0xBD, not
    ///   that of a PES packet.
    /// * [`PesPacket`](Error::PesPacket): [`PesPacket::write`] refuses the
    ///   packet, as it does one too long for `PES_packet_length` without
    ///   `length_unspecified`, or `length_unspecified` on a stream other than
    ///   video.
    /// * [`Packet`](Error::Packet): [`TransportPacket::write`] refuses a
    ///   packet, as it does a PCR with an extension of 300 or more.
    pub fn write_pes_packet(
        &mut self,
        pid: Pid,
        packet: &PesPacket<'_>,
        pcr: Option<ClockReference>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        if !pid.is_assignable() {
            return Err(Error::Pid { pid });
        }
        let stream_id = packet.stream_id;
        if !stream_id.is_pes_packet() {
            return Err(Error::StreamId { stream_id });
        }
        let adaptation_field = pcr.map(|pcr| {
            let mut adaptation_field = AdaptationField::new();
            adaptation_field.program_clock_reference = Some(pcr);
            adaptation_field
        });
        self.write_payload_unit(
            pid,
            |out| {
                packet
                    .write(out)
                    .map_err(|error| Error::PesPacket { error })
            },
            adaptation_field,
            Stuffing::AdaptationField,
            out,
        )
    }

    /// Appends `section` to `out` as Transport Packets of `pid`, and returns
    /// how many it wrote
    ///
    /// Section, ISO/IEC 13818-1 §2.4.4, carried as §2.4.4.1 lays out
    ///
    /// The first packet has `payload_unit_start_indicator` set and a
    /// `pointer_field` of 0 before the section; the last is filled out with
    /// `0xFF` after the section. Each section starts a packet of its own.
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pid` is that of null packets.
    /// * [`Section`](Error::Section): [`Section::write`] refuses the section.
    pub fn write_section(
        &mut self,
        pid: Pid,
        section: &Section<'_>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.write_section_with(
            pid,
            |out| section.write(out).map_err(|error| Error::Section { error }),
            out,
        )
    }

    /// Appends `section`, the PAT, to `out` as Transport Packets of the PID
    /// of the PAT, and returns how many it wrote
    ///
    /// The packets are those [`write_section`](Self::write_section) writes.
    ///
    /// # Errors
    ///
    /// * [`ProgramAssociationSection`](Error::ProgramAssociationSection):
    ///   [`ProgramAssociationSection::write`] refuses the section.
    pub fn write_program_association_section(
        &mut self,
        section: &ProgramAssociationSection<'_>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.write_section_with(
            Pid::PAT,
            |out| {
                section
                    .write(out)
                    .map_err(|error| Error::ProgramAssociationSection { error })
            },
            out,
        )
    }

    /// Appends `section`, a PMT, to `out` as Transport Packets of `pid`, its
    /// `program_map_PID`, and returns how many it wrote
    ///
    /// The packets are those [`write_section`](Self::write_section) writes.
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pid` is that of null packets.
    /// * [`TsProgramMapSection`](Error::TsProgramMapSection):
    ///   [`TsProgramMapSection::write`] refuses the section.
    pub fn write_ts_program_map_section(
        &mut self,
        pid: Pid,
        section: &TsProgramMapSection<'_>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.write_section_with(
            pid,
            |out| {
                section
                    .write(out)
                    .map_err(|error| Error::TsProgramMapSection { error })
            },
            out,
        )
    }

    /// Appends `section`, the CAT, to `out` as Transport Packets of the PID
    /// of the CAT, and returns how many it wrote
    ///
    /// The packets are those [`write_section`](Self::write_section) writes.
    ///
    /// # Errors
    ///
    /// * [`CaSection`](Error::CaSection): [`CaSection::write`] refuses the
    ///   section.
    pub fn write_ca_section(
        &mut self,
        section: &CaSection<'_>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        self.write_section_with(
            Pid::CAT,
            |out| {
                section
                    .write(out)
                    .map_err(|error| Error::CaSection { error })
            },
            out,
        )
    }

    /// Appends the section `write_section` writes as Transport Packets of
    /// `pid`, after a `pointer_field` of 0, and returns how many it wrote
    fn write_section_with(
        &mut self,
        pid: Pid,
        write_section: impl FnOnce(&mut Vec<u8>) -> Result<(), Error>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        if pid == Pid::NULL {
            return Err(Error::Pid { pid });
        }
        self.write_payload_unit(
            pid,
            |out| {
                out.push(POINTER_FIELD);
                write_section(out)
            },
            None,
            Stuffing::Payload,
            out,
        )
    }

    /// Appends the payload unit `write_unit` writes as Transport Packets of
    /// `pid`, the first with `adaptation_field`, and returns how many it wrote
    fn write_payload_unit(
        &mut self,
        pid: Pid,
        write_unit: impl FnOnce(&mut Vec<u8>) -> Result<(), Error>,
        mut adaptation_field: Option<AdaptationField<'_>>,
        stuffing: Stuffing,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let mut continuity_counter = self
            .continuity_counters
            .get(&pid)
            .copied()
            .unwrap_or(ContinuityCounter(0));
        let mut written: usize = 0;
        let start = out.len();
        write_or_truncate(out, |out| {
            // Why write the unit into `out` and copy it out packet by packet,
            // then remove it: the packets need its bytes whole, and `out` is
            // the one buffer that grows to fit without the muxer holding one
            // of its own.
            write_unit(out)?;
            let end = out.len();
            let mut offset = start;
            loop {
                let adaptation_field_length = adaptation_field
                    .as_ref()
                    .map_or(0, AdaptationField::encoded_len);
                let capacity = PAYLOAD_CAPACITY.saturating_sub(adaptation_field_length);
                let length = end.saturating_sub(offset).min(capacity);
                let gap = capacity.saturating_sub(length);
                let mut payload = [0xFF; PAYLOAD_CAPACITY];
                let (unit_bytes, _) = out.split_at(offset).1.split_at(length);
                payload.split_at_mut(length).0.copy_from_slice(unit_bytes);
                let payload_length = match stuffing {
                    Stuffing::Payload => capacity,
                    Stuffing::AdaptationField => {
                        if gap > 0 {
                            // Why grow the stuffing up to a length rather than
                            // by the gap: a new adaptation field takes one
                            // byte of the gap for its `adaptation_field_length`.
                            let filling = adaptation_field_length.saturating_add(gap);
                            let stuffed = adaptation_field.get_or_insert_with(AdaptationField::new);
                            stuffed.stuffing_bytes = stuffed
                                .stuffing_bytes
                                .saturating_add(filling.saturating_sub(stuffed.encoded_len()));
                        }
                        length
                    }
                };

                let mut packet = TransportPacket::new(pid, continuity_counter);
                packet.payload_unit_start_indicator = written == 0;
                packet.adaptation_field = adaptation_field.take();
                packet.payload = Some(payload.split_at(payload_length).0);
                packet.write(out).map_err(|error| Error::Packet { error })?;
                continuity_counter = continuity_counter.next();
                written = written.saturating_add(1);
                offset = offset.saturating_add(length);
                if offset >= end {
                    break;
                }
            }
            out.drain(start..end);
            Ok(())
        })?;
        self.continuity_counters.insert(pid, continuity_counter);
        Ok(written)
    }
}

#[cfg(test)]
mod tests;
