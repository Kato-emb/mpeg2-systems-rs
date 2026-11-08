//! Demuxing a Program Stream: input of any length in, events out

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::ops::Range;

use crate::pes::PesPacket;
use crate::ps::{ProgramStreamMap, SystemHeader};
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
                Output::SyncAcquired(header) => {
                    self.pending.push_back(Pending::Ready(Event::SyncAcquired));
                    self.pending
                        .push_back(Pending::Ready(Event::PackHeader(header)));
                }
                Output::PackHeader(header) => self
                    .pending
                    .push_back(Pending::Ready(Event::PackHeader(header))),
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
                    if let Ok(map) = map
                        && map.current_next_indicator
                        && self.program_stream_map_version != Some(map.program_stream_map_version)
                    {
                        self.program_stream_map_version = Some(map.program_stream_map_version);
                        self.pending
                            .push_back(Pending::Element(Element::ProgramStreamMap, range));
                    }
                }
                Output::Element(Element::PesPacket, range) => self
                    .pending
                    .push_back(Pending::Element(Element::PesPacket, range)),
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
            // Why read the element again when polled: the queue holds what it
            // lends as a range, and the element read borrows those bytes; the
            // framer read the same bytes when it cut them, so the reading
            // succeeds.
            Pending::Element(element, range) => {
                let bytes = self.framer.bytes(range);
                match element {
                    Element::SystemHeader => SystemHeader::read(bytes).map_or_else(
                        |error| Event::Error(Error::SystemHeader { error }),
                        Event::SystemHeader,
                    ),
                    Element::ProgramStreamMap => ProgramStreamMap::read(bytes).map_or_else(
                        |error| Event::Error(Error::ProgramStreamMap { error }),
                        Event::ProgramStreamMap,
                    ),
                    Element::PesPacket => PesPacket::read(bytes).map_or_else(
                        |error| Event::Error(Error::PesPacket { error }),
                        Event::PesPacket,
                    ),
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
mod tests;
