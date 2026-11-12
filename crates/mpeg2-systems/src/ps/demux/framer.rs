//! Framing of the elements of a Program Stream by their start codes
//!
//! The framer finds the first pack header whose length it can tell, then
//! cuts each pack header, system header, program stream map and PES packet
//! off the input by its declared length, until an `MPEG_program_end_code`
//! drops the stream. What it cannot cut is passed over to the next start
//! code. It reads no element: the demuxer reads what is cut. Every byte is
//! cut into some output, as a range of the buffer.

use core::ops::Range;

use super::Error;
use crate::framing::{Buffer, Decoder, Skipped};
use crate::pes::{self, EncodedLen, StreamId};
use crate::ps::start_code::{
    PACK_STREAM_ID, PROGRAM_END_STREAM_ID, PROGRAM_STREAM_MAP_STREAM_ID, START_CODE_PREFIX,
    SYSTEM_HEADER_STREAM_ID,
};
use crate::ps::{MPEG_PROGRAM_END_CODE, PACK_START_CODE, pack_header, psm, system_header};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Searching {
        /// Bytes after the cut ones that the search walked past and has not
        /// reported yet
        skipped: Skipped,
    },
    Synced,
    /// Synchronized, and passing over bytes that are no element, up to the
    /// next start code the input has not brought yet
    Passing {
        /// Bytes after the cut ones in which no start code begins
        // Why not rescan from the front instead: the bytes wait for the next
        // start code, so rescanning them on every step costs time quadratic
        // in the run.
        scanned: usize,
        /// Why the bytes are passed over, when a pack header is one this
        /// crate does not read
        // Why not classify the front again on each step instead: the verdict
        // is known when passing starts, and the bytes after it do not change it.
        error: Option<Error>,
    },
}

/// What the search saw where it stands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Search {
    /// This many bytes come before the next possible start code
    Garbage(usize),
    /// A pack header this long, held whole
    Pack(usize),
    /// A start code that is not a pack header this crate reads
    FalseStart,
    /// Too few bytes to tell
    NeedMoreData,
}

/// What the synchronized framer saw at the front of the input
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Synced {
    /// An element this long, held whole
    Element(Element, usize),
    /// A pack header this crate does not read, and why
    Broken(Error),
    /// A PES packet whose `PES_packet_length` is 0
    UnboundedPes,
    /// `MPEG_program_end_code`
    ProgramEnd,
    /// A start code the framer does not cut, or bytes that are no start code
    Unknown,
    /// Too few bytes to tell
    NeedMoreData,
}

/// An element the framer cuts, for the demuxer to read
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Element {
    PackHeader,
    SystemHeader,
    ProgramStreamMap,
    PesPacket,
}

/// A unit [`ProgramStreamElements`] cut, with the range of the buffer it lies
/// in
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Output {
    /// The first pack header whose length the framer can tell, which
    /// synchronizes it
    SyncAcquired(Range<usize>),
    /// An element cut by its declared length
    Element(Element, Range<usize>),
    /// `MPEG_program_end_code`, which drops synchronization
    ProgramEnd(Range<usize>),
    /// Bytes the search walked past
    Unsynchronized(Range<usize>),
    /// Bytes that could not be cut while synchronized, up to the next start
    /// code, and why when a pack header is one this crate does not read
    Unframable(Range<usize>, Option<Error>),
    /// At the end of input, the bytes of an element short of its length
    Truncated(Range<usize>),
}

/// What a cut yields, before the bytes it takes are known as a range
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cut {
    SyncAcquired,
    Element(Element),
    ProgramEnd,
    Unsynchronized,
    Unframable(Option<Error>),
    Truncated,
}

struct Transition {
    next_state: State,
    /// Bytes cut off the front, which the output lends
    consume: usize,
    then: Then,
}

/// What follows a transition
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    /// The cut goes on, observing again
    Continue,
    /// The cut ends with no output: the input holds too few bytes
    NeedMoreData,
    /// The cut ends with this output, over the bytes cut
    Output(Cut),
}

/// The elements of a Program Stream, cut by their start codes
#[derive(Debug, Clone)]
pub(super) struct ProgramStreamElements {
    state: State,
}

impl ProgramStreamElements {
    pub(super) const fn new() -> Self {
        Self {
            state: State::Searching {
                skipped: Skipped::new(),
            },
        }
    }

