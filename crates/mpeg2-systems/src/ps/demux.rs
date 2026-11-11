//! Demuxing a Program Stream: input of any length in, events out

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::ops::Range;

use crate::pes::PesPacket;
use crate::ps::{PackHeader, ProgramStreamMap, SystemHeader};
use crate::psi::VersionNumber;

mod event;
mod framer;

pub use event::{Error, Event};
use framer::{Element, Output, PackFramer};

/// An event queued for [`Demuxer::poll_event`], holding the range of the
/// bytes it lends rather than the bytes
#[derive(Debug, Clone)]
enum Pending {
    /// An event that lends nothing
    Ready(Event<'static>),
    /// An element cut whole, read when polled
    Element(Element, Range<usize>),
    Unsynchronized(Range<usize>),
    Unframable(Range<usize>),
    Truncated(Range<usize>),
}

/// Program Stream demuxer, which takes input of any length and reports what
/// it finds as events
///
/// The demuxer synchronizes on the first pack header it can read, then cuts
/// each pack header, system header, program stream map and PES packet off the
/// input by its declared length, and reports each byte it could not cut.
/// System headers and program stream maps are reported when they change.
/// Handing input over never fails: a broken stream is reported as
/// [`Event::Error`] and [`Event::Unframable`], and demuxing goes on. The
/// demuxer does no I/O, and does not read the elementary streams; both are up
/// to the caller.
///
/// ```
/// use mpeg2_systems::ps::demux::{Demuxer, Event};
///
/// let stream = include_bytes!("../../tests/fixtures/program.ps");
/// let mut demuxer = Demuxer::new();
/// let (mut packs, mut system_headers, mut pes_packets) = (0, 0, Vec::new());
/// let mut take = |event: Event<'_>| match event {
///     Event::PackHeader(_) => packs += 1,
///     Event::SystemHeader(_) => system_headers += 1,
///     Event::PesPacket(packet) => pes_packets.push(packet.stream_id.get()),
///     _ => {}
/// };
///
/// // Hand over the input in any pieces, taking the events after each
/// for chunk in stream.chunks(1000) {
///     demuxer.handle_input(chunk);
///     while let Some(event) = demuxer.poll_event() {
///         take(event);
///     }
/// }
///
/// // End the input and take the events it leaves
/// demuxer.finish();
/// while let Some(event) = demuxer.poll_event() {
///     take(event);
/// }
///
/// assert_eq!((packs, system_headers), (5, 1));
/// assert_eq!(pes_packets, [0xe0, 0xc0, 0xe0, 0xbe, 0xc0, 0xc0, 0xbe]);
/// ```
#[derive(Debug, Clone)]
pub struct Demuxer {
    framer: PackFramer,
    /// The bytes of the system header last reported, or none
    // Why the bytes rather than an Option of them: a system header is never
    // empty, so an empty Vec states "none reported", and it keeps the
    // capacity it grew to across a program end.
    system_header: Vec<u8>,
    program_stream_map_version: Option<VersionNumber>,
    pending: VecDeque<Pending>,
}

impl Default for Demuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl Demuxer {
    /// Returns a demuxer that has seen no input
    #[must_use]
    pub const fn new() -> Self {
        Self {
            framer: PackFramer::new(),
            system_header: Vec::new(),
            program_stream_map_version: None,
            pending: VecDeque::new(),
        }
    }

    /// Hands over the next bytes of the stream, cut anywhere
    ///
    /// The bytes are only kept; [`Demuxer::poll_event`] reads them.
    ///
    /// # Panics
    /// Panics if called after [`Demuxer::finish`] before
    /// [`Demuxer::poll_event`] has returned `None`.
    pub fn handle_input(&mut self, bytes: &[u8]) {
        // Why not compact here: a queued event can lend a range of the
        // input, which compacting would shift.
        self.framer.extend(bytes);
    }

