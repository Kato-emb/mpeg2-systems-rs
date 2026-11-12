//! Analysis of each Transport Packet the framer cuts
//!
//! The events of one packet are queued in a fixed order: a packet that cannot
//! be read, or one with `transport_error_indicator` set, gives one error and
//! nothing else; any other gives a discontinuity, then its PCR, then the
//! sections it completes, each read or failed, then the PES packets it ends
//! on a subscribed PID. A PAT, PMT or CAT section is queued only at a version
//! not reported before.

use alloc::collections::VecDeque;
use core::mem;
use core::ops::Range;

use super::pending::{Lent, Pending};
use super::{Error, Event};
use crate::psi::section::{PREFIX_LENGTH, SYNTAX_LENGTH, Section};
use crate::ts::Pid;
use crate::ts::packet::TransportPacket;

mod continuity;
mod pes_reassembly;
mod section_reassembly;
mod tables;

use continuity::{ContinuityTracker, Verdict};
use pes_reassembly::{PesReassembly, Sequence};
use section_reassembly::{Completed, SectionReassembler, SectionReassembly};
use tables::{Followed, Tables};

/// Continuity, section reassembly and table version state of the PIDs a
/// demuxer follows
///
/// Sections are reassembled on the PAT and CAT PIDs, on every PID the PAT
/// names for a program map and on the PIDs subscribed to for sections; PES
/// packets on the PIDs subscribed to for PES packets.
#[derive(Debug, Clone)]
pub(super) struct PacketAnalyzer {
    continuity: ContinuityTracker,
    sections: SectionReassembly,
    tables: Tables,
    pes: PesReassembly,
}

impl PacketAnalyzer {
    pub(super) const fn new(pes_buffer_limit: usize) -> Self {
        Self {
            continuity: ContinuityTracker::new(),
            sections: SectionReassembly::new(),
            tables: Tables::new(),
            pes: PesReassembly::new(pes_buffer_limit),
        }
    }

    pub(super) fn subscribe_pes(&mut self, pid: Pid) {
        self.pes.subscribe(pid);
    }

    pub(super) fn unsubscribe_pes(&mut self, pid: Pid, pending: &mut VecDeque<Pending>) {
        self.pes.unsubscribe(pid, pending);
    }

    pub(super) fn subscribe_sections(&mut self, pid: Pid) {
        self.sections.subscribe(pid);
    }

    pub(super) fn unsubscribe_sections(&mut self, pid: Pid, pending: &mut VecDeque<Pending>) {
        self.sections
            .unsubscribe(pid, self.tables.carries_psi(pid), pending);
    }

    /// Returns the bytes of a [`Lent::Reassembled`] range of a PES packet on
    /// `pid`
    pub(super) fn pes_bytes(&self, pid: Pid, range: Range<usize>) -> &[u8] {
        self.pes.bytes(pid, range)
    }

    /// Queues the PES packets in flight, ended by the end of input, then
    /// resets as [`PacketAnalyzer::reset`] does
    pub(super) fn finish(&mut self, pending: &mut VecDeque<Pending>) {
        // Why end the PES packets before the reset: the reset drops what is
        // in flight, and ending them moves their bytes out of flight.
        self.pes.finish(pending);
        self.reset();
    }

    /// Drops the continuity history, and the section and the PES packet in
    /// flight on every PID; the bytes the queued events lend, the PIDs
    /// learned from a PAT, the PAT sections gathered, the versions reported
    /// and the subscriptions stay
    pub(super) fn reset(&mut self) {
        // Why not forget the versions reported, nor the PAT sections
        // gathered: after a resynchronization or across an end of input, the
        // tables on air are most likely the same, so reporting them again
        // would read as a change, and a version names the same programs
        // before and after the gap.
        self.continuity.reset();
        self.sections.discard_in_flight();
        self.pes.discard_in_flight();
    }