    /// Cuts the next unit; `eof` tells that no input follows the bytes held
    fn cut(&mut self, buffer: &mut Buffer, eof: bool) -> Option<Output> {
        loop {
            let transition = self.transition(buffer.unconsumed(), eof);
            self.state = transition.next_state;
            let range = buffer.split_to(transition.consume);
            let cut = match transition.then {
                Then::Continue => continue,
                Then::NeedMoreData => return None,
                Then::Output(cut) => cut,
            };
            return Some(match cut {
                Cut::SyncAcquired => Output::SyncAcquired(range),
                Cut::Element(element) => Output::Element(element, range),
                Cut::ProgramEnd => Output::ProgramEnd(range),
                Cut::Unsynchronized => Output::Unsynchronized(range),
                Cut::Unframable(error) => Output::Unframable(range, error),
                Cut::Truncated => Output::Truncated(range),
            });
        }
    }

    fn transition(&self, rest: &[u8], eof: bool) -> Transition {
        match self.state {
            // Why report before observing again: the report is capped, so a
            // stream that never synchronizes hands its bytes back in bounded
            // pieces instead of holding all of them.
            State::Searching { mut skipped } if skipped.is_full() => report_skipped(&mut skipped),
            State::Searching { mut skipped } => {
                let searched = rest.get(skipped.count()..).unwrap_or_default();
                match (observe_search(searched), eof) {
                    // Why cap the garbage taken at what the report has room
                    // for: one chunk can carry far more garbage than the
                    // limit, and taking it all would make one report exceed
                    // the bound.
                    (Search::Garbage(length), _) => {
                        skipped.skip(length);
                        Transition {
                            next_state: State::Searching { skipped },
                            consume: 0,
                            then: Then::Continue,
                        }
                    }
                    // Why report the run walked past before acquiring: one
                    // cut carries one output, so the run goes out here and the
                    // next cut acquires on the same bytes.
                    (Search::Pack(..), _) if skipped.count() > 0 => report_skipped(&mut skipped),
                    (Search::Pack(length), _) => Transition {
                        next_state: State::Synced,
                        consume: length,
                        then: Then::Output(Cut::SyncAcquired),
                    },
                    // Why one byte, not the whole start code: a real pack
                    // start code may overlap the bytes of the false one.
                    (Search::FalseStart, _) => {
                        skipped.skip(1);
                        Transition {
                            next_state: State::Searching { skipped },
                            consume: 0,
                            then: Then::Continue,
                        }
                    }
                    // Why keep the run rather than report it on waiting: the
                    // events would then depend on where the input was cut.
                    (Search::NeedMoreData, false) => Transition {
                        next_state: State::Searching { skipped },
                        consume: 0,
                        then: Then::NeedMoreData,
                    },
                    (Search::NeedMoreData, true) => {
                        let length = skipped.report_rest(rest.len());
                        Transition {
                            next_state: State::Searching { skipped },
                            consume: length,
                            then: if length > 0 {
                                Then::Output(Cut::Unsynchronized)
                            } else {
                                Then::NeedMoreData
                            },
                        }
                    }
                }
            }
            State::Synced => match (observe_synced(rest), eof) {
                (Synced::Element(element, length), _) => Transition {
                    next_state: State::Synced,
                    consume: length,
                    then: Then::Output(Cut::Element(element)),
                },
                // Why to the next start code: a pack header this crate does
                // not read states no length it can trust.
                (Synced::Broken(error), _) => pass_over(rest, Some(error), eof, 1),
                // Why not read to the next start code: PES_packet_length 0 is
                // allowed only for video in a Transport Stream (§2.4.3.7), so
                // in a Program Stream the packet is broken.
                (Synced::UnboundedPes | Synced::Unknown, _) => pass_over(rest, None, eof, 1),
                // Why back to searching: the end code ends the Program
                // Stream, so a stream after it synchronizes on its own first
                // pack header.
                (Synced::ProgramEnd, _) => Transition {
                    next_state: State::Searching {
                        skipped: Skipped::new(),
                    },
                    consume: MPEG_PROGRAM_END_CODE.len(),
                    then: Then::Output(Cut::ProgramEnd),
                },
                (Synced::NeedMoreData, false) => Transition {
                    next_state: State::Synced,
                    consume: 0,
                    then: Then::NeedMoreData,
                },
                // Why keep the state, unlike the end code: the end of input
                // is not the end of the Program Stream, so a caller that goes
                // on hands over bytes on the same footing.
                (Synced::NeedMoreData, true) if rest.is_empty() => Transition {
                    next_state: State::Synced,
                    consume: 0,
                    then: Then::NeedMoreData,
                },
                (Synced::NeedMoreData, true) => Transition {
                    next_state: State::Synced,
                    consume: rest.len(),
                    then: Then::Output(Cut::Truncated),
                },
            },
            State::Passing { scanned, error } => pass_over(rest, error, eof, scanned),
        }
    }
}