    /// Reads the input as far as the next event and returns it, or `None`
    /// when the input handed over so far holds no more events
    pub fn poll_event(&mut self) -> Option<Event<'_>> {
        // Why queue ranges and lend only at the end: returning a borrow from
        // inside this loop, which steps the framer again on the next turn, is
        // refused by the borrow checker. The framer steps only once the queue
        // is empty, so the bytes the queued ranges point at stay put.
        while self.pending.is_empty() {
            self.framer.compact();
            match self.framer.step() {
                Output::NeedMoreData => return None,
                Output::EndOfInput => {}
                Output::SyncAcquired(range) => {
                    self.pending.push_back(Pending::Ready(Event::SyncAcquired));
                    self.pending
                        .push_back(Pending::Element(Element::PackHeader, range));
                }
                Output::Element(Element::SystemHeader, range) => {
                    // Why compare the bytes rather than the header read: a
                    // system header restates the same layout throughout a
                    // stream, so only one whose bytes differ tells anything,
                    // and comparing bytes needs no reading.
                    let bytes = self.framer.bytes(range.clone());
                    if bytes != self.system_header.as_slice() {
                        self.system_header.clear();
                        self.system_header.extend_from_slice(bytes);
                        self.pending
                            .push_back(Pending::Element(Element::SystemHeader, range));
                    }
                }
                Output::Element(Element::ProgramStreamMap, range) => {
                    let map = ProgramStreamMap::read(self.framer.bytes(range.clone()));
                    // Why not take a map with current_next_indicator '0' as
                    // a change: it announces the next version, and the map in
                    // force stays until that version comes with the indicator
                    // set (§2.5.4.2).
                    match map {
                        Ok(map)
                            if map.current_next_indicator
                                && self.program_stream_map_version
                                    != Some(map.program_stream_map_version) =>
                        {
                            self.program_stream_map_version = Some(map.program_stream_map_version);
                            self.pending
                                .push_back(Pending::Element(Element::ProgramStreamMap, range));
                        }
                        Ok(_) => {}
                        // Why queue a map that does not read: the read when
                        // polled reports it, as it does every element.
                        Err(_) => self
                            .pending
                            .push_back(Pending::Element(Element::ProgramStreamMap, range)),
                    }
                }
                Output::Element(element @ (Element::PackHeader | Element::PesPacket), range) => {
                    self.pending.push_back(Pending::Element(element, range));
                }
                // Why forget the system header and map reported: the end code
                // closes the Program Stream, so those of a stream after it
                // are new there, though they repeat the ones before.
                Output::ProgramEnd => {
                    self.system_header.clear();
                    self.program_stream_map_version = None;
                    self.pending.push_back(Pending::Ready(Event::ProgramEnd));
                }
                Output::Unsynchronized(range) => {
                    self.pending.push_back(Pending::Unsynchronized(range));
                }
                Output::Unframable(range, error) => {
                    if let Some(error) = error {
                        self.pending.push_back(Pending::Ready(Event::Error(error)));
                    }
                    self.pending.push_back(Pending::Unframable(range));
                }
                Output::Truncated(range) => self.pending.push_back(Pending::Truncated(range)),
            }
        }
        let event = match self.pending.pop_front()? {
            Pending::Ready(event) => event,
            // Why read the element only when polled: the queue holds what it
            // lends as a range, and the element read borrows those bytes. A
            // program stream map is read once more here, after the read that
            // compared its version.
            Pending::Element(element, range) => {
                let bytes = self.framer.bytes(range.clone());
                let read = match element {
                    Element::PackHeader => PackHeader::read(bytes)
                        .map(Event::PackHeader)
                        .map_err(|error| Error::PackHeader { error }),
                    Element::SystemHeader => SystemHeader::read(bytes)
                        .map(Event::SystemHeader)
                        .map_err(|error| Error::SystemHeader { error }),
                    Element::ProgramStreamMap => ProgramStreamMap::read(bytes)
                        .map(Event::ProgramStreamMap)
                        .map_err(|error| Error::ProgramStreamMap { error }),
                    Element::PesPacket => PesPacket::read(bytes)
                        .map(Event::PesPacket)
                        .map_err(|error| Error::PesPacket { error }),
                };
                match read {
                    Ok(event) => event,
                    // Why hand the bytes back after the error: an element that
                    // does not read is still input, and every byte handed
                    // over is reported (ADR 0004).
                    Err(error) => {
                        // Why forget the system header: the one reported no
                        // longer stands, so the next one that reads is news
                        if element == Element::SystemHeader {
                            self.system_header.clear();
                        }
                        self.pending.push_front(Pending::Unframable(range));
                        Event::Error(error)
                    }
                }
            }
            Pending::Unsynchronized(range) => Event::Unsynchronized {
                bytes: self.framer.bytes(range),
            },
            Pending::Unframable(range) => Event::Unframable {
                bytes: self.framer.bytes(range),
            },
            Pending::Truncated(range) => Event::Truncated {
                bytes: self.framer.bytes(range),
            },
        };
        Some(event)
    }