    /// Returns the event of a section read whole on `pid`, typed as it was
    /// when analyzed
    pub(super) fn section_event<'a>(&self, pid: Pid, section: &Section<'a>) -> Event<'a> {
        self.tables.event(pid, section)
    }

    /// Returns the bytes of a [`Lent::Reassembled`] range of `pid`
    pub(super) fn reassembled(&self, pid: Pid, range: Range<usize>) -> &[u8] {
        self.sections.bytes(pid, range)
    }

    /// Analyzes one packet, which starts at `packet_start` in the framer's
    /// buffer and whose unit starts at input `offset`, and queues its events
    pub(super) fn analyze(
        &mut self,
        bytes: &[u8],
        packet_start: usize,
        offset: u64,
        pending: &mut VecDeque<Pending>,
    ) {
        let packet = match TransportPacket::read(bytes) {
            Ok(packet) => packet,
            Err(error) => {
                pending.push_back(Pending::Ready(Event::Error(Error::Packet { error })));
                return;
            }
        };
        let pid = packet.pid;
        // Why not report the PCR of such a packet: transport_error_indicator
        // marks its contents as unreliable, and a wrong clock value would
        // mislead clock recovery downstream.
        if packet.transport_error_indicator {
            pending.push_back(Pending::Ready(Event::Error(
                Error::TransportErrorIndicator { pid },
            )));
            self.pes
                .push(&packet, Sequence::Errored, packet_start, offset, pending);
            return;
        }
        let verdict = self.continuity.check(&packet);
        if let Verdict::Broken { expected, found } = verdict {
            pending.push_back(Pending::Ready(Event::Error(Error::Discontinuity {
                pid,
                expected,
                found,
            })));
        }
        // Why report the PCR of a duplicate too: §2.4.3.3 lets a duplicate
        // differ from the original only in its PCR, which can be a valid,
        // restamped value.
        if let Some(program_clock_reference) = packet
            .adaptation_field
            .as_ref()
            .and_then(|field| field.program_clock_reference)
        {
            pending.push_back(Pending::Ready(Event::ProgramClockReference {
                pid,
                program_clock_reference,
            }));
        }
        let payload_start = packet.payload.map_or(packet_start, |payload| {
            packet_start.saturating_add(bytes.len().saturating_sub(payload.len()))
        });
        match pid {
            Pid::PAT => {
                // Why not keep the PAT's reassembler in place while it
                // pushes: the program maps a PAT names are added to the
                // sections reassembled as its sections are read. Putting it
                // back in the entry, which the PAT PID always keeps, allocates
                // nothing.
                let mut reassembler =
                    mem::replace(self.sections.for_psi(pid), SectionReassembler::new());
                reassemble(
                    &mut reassembler,
                    &mut self.tables,
                    Some(&mut self.sections),
                    verdict,
                    &packet,
                    payload_start,
                    pending,
                );
                *self.sections.for_psi(pid) = reassembler;
            }
            _ => {
                let reassembler = if pid == Pid::CAT {
                    Some(self.sections.for_psi(pid))
                } else {
                    self.sections.reassembler_mut(pid)
                };
                if let Some(reassembler) = reassembler {
                    reassemble(
                        reassembler,
                        &mut self.tables,
                        None,
                        verdict,
                        &packet,
                        payload_start,
                        pending,
                    );
                }
            }
        }
        self.pes
            .push(&packet, verdict.into(), payload_start, offset, pending);
    }
}