impl Decoder for ProgramStreamElements {
    type Item = Output;

    fn decode(&mut self, buffer: &mut Buffer) -> Option<Output> {
        self.cut(buffer, false)
    }

    fn decode_eof(&mut self, buffer: &mut Buffer) -> Option<Output> {
        self.cut(buffer, true)
    }
}

/// Reports the run the search walked past, from the front
fn report_skipped(skipped: &mut Skipped) -> Transition {
    Transition {
        consume: skipped.report(),
        next_state: State::Searching { skipped: *skipped },
        then: Then::Output(Cut::Unsynchronized),
    }
}

/// Passes over `rest` up to its next start code, scanning from `scanned`, at
/// least 1; `eof` tells that no input follows the bytes held
///
/// Waits while no start code has come within the report limit and more input
/// may follow, so where the input is cut does not change what is reported.
fn pass_over(rest: &[u8], error: Option<Error>, eof: bool, scanned: usize) -> Transition {
    // Why a start code may begin this many bytes before the last held: it
    // completes with the bytes the next input brings.
    let incomplete = START_CODE_PREFIX.len().saturating_sub(1);
    // Why look no further than the report limit: a run is reported in pieces
    // no longer than it, as the search reports what it walks past.
    let held = rest.len().min(Skipped::LIMIT.saturating_add(incomplete));
    let region = rest.get(..held).unwrap_or_default();
    let length = match (scan_start_code(region, scanned), eof) {
        (Scan::Found(index), _) => Some(index.min(Skipped::LIMIT)),
        // Why the limit once held past it: no start code begins within it, as
        // a prefix starting there would have been found whole.
        _ if held > Skipped::LIMIT => Some(Skipped::LIMIT),
        (Scan::Partial(index), true) => Some(index),
        (Scan::NotFound, true) => Some(held),
        (Scan::Partial(_) | Scan::NotFound, false) => None,
    };
    let Some(length) = length else {
        return Transition {
            next_state: State::Passing {
                // Why at least 1: the front is the start code that could not
                // be cut, so the scan always passes over at least one byte.
                scanned: held.saturating_sub(incomplete).max(1),
                error,
            },
            consume: 0,
            then: Then::NeedMoreData,
        };
    };
    Transition {
        next_state: State::Synced,
        consume: length,
        then: Then::Output(Cut::Unframable(error)),
    }
}

/// Classifies the bytes the search stands at
fn observe_search(bytes: &[u8]) -> Search {
    if bytes.is_empty() {
        return Search::NeedMoreData;
    }
    match scan_start_code(bytes, 0) {
        Scan::Found(0) => match bytes.get(PACK_START_CODE.len().saturating_sub(1)) {
            None => Search::NeedMoreData,
            Some(&PACK_STREAM_ID) => match pack_header::encoded_len(bytes) {
                pack_header::EncodedLen::Bounded(length) if bytes.len() >= length => {
                    Search::Pack(length)
                }
                pack_header::EncodedLen::Bounded(_) | pack_header::EncodedLen::NeedMoreData => {
                    Search::NeedMoreData
                }
                pack_header::EncodedLen::Refused(_) => Search::FalseStart,
            },
            // Why pass over another start code: only a pack header can
            // synchronize the search (§2.5.3.3).
            Some(_) => Search::FalseStart,
        },
        Scan::Partial(0) => Search::NeedMoreData,
        Scan::Found(length) | Scan::Partial(length) => Search::Garbage(length),
        Scan::NotFound => Search::Garbage(bytes.len()),
    }
}