    /// Ends the input at the bytes handed over so far
    ///
    /// The events [`Demuxer::poll_event`] returns next include the bytes of
    /// an element short of its declared length as [`Event::Truncated`], or,
    /// before synchronization, every byte left as [`Event::Unsynchronized`],
    /// so nothing is joined across the end. Poll until `None` before handing
    /// over more input. The synchronization, the system header and the
    /// program stream map version reported stay, so the same demuxer can then
    /// take a stream that goes on.
    pub fn finish(&mut self) {
        self.framer.finish();
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "elements are built and cut at constant offsets well inside their length"
)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::ps::{MPEG_PROGRAM_END_CODE, PackHeader};

    /// A pack header with SCR 0 and `pack_stuffing_length` covering `stuffing`
    fn pack_header_bytes(stuffing: &[u8]) -> Vec<u8> {
        let stuffing_length = u8::try_from(stuffing.len() & 0x07).unwrap();
        let mut bytes = vec![
            0x00,
            0x00,
            0x01,
            0xBA,
            0x44,
            0x00,
            0x04,
            0x00,
            0x04,
            0x01,
            0x00,
            0x00,
            0x03,
            0xF8 | stuffing_length,
        ];
        bytes.extend_from_slice(stuffing);
        bytes
    }

    /// A pack header whose bits after the start code are `'0010'`: an ISO/IEC
    /// 11172-1 pack
    fn iso_11172_pack_bytes() -> Vec<u8> {
        let mut bytes = pack_header_bytes(&[]);
        bytes[4] = (bytes[4] & 0x3F) | 0x20;
        bytes
    }

    fn system_header_bytes() -> Vec<u8> {
        vec![
            0x00, 0x00, 0x01, 0xBB, 0x00, 0x06, 0x80, 0x00, 0x01, 0x00, 0x20, 0x00,
        ]
    }

    fn changed_system_header_bytes() -> Vec<u8> {
        let mut bytes = system_header_bytes();
        bytes[7] = 0x02;
        bytes
    }