/// Feeds a packet's payload, which starts at `payload_start` in the framer's
/// buffer, to the reassembler of its PID, and queues the sections it
/// completes
///
/// With `program_maps`, the packet is on the PAT PID: the tables follow the
/// program maps its sections name, and so do the sections reassembled.
fn reassemble(
    reassembler: &mut SectionReassembler,
    tables: &mut Tables,
    mut program_maps: Option<&mut SectionReassembly>,
    verdict: Verdict,
    packet: &TransportPacket<'_>,
    payload_start: usize,
    pending: &mut VecDeque<Pending>,
) {
    let pid = packet.pid;
    reassembler.push(
        verdict,
        packet.payload_unit_start_indicator,
        packet.payload,
        &mut |completed, section_bytes| {
            let section = match Section::read(section_bytes) {
                Ok(section) => section,
                Err(error) => {
                    pending.push_back(Pending::Ready(Event::Error(Error::Section { pid, error })));
                    return;
                }
            };
            let Some(event) = tables.admit(pid, &section) else {
                return;
            };
            if let Event::Error(error) = event {
                pending.push_back(Pending::Ready(Event::Error(error)));
                return;
            }
            if let (Event::ProgramAssociationSection(pat), Some(program_maps)) =
                (&event, program_maps.as_deref_mut())
            {
                let followed = tables.follow(pat);
                if followed != Followed::Repeated {
                    for pid in tables::program_map_pids(&pat.programs) {
                        program_maps.for_psi(pid);
                    }
                }
                // Why stop reassembling only once a version is whole: until
                // then, the programs of the previous version stay named.
                if followed == Followed::Whole {
                    program_maps.retain(|pid| tables.carries_psi(pid));
                }
            }
            let header_length = if section.syntax.is_some() {
                PREFIX_LENGTH.saturating_add(SYNTAX_LENGTH)
            } else {
                PREFIX_LENGTH
            };
            let body = |section_start: usize| {
                let start = section_start.saturating_add(header_length);
                start..start.saturating_add(section.body.len())
            };
            pending.push_back(Pending::Section {
                pid,
                body: match completed {
                    Completed::InPayload(start) => {
                        Lent::Packet(body(payload_start.saturating_add(start)))
                    }
                    Completed::Reassembled(start) => Lent::Reassembled(body(start)),
                },
                section: Section {
                    body: &[],
                    ..section
                },
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use crate::ts::Pid;
    use crate::ts::demux::tests::{
        behind_null_packets, events_of, packet, pat_section, pat_section_of, section_packet,
    };
    use crate::ts::demux::{Demuxer, DemuxerConfig};

    // Why not test through poll_event: a section kept in the buffer changes no
    // event, only the memory the demuxer holds, so the test reads the buffer.
    #[test]
    fn a_section_that_fails_to_read_does_not_grow_the_buffer_input_after_input() {
        // A CAT section of 203 bytes whose CRC_32 fails, across two packets
        let mut section = vec![0x01, 0xB0, 200, 0x00, 0x00, 0xC1, 0x00, 0x00];
        section.resize(203, 0x00);
        let (head, tail) = section.split_at(183);
        let mut stream = Vec::new();
        for _ in 0..5 {
            stream.extend(packet(0x1FFF, false, 0, &[]));
        }
        stream.extend(packet(0x0001, true, 0, &[&[0x00][..], head].concat()));
        stream.extend(packet(0x0001, false, 1, tail));

        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        let mut lengths = Vec::new();
        for _ in 0..4 {
            demuxer.handle_input(&stream);
            while demuxer.poll_event().is_some() {}
            lengths.push(demuxer.analyzer.sections.buffer_len(Pid::CAT).unwrap());
        }
        let first = lengths.first().copied().unwrap();
        assert_eq!(lengths, [first; 4]);
    }

    #[test]
    fn a_pat_across_packets_names_its_program_maps() {
        // A PAT of 45 programs, program_number 1 to 45 on PIDs 0x101 to 0x12D,
        // long enough to cross two packets
        let mut section = vec![0x00, 0xB0, 189, 0x00, 0x01, 0xC1, 0x00, 0x00];
        let pids: Vec<u16> = (0x0101..=0x012D).collect();
        for (program_number, pid) in (1_u16..).zip(&pids) {
            section.extend_from_slice(&program_number.to_be_bytes());
            section.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        }
        section.extend_from_slice(&crate::crc::crc_32(&section).to_be_bytes());
        let (head, tail) = section.split_at(183);
        let mut stream = Vec::new();
        for _ in 0..5 {
            stream.extend(packet(0x1FFF, false, 0, &[]));
        }
        stream.extend(packet(0x0000, true, 0, &[&[0x00][..], head].concat()));
        stream.extend(packet(0x0000, false, 1, tail));

        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        demuxer.handle_input(&stream);
        while demuxer.poll_event().is_some() {}
        let analyzer = &demuxer.analyzer;
        let followed: Vec<u16> = analyzer.sections.pids().map(Pid::get).collect();
        assert_eq!(followed, [&[0x0000][..], &pids].concat());
    }

    #[test]
    fn a_whole_pat_version_stops_reassembling_only_the_program_maps_it_no_longer_names() {
        let versions = [
            section_packet(0x0000, 0, &pat_section(0, &[(1, 0x0100), (2, 0x0200)])),
            section_packet(0x0000, 1, &pat_section(1, &[(2, 0x0200), (3, 0x0300)])),
            section_packet(0x0000, 2, &pat_section_of(2, (0, 1), &[(4, 0x0400)])),
        ];
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        let mut stream = behind_null_packets(&[]);
        let mut followed = Vec::new();
        for packet in versions {
            stream.extend(packet);
            events_of(&mut demuxer, &stream);
            stream.clear();
            followed.push(
                demuxer
                    .analyzer
                    .sections
                    .pids()
                    .map(Pid::get)
                    .collect::<Vec<_>>(),
            );
        }
        assert_eq!(
            followed,
            [
                vec![0x0000, 0x0100, 0x0200],
                vec![0x0000, 0x0200, 0x0300],
                vec![0x0000, 0x0200, 0x0300, 0x0400],
            ]
        );
    }
}