/// Classifies the front of the input while synchronized
fn observe_synced(bytes: &[u8]) -> Synced {
    let Some(&[first, second, third, stream_id]) = bytes.first_chunk::<4>() else {
        // Why wait only for bytes that can begin a start code: others are
        // unframable however many follow, and the end of input would report
        // them as an element cut short.
        return if START_CODE_PREFIX.starts_with(bytes) {
            Synced::NeedMoreData
        } else {
            Synced::Unknown
        };
    };
    if [first, second, third] != START_CODE_PREFIX {
        return Synced::Unknown;
    }
    match stream_id {
        PACK_STREAM_ID => match pack_header::encoded_len(bytes) {
            pack_header::EncodedLen::Bounded(length) => {
                observe_element(bytes, Element::PackHeader, Some(length))
            }
            pack_header::EncodedLen::NeedMoreData => Synced::NeedMoreData,
            pack_header::EncodedLen::Refused(error) => Synced::Broken(Error::PackHeader { error }),
        },
        PROGRAM_END_STREAM_ID => Synced::ProgramEnd,
        SYSTEM_HEADER_STREAM_ID => observe_element(
            bytes,
            Element::SystemHeader,
            system_header::encoded_len(bytes),
        ),
        PROGRAM_STREAM_MAP_STREAM_ID => {
            observe_element(bytes, Element::ProgramStreamMap, psm::encoded_len(bytes))
        }
        _ if StreamId::from(stream_id).is_pes_packet() => match pes::encoded_len(bytes) {
            EncodedLen::NeedMoreData => Synced::NeedMoreData,
            EncodedLen::Unbounded => Synced::UnboundedPes,
            EncodedLen::Bounded(length) => observe_element(bytes, Element::PesPacket, Some(length)),
        },
        _ => Synced::Unknown,
    }
}

/// Cuts the `element` of `length` bytes that `bytes` start with, once they
/// hold it whole
fn observe_element(bytes: &[u8], element: Element, length: Option<usize>) -> Synced {
    match length {
        Some(length) if bytes.len() >= length => Synced::Element(element, length),
        _ => Synced::NeedMoreData,
    }
}

/// Where the next start code is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// A whole `packet_start_code_prefix` starts at this index
    Found(usize),
    /// The bytes from this index begin a prefix the next input may complete
    Partial(usize),
    /// Neither
    NotFound,
}

/// Locates the next `packet_start_code_prefix` at or after `from`
fn scan_start_code(bytes: &[u8], from: usize) -> Scan {
    let Some(region) = bytes.get(from..) else {
        return Scan::NotFound;
    };
    if let Some(offset) = region
        .windows(START_CODE_PREFIX.len())
        .position(|window| window == START_CODE_PREFIX)
    {
        return Scan::Found(from.saturating_add(offset));
    }
    let Some(partial) = (1..START_CODE_PREFIX.len()).rev().find(|&length| {
        START_CODE_PREFIX
            .get(..length)
            .is_some_and(|prefix| region.ends_with(prefix))
    }) else {
        return Scan::NotFound;
    };
    Scan::Partial(from.saturating_add(region.len().saturating_sub(partial)))
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::framing::tests::{Reported, check};

    impl Reported for Output {
        fn range(&self) -> Option<Range<usize>> {
            match self {
                Self::SyncAcquired(range)
                | Self::Element(_, range)
                | Self::ProgramEnd(range)
                | Self::Unsynchronized(range)
                | Self::Unframable(range, _)
                | Self::Truncated(range) => Some(range.clone()),
            }
        }

        fn is_passed_over(&self) -> bool {
            matches!(self, Self::Unsynchronized(_) | Self::Unframable(..))
        }
    }

    const PACK_HEADER: [u8; 14] = [
        0x00, 0x00, 0x01, 0xBA, 0x44, 0x00, 0x04, 0x00, 0x04, 0x01, 0x00, 0x00, 0x03, 0xF8,
    ];
    const PES_PACKET: [u8; 12] = [
        0x00, 0x00, 0x01, 0xE0, 0x00, 0x06, 0x80, 0x00, 0x00, 0xAA, 0xBB, 0xCC,
    ];

    #[test]
    fn the_framing_of_a_program_stream_follows_the_rules_of_every_framing() {
        let synced = |middle: &[u8]| [&PACK_HEADER[..], &PES_PACKET, middle, &PACK_HEADER].concat();
        let mut iso_11172_pack = PACK_HEADER;
        iso_11172_pack[4] = 0x21;
        let streams: [Vec<u8>; 7] = [
            synced(&[]),
            vec![0xFF; 3 * Skipped::LIMIT + 100],
            [&vec![0xFF; Skipped::LIMIT - 1][..], &PACK_HEADER[..10]].concat(),
            synced(&vec![0xFF; 2 * Skipped::LIMIT + 7]),
            synced(&[&iso_11172_pack[..], &[0x00, 0x00, 0x02, 0x99]].concat()),
            synced(&[0x00, 0x00, 0x01, 0xB9]),
            [&PACK_HEADER[..], &PES_PACKET[..7]].concat(),
        ];
        for stream in &streams {
            check(&ProgramStreamElements::new(), stream);
        }
    }
}