    fn pes_packet_bytes(stream_id: u8, payload: &[u8]) -> Vec<u8> {
        let body_length = u16::try_from(payload.len() + 3).unwrap();
        let mut bytes = vec![0x00, 0x00, 0x01, stream_id];
        bytes.extend_from_slice(&body_length.to_be_bytes());
        bytes.extend_from_slice(&[0x80, 0x00, 0x00]);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn psm_bytes(version: u8, current_next_indicator: bool) -> Vec<u8> {
        let stream_map = [0x1B, 0xE0, 0x00, 0x00];
        let mut body = vec![
            (u8::from(current_next_indicator) << 7) | 0x60 | (version & 0x1F),
            0xFF,
            0x00,
            0x00,
        ];
        body.extend_from_slice(&u16::try_from(stream_map.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&stream_map);
        let program_stream_map_length = u16::try_from(body.len() + 4).unwrap();
        let mut bytes = vec![0x00, 0x00, 0x01, 0xBC];
        bytes.extend_from_slice(&program_stream_map_length.to_be_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(&crate::crc::crc_32(&bytes).to_be_bytes());
        bytes
    }

    fn described(event: &Event<'_>) -> String {
        format!("{event:?}")
    }

    /// Every event the demuxer gives for `stream`, as `Debug` prints it, so the
    /// events can outlive their borrow of the demuxer
    fn events_of(demuxer: &mut Demuxer, stream: &[u8]) -> Vec<String> {
        demuxer.handle_input(stream);
        drained(demuxer)
    }

    fn drained(demuxer: &mut Demuxer) -> Vec<String> {
        let mut events = Vec::new();
        while let Some(event) = demuxer.poll_event() {
            events.push(described(&event));
        }
        events
    }

    fn demux(stream: &[u8]) -> Vec<String> {
        events_of(&mut Demuxer::new(), stream)
    }

    fn acquired() -> String {
        described(&Event::SyncAcquired)
    }

    fn pack(bytes: &[u8]) -> String {
        described(&Event::PackHeader(PackHeader::read(bytes).unwrap()))
    }

    fn system_header(bytes: &[u8]) -> String {
        described(&Event::SystemHeader(SystemHeader::read(bytes).unwrap()))
    }

    fn pes_packet(bytes: &[u8]) -> String {
        described(&Event::PesPacket(PesPacket::read(bytes).unwrap()))
    }

    fn program_stream_map(bytes: &[u8]) -> String {
        described(&Event::ProgramStreamMap(
            ProgramStreamMap::read(bytes).unwrap(),
        ))
    }

    fn program_end() -> String {
        described(&Event::ProgramEnd)
    }

    fn unsynchronized(bytes: &[u8]) -> String {
        described(&Event::Unsynchronized { bytes })
    }

    fn unframable(bytes: &[u8]) -> String {
        described(&Event::Unframable { bytes })
    }

    fn truncated(bytes: &[u8]) -> String {
        described(&Event::Truncated { bytes })
    }

    fn error(error: Error) -> String {
        described(&Event::Error(error))
    }

    fn concat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn acquires_sync_on_the_first_pack_header() {
        let pack_bytes = pack_header_bytes(&[]);
        assert_eq!(demux(&pack_bytes), [acquired(), pack(&pack_bytes)]);
    }

    #[test]
    fn reports_leading_garbage_before_the_pack_start_code() {
        let pack_bytes = pack_header_bytes(&[]);
        let garbage = [0xFF, 0x12, 0x00, 0x34];
        assert_eq!(
            demux(&concat(&[&garbage, &pack_bytes])),
            [unsynchronized(&garbage), acquired(), pack(&pack_bytes)]
        );
    }

    #[test]
    fn reports_a_system_header_after_the_pack() {
        let (pack_bytes, system) = (pack_header_bytes(&[]), system_header_bytes());
        assert_eq!(
            demux(&concat(&[&pack_bytes, &system])),
            [acquired(), pack(&pack_bytes), system_header(&system)]
        );
    }

    #[test]
    fn absorbs_a_system_header_repeated_unchanged() {
        let (pack_bytes, system) = (pack_header_bytes(&[]), system_header_bytes());
        assert_eq!(
            demux(&concat(&[&pack_bytes, &system, &system])),
            [acquired(), pack(&pack_bytes), system_header(&system)]
        );
    }

    #[test]
    fn reports_a_system_header_again_when_its_contents_change() {
        let pack_bytes = pack_header_bytes(&[]);
        let (first, second) = (system_header_bytes(), changed_system_header_bytes());
        assert_eq!(
            demux(&concat(&[&pack_bytes, &first, &second])),
            [
                acquired(),
                pack(&pack_bytes),
                system_header(&first),
                system_header(&second),
            ]
        );
    }

    #[test]
    fn reports_consecutive_packs_without_reacquiring() {
        let pack_bytes = pack_header_bytes(&[]);
        assert_eq!(
            demux(&concat(&[&pack_bytes, &pack_bytes])),
            [acquired(), pack(&pack_bytes), pack(&pack_bytes)]
        );
    }

    #[test]
    fn does_not_acquire_on_a_malformed_pack_header() {
        let (malformed, good) = (iso_11172_pack_bytes(), pack_header_bytes(&[]));
        assert_eq!(
            demux(&concat(&[&malformed, &good])),
            [unsynchronized(&malformed), acquired(), pack(&good)]
        );
    }

    #[test]
    fn acquires_across_a_split_feed() {
        let pack_bytes = pack_header_bytes(&[]);
        let (first, second) = pack_bytes.split_at(7);
        let mut demuxer = Demuxer::new();
        assert_eq!(
            (
                events_of(&mut demuxer, first),
                events_of(&mut demuxer, second)
            ),
            (vec![], vec![acquired(), pack(&pack_bytes)])
        );
    }

    #[test]
    fn discards_an_unframed_start_code_and_reports_the_next_pack() {
        let pack_bytes = pack_header_bytes(&[]);
        let unframed = [0x00, 0x00, 0x01, 0x01, 0xAA, 0xBB];
        assert_eq!(
            demux(&concat(&[&pack_bytes, &unframed, &pack_bytes])),
            [
                acquired(),
                pack(&pack_bytes),
                unframable(&unframed),
                pack(&pack_bytes),
            ]
        );
    }

    #[test]
    fn discards_bytes_recovering_from_a_malformed_pack_while_synced() {
        let (good, malformed) = (pack_header_bytes(&[]), iso_11172_pack_bytes());
        assert_eq!(
            demux(&concat(&[&good, &malformed, &good])),
            [
                acquired(),
                pack(&good),
                error(Error::PackHeader {
                    error: crate::ps::pack_header::Error::Iso11172Pack,
                }),
                unframable(&malformed),
                pack(&good),
            ]
        );
    }

    #[test]
    fn discards_each_of_consecutive_unframable_start_codes() {
        let pack_bytes = pack_header_bytes(&[]);
        let slice = [0x00, 0x00, 0x01, 0x01, 0xAA, 0xBB];
        let picture = [0x00, 0x00, 0x01, 0x00, 0xCC, 0xDD];
        assert_eq!(
            demux(&concat(&[&pack_bytes, &slice, &picture, &pack_bytes])),
            [
                acquired(),
                pack(&pack_bytes),
                unframable(&slice),
                unframable(&picture),
                pack(&pack_bytes),
            ]
        );
    }

    #[test]
    fn extracts_a_pes_packet_between_two_packs() {
        let pack_bytes = pack_header_bytes(&[]);
        let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(
            demux(&concat(&[&pack_bytes, &pes, &pack_bytes])),
            [
                acquired(),
                pack(&pack_bytes),
                pes_packet(&pes),
                pack(&pack_bytes),
            ]
        );
    }

    #[test]
    fn waits_for_the_rest_of_a_pes_packet() {
        let pack_bytes = pack_header_bytes(&[]);
        let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
        let (head, tail) = pes.split_at(pes.len() - 2);
        let mut demuxer = Demuxer::new();
        assert_eq!(
            (
                events_of(&mut demuxer, &concat(&[&pack_bytes, head])),
                events_of(&mut demuxer, tail),
            ),
            (vec![acquired(), pack(&pack_bytes)], vec![pes_packet(&pes)])
        );
    }

    #[test]
    fn discards_a_length_unbounded_pes_packet() {
        let pack_bytes = pack_header_bytes(&[]);
        let mut unbounded = pes_packet_bytes(0xE0, &[0xAA, 0xBB]);
        unbounded[4] = 0x00;
        unbounded[5] = 0x00;
        assert_eq!(
            demux(&concat(&[&pack_bytes, &unbounded, &pack_bytes])),
            [
                acquired(),
                pack(&pack_bytes),
                unframable(&unbounded),
                pack(&pack_bytes),
            ]
        );
    }

    #[test]
    fn reports_the_program_end_code() {
        let pack_bytes = pack_header_bytes(&[]);
        assert_eq!(
            demux(&concat(&[&pack_bytes, &MPEG_PROGRAM_END_CODE])),
            [acquired(), pack(&pack_bytes), program_end()]
        );
    }

    #[test]
    fn reacquires_sync_after_a_program_end_code() {
        let pack_bytes = pack_header_bytes(&[]);
        assert_eq!(
            demux(&concat(&[&pack_bytes, &MPEG_PROGRAM_END_CODE, &pack_bytes])),
            [
                acquired(),
                pack(&pack_bytes),
                program_end(),
                acquired(),
                pack(&pack_bytes),
            ]
        );
    }

    #[test]
    fn re_reports_a_system_header_in_a_stream_following_a_program_end() {
        let (pack_bytes, system) = (pack_header_bytes(&[]), system_header_bytes());
        assert_eq!(
            demux(&concat(&[
                &pack_bytes,
                &system,
                &MPEG_PROGRAM_END_CODE,
                &pack_bytes,
                &system,
            ])),
            [
                acquired(),
                pack(&pack_bytes),
                system_header(&system),
                program_end(),
                acquired(),
                pack(&pack_bytes),
                system_header(&system),
            ]
        );
    }

    #[test]
    fn waits_for_the_rest_of_a_system_header() {
        let (pack_bytes, system) = (pack_header_bytes(&[]), system_header_bytes());
        assert_eq!(
            demux(&concat(&[&pack_bytes, &system[..system.len() - 1]])),
            [acquired(), pack(&pack_bytes)]
        );
    }

    #[test]
    fn reports_a_program_stream_map_after_the_pack() {
        let (pack_bytes, psm) = (pack_header_bytes(&[]), psm_bytes(0, true));
        assert_eq!(
            demux(&concat(&[&pack_bytes, &psm])),
            [acquired(), pack(&pack_bytes), program_stream_map(&psm)]
        );
    }

    #[test]
    fn absorbs_a_program_stream_map_resent_at_the_same_version() {
        let (pack_bytes, psm) = (pack_header_bytes(&[]), psm_bytes(0, true));
        assert_eq!(
            demux(&concat(&[&pack_bytes, &psm, &psm])),
            [acquired(), pack(&pack_bytes), program_stream_map(&psm)]
        );
    }

    #[test]
    fn reports_a_program_stream_map_again_when_its_version_changes() {
        let pack_bytes = pack_header_bytes(&[]);
        let (first, second) = (psm_bytes(0, true), psm_bytes(1, true));
        assert_eq!(
            demux(&concat(&[&pack_bytes, &first, &second])),
            [
                acquired(),
                pack(&pack_bytes),
                program_stream_map(&first),
                program_stream_map(&second),
            ]
        );
    }

    #[test]
    fn absorbs_a_next_version_announcement() {
        let pack_bytes = pack_header_bytes(&[]);
        assert_eq!(
            demux(&concat(&[&pack_bytes, &psm_bytes(5, false)])),
            [acquired(), pack(&pack_bytes)]
        );
    }

    #[test]
    fn re_reports_a_program_stream_map_in_a_stream_following_a_program_end() {
        let (pack_bytes, psm) = (pack_header_bytes(&[]), psm_bytes(0, true));
        assert_eq!(
            demux(&concat(&[
                &pack_bytes,
                &psm,
                &MPEG_PROGRAM_END_CODE,
                &pack_bytes,
                &psm,
            ])),
            [
                acquired(),
                pack(&pack_bytes),
                program_stream_map(&psm),
                program_end(),
                acquired(),
                pack(&pack_bytes),
                program_stream_map(&psm),
            ]
        );
    }

    #[test]
    fn waits_for_the_rest_of_a_program_stream_map() {
        let (pack_bytes, psm) = (pack_header_bytes(&[]), psm_bytes(0, true));
        let (head, tail) = psm.split_at(psm.len() - 1);
        let mut demuxer = Demuxer::new();
        assert_eq!(
            (
                events_of(&mut demuxer, &concat(&[&pack_bytes, head])),
                events_of(&mut demuxer, tail),
            ),
            (
                vec![acquired(), pack(&pack_bytes)],
                vec![program_stream_map(&psm)]
            )
        );
    }

    #[test]
    fn reports_a_pes_packet_cut_short_by_the_stream_end() {
        let pack_bytes = pack_header_bytes(&[]);
        let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
        let cut = &pes[..pes.len() - 2];
        let mut demuxer = Demuxer::new();
        let mut events = events_of(&mut demuxer, &concat(&[&pack_bytes, cut]));
        demuxer.finish();
        events.extend(drained(&mut demuxer));
        assert_eq!(events, [acquired(), pack(&pack_bytes), truncated(cut)]);
    }

    #[test]
    fn reports_nothing_when_the_stream_ends_on_a_unit_boundary() {
        let pack_bytes = pack_header_bytes(&[]);
        let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(&concat(&[&pack_bytes, &pes]));
        demuxer.finish();
        assert_eq!(
            drained(&mut demuxer),
            [acquired(), pack(&pack_bytes), pes_packet(&pes)]
        );
    }

    #[test]
    fn reports_the_bytes_held_when_the_stream_ends_before_sync_is_acquired() {
        let pack_bytes = pack_header_bytes(&[]);
        let partial = &pack_bytes[..pack_bytes.len() - 1];
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(partial);
        demuxer.finish();
        assert_eq!(drained(&mut demuxer), [unsynchronized(partial)]);
    }

    #[test]
    fn keeps_synchronization_across_a_stream_end() {
        let pack_bytes = pack_header_bytes(&[]);
        let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(&pack_bytes);
        demuxer.finish();
        let before = drained(&mut demuxer);
        assert_eq!(
            (before, events_of(&mut demuxer, &pes)),
            (vec![acquired(), pack(&pack_bytes)], vec![pes_packet(&pes)])
        );
    }

    #[test]
    fn drops_the_bytes_it_reported_truncated() {
        let pack_bytes = pack_header_bytes(&[]);
        let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
        let (first, rest) = pes.split_at(pes.len() - 5);
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(&concat(&[&pack_bytes, first]));
        demuxer.finish();
        let before = drained(&mut demuxer);
        assert_eq!(
            (before, events_of(&mut demuxer, rest)),
            (
                vec![acquired(), pack(&pack_bytes), truncated(first)],
                vec![unframable(rest)]
            )
        );
    }

    #[test]
    fn discards_a_corrupted_program_stream_map() {
        let pack_bytes = pack_header_bytes(&[]);
        let mut corrupted = psm_bytes(0, true);
        *corrupted.last_mut().unwrap() ^= 0xFF;
        assert_eq!(
            demux(&concat(&[&pack_bytes, &corrupted, &pack_bytes])),
            [
                acquired(),
                pack(&pack_bytes),
                error(Error::ProgramStreamMap {
                    error: ProgramStreamMap::read(&corrupted).unwrap_err(),
                }),
                unframable(&corrupted),
                pack(&pack_bytes),
            ]
        );
    }

    #[test]
    fn reports_why_a_pes_packet_that_does_not_read_was_passed_over() {
        let pack_bytes = pack_header_bytes(&[]);
        let mut broken = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
        broken[6] = 0x40;
        assert_eq!(
            demux(&concat(&[&pack_bytes, &broken, &pack_bytes])),
            [
                acquired(),
                pack(&pack_bytes),
                error(Error::PesPacket {
                    error: PesPacket::read(&broken).unwrap_err(),
                }),
                unframable(&broken),
                pack(&pack_bytes),
            ]
        );
    }

    #[test]
    fn cuts_an_element_that_does_not_read_by_its_declared_length() {
        let pack_bytes = pack_header_bytes(&[]);
        let mut broken = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
        broken[6] = 0x40;
        let after = [0x12, 0x34];
        assert_eq!(
            demux(&concat(&[&pack_bytes, &broken, &after, &pack_bytes])),
            [
                acquired(),
                pack(&pack_bytes),
                error(Error::PesPacket {
                    error: PesPacket::read(&broken).unwrap_err(),
                }),
                unframable(&broken),
                unframable(&after),
                pack(&pack_bytes),
            ]
        );
    }

    #[test]
    fn bytes_that_cannot_begin_a_start_code_are_unframable_without_waiting_for_four() {
        let pack_bytes = pack_header_bytes(&[]);
        for tail in [&[0xFF][..], &[0xFF, 0x12, 0x34]] {
            let mut demuxer = Demuxer::new();
            demuxer.handle_input(&concat(&[&pack_bytes, tail]));
            demuxer.finish();
            assert_eq!(
                drained(&mut demuxer),
                [acquired(), pack(&pack_bytes), unframable(tail)],
                "{tail:02x?}"
            );
        }
    }

    #[test]
    fn the_start_of_a_start_code_left_at_the_end_is_truncated() {
        let pack_bytes = pack_header_bytes(&[]);
        for tail in [&[0x00][..], &[0x00, 0x00], &[0x00, 0x00, 0x01]] {
            let mut demuxer = Demuxer::new();
            demuxer.handle_input(&concat(&[&pack_bytes, tail]));
            demuxer.finish();
            assert_eq!(
                drained(&mut demuxer),
                [acquired(), pack(&pack_bytes), truncated(tail)],
                "{tail:02x?}"
            );
        }
    }

    #[test]
    fn an_iso_11172_pack_is_reported_without_waiting_for_the_length_of_a_13818_pack() {
        let pack_bytes = pack_header_bytes(&[]);
        let iso_11172 = [
            0x00, 0x00, 0x01, 0xBA, 0x21, 0x00, 0x01, 0x00, 0x01, 0x80, 0x00, 0x01,
        ];
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(&concat(&[&pack_bytes, &iso_11172]));
        demuxer.finish();
        assert_eq!(
            drained(&mut demuxer),
            [
                acquired(),
                pack(&pack_bytes),
                error(Error::PackHeader {
                    error: crate::ps::pack_header::Error::Iso11172Pack,
                }),
                unframable(&iso_11172),
            ]
        );
    }
}
