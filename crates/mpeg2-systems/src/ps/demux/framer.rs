//! Framing of the elements of a Program Stream by their start codes
//!
//! The framer finds the first pack header whose length it can tell, then
//! cuts each pack header, system header, program stream map and PES packet
//! off the input by its declared length, until an `MPEG_program_end_code`
//! drops the stream. What it cannot cut is passed over to the next start
//! code. It reads no element: the demuxer reads what is cut. Every byte is
//! reported in some output, as a range of the input buffer.

use core::ops::Range;

use super::Error;
use crate::input_buffer::{End, Input, InputBuffer};
use crate::pes::{self, EncodedLen, StreamId};
use crate::ps::start_code::{
    PACK_STREAM_ID, PROGRAM_END_STREAM_ID, PROGRAM_STREAM_MAP_STREAM_ID, START_CODE_PREFIX,
    SYSTEM_HEADER_STREAM_ID,
};
use crate::ps::{MPEG_PROGRAM_END_CODE, PACK_START_CODE, pack_header, psm, system_header};

/// Bytes walked past that force an [`Output::Unsynchronized`] out
///
/// A longer run is reported in several outputs, which bounds what a stream
/// that never synchronizes holds while still handing every byte back.
const UNSYNCHRONIZED_REPORT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Searching {
        /// Bytes after the consumed ones that the search walked past and has
        /// not reported yet
        skipped: usize,
    },
    Synced,
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

/// Output of one [`PackFramer::step`]
///
/// A range indexes the input through [`PackFramer::bytes`] and stays valid
/// until the next [`PackFramer::compact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Output {
    /// The input holds too few bytes to decide
    NeedMoreData,
    /// The first pack header whose length the framer can tell, which
    /// synchronizes it
    SyncAcquired(Range<usize>),
    /// An element cut by its declared length
    Element(Element, Range<usize>),
    /// `MPEG_program_end_code`, which drops synchronization
    ProgramEnd,
    /// Bytes the search walked past
    Unsynchronized(Range<usize>),
    /// Bytes that could not be cut while synchronized, up to the next start
    /// code, and why when a pack header is one this crate does not read
    Unframable(Range<usize>, Option<Error>),
    /// At the end of input, the bytes of an element short of its length
    Truncated(Range<usize>),
    /// Every byte before the end [`PackFramer::finish`] marked is reported
    EndOfInput,
}

struct Transition {
    next_state: State,
    consume: usize,
    /// `None` keeps [`PackFramer::step`] stepping
    output: Option<Output>,
}

/// Start code state machine over the input buffer
#[derive(Debug, Clone)]
pub(super) struct PackFramer {
    state: State,
    input: InputBuffer,
}

impl PackFramer {
    pub(super) const fn new() -> Self {
        Self {
            state: State::Searching { skipped: 0 },
            input: InputBuffer::new(),
        }
    }

    /// Appends input
    ///
    /// # Panics
    /// Panics if called after [`PackFramer::finish`] before
    /// [`PackFramer::step`] has returned [`Output::EndOfInput`].
    pub(super) fn extend(&mut self, bytes: &[u8]) {
        self.input.extend(bytes);
    }

    /// Drops the bytes reported or cut, once no output's range points at
    /// them, if they are no fewer than the bytes left
    pub(super) fn compact(&mut self) {
        self.input.compact();
    }

    /// Ends the input at the bytes handed over so far
    ///
    /// The steps that follow report what is left: an element short of its
    /// length as [`Output::Truncated`], or what the search walked past as
    /// [`Output::Unsynchronized`]. The state stays, so input extended once
    /// the end is reported goes on synchronized.
    pub(super) fn finish(&mut self) {
        self.input.finish();
    }

    /// Returns the bytes of a range an [`Output`] carries
    pub(super) fn bytes(&self, range: Range<usize>) -> &[u8] {
        self.input.get(range)
    }

    /// Steps the state machine until it has something to report
    pub(super) fn step(&mut self) -> Output {
        loop {
            if self.input.reach_end() == End::Reached {
                return Output::EndOfInput;
            }
            let transition = self.transition();
            self.state = transition.next_state;
            self.input.consume(transition.consume);
            if let Some(output) = transition.output {
                return output;
            }
        }
    }

    fn transition(&self) -> Transition {
        let consumed = self.input.consumed();
        let rest = self.input.from(consumed);
        let input = self.input.input();
        match self.state {
            // Why report before observing again: the report is capped, so a
            // stream that never synchronizes hands its bytes back in bounded
            // pieces instead of holding all of them.
            State::Searching { skipped } if skipped >= UNSYNCHRONIZED_REPORT_LIMIT => {
                report_skipped(consumed, skipped)
            }
            State::Searching { skipped } => {
                let searched = rest.get(skipped..).unwrap_or_default();
                match (observe_search(searched), input) {
                    // Why cap the garbage taken at what the report has room
                    // for: one chunk can carry far more garbage than the
                    // limit, and taking it all would make one report exceed
                    // the bound.
                    (Search::Garbage(length), _) => Transition {
                        next_state: State::Searching {
                            skipped: skipped.saturating_add(
                                length.min(UNSYNCHRONIZED_REPORT_LIMIT.saturating_sub(skipped)),
                            ),
                        },
                        consume: 0,
                        output: None,
                    },
                    // Why report the run walked past before acquiring: one
                    // step carries one output, so the run goes out here and
                    // the next step acquires on the same bytes.
                    (Search::Pack(..), _) if skipped > 0 => report_skipped(consumed, skipped),
                    (Search::Pack(length), _) => Transition {
                        next_state: State::Synced,
                        consume: length,
                        output: Some(Output::SyncAcquired(
                            consumed..consumed.saturating_add(length),
                        )),
                    },
                    // Why one byte, not the whole start code: a real pack
                    // start code may overlap the bytes of the false one.
                    (Search::FalseStart, _) => Transition {
                        next_state: State::Searching {
                            skipped: skipped.saturating_add(1),
                        },
                        consume: 0,
                        output: None,
                    },
                    // Why keep the run rather than report it on waiting: the
                    // events would then depend on where the input was cut.
                    (Search::NeedMoreData, Input::Continues) => Transition {
                        next_state: State::Searching { skipped },
                        consume: 0,
                        output: Some(Output::NeedMoreData),
                    },
                    // Why no more than the report limit at the end too: the
                    // bound on a report holds wherever the input ends.
                    (Search::NeedMoreData, Input::Ends) => {
                        let length = rest.len().min(UNSYNCHRONIZED_REPORT_LIMIT);
                        Transition {
                            next_state: State::Searching { skipped: 0 },
                            consume: length,
                            output: Some(Output::Unsynchronized(
                                consumed..consumed.saturating_add(length),
                            )),
                        }
                    }
                }
            }
            State::Synced => match (observe_synced(rest), input) {
                (Synced::Element(element, length), _) => Transition {
                    next_state: State::Synced,
                    consume: length,
                    output: Some(Output::Element(
                        element,
                        consumed..consumed.saturating_add(length),
                    )),
                },
                // Why to the next start code: a pack header this crate does
                // not read states no length it can trust.
                (Synced::Broken(error), _) => unframable(rest, consumed, Some(error)),
                // Why not read to the next start code: PES_packet_length 0 is
                // allowed only for video in a Transport Stream (§2.4.3.7), so
                // in a Program Stream the packet is broken.
                (Synced::UnboundedPes | Synced::Unknown, _) => unframable(rest, consumed, None),
                // Why back to searching: the end code ends the Program
                // Stream, so a stream after it synchronizes on its own first
                // pack header.
                (Synced::ProgramEnd, _) => Transition {
                    next_state: State::Searching { skipped: 0 },
                    consume: MPEG_PROGRAM_END_CODE.len(),
                    output: Some(Output::ProgramEnd),
                },
                (Synced::NeedMoreData, Input::Continues) => Transition {
                    next_state: State::Synced,
                    consume: 0,
                    output: Some(Output::NeedMoreData),
                },
                // Why keep the state, unlike the end code: the end of input
                // is not the end of the Program Stream, so a caller that goes
                // on hands over bytes on the same footing.
                (Synced::NeedMoreData, Input::Ends) => Transition {
                    next_state: State::Synced,
                    consume: rest.len(),
                    output: Some(Output::Truncated(consumed..self.input.len())),
                },
            },
        }
    }
}

/// Reports the run the search walked past, which starts at `consumed`
fn report_skipped(consumed: usize, skipped: usize) -> Transition {
    Transition {
        next_state: State::Searching { skipped: 0 },
        consume: skipped,
        output: Some(Output::Unsynchronized(
            consumed..consumed.saturating_add(skipped),
        )),
    }
}

/// Passes over `rest`, which starts at `consumed`, up to its next start code
fn unframable(rest: &[u8], consumed: usize, error: Option<Error>) -> Transition {
    // Why scan from the second byte: the front is the start code that could
    // not be cut, so the scan always passes over at least one byte.
    let length = match scan_start_code(rest, 1) {
        Scan::Found(index) | Scan::Partial(index) => index,
        Scan::NotFound => rest.len(),
    };
    Transition {
        next_state: State::Synced,
        consume: length,
        output: Some(Output::Unframable(
            consumed..consumed.saturating_add(length),
            error,
        )),
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

    /// A chunk short enough that many come before the report limit
    const CHUNK_LENGTH: usize = 1024;

    #[test]
    fn a_search_that_never_synchronizes_hands_every_byte_back_in_bounded_pieces() {
        let chunks = UNSYNCHRONIZED_REPORT_LIMIT / CHUNK_LENGTH * 3;
        let mut framer = PackFramer::new();
        let (mut pieces, mut others) = (Vec::new(), Vec::new());
        let mut take = |framer: &mut PackFramer| loop {
            match framer.step() {
                Output::NeedMoreData | Output::EndOfInput => break,
                Output::Unsynchronized(range) => pieces.push(range.len()),
                other @ (Output::SyncAcquired(_)
                | Output::Element(..)
                | Output::ProgramEnd
                | Output::Unframable(..)
                | Output::Truncated(_)) => others.push(other),
            }
        };
        for _ in 0..chunks {
            framer.extend(&vec![0xFF; CHUNK_LENGTH]);
            take(&mut framer);
        }
        framer.finish();
        take(&mut framer);
        assert_eq!(
            (
                others,
                pieces.len() > 1,
                pieces
                    .iter()
                    .all(|&length| length <= UNSYNCHRONIZED_REPORT_LIMIT),
                pieces.iter().sum::<usize>(),
            ),
            (vec![], true, true, chunks * CHUNK_LENGTH)
        );
    }

    #[test]
    fn a_search_the_end_cuts_short_hands_what_is_left_back_in_bounded_pieces() {
        let part_of_a_pack_header = [0x00, 0x00, 0x01, 0xBA, 0x44, 0x00, 0x04, 0x00, 0x04, 0x01];
        let garbage = vec![0xFF; UNSYNCHRONIZED_REPORT_LIMIT - 1];
        let mut framer = PackFramer::new();
        framer.extend(&garbage);
        framer.extend(&part_of_a_pack_header);
        framer.finish();
        let (mut pieces, mut others) = (Vec::new(), Vec::new());
        loop {
            match framer.step() {
                Output::NeedMoreData | Output::EndOfInput => break,
                Output::Unsynchronized(range) => pieces.push(range.len()),
                other @ (Output::SyncAcquired(_)
                | Output::Element(..)
                | Output::ProgramEnd
                | Output::Unframable(..)
                | Output::Truncated(_)) => others.push(other),
            }
        }
        assert_eq!(
            (
                others,
                pieces
                    .iter()
                    .all(|&length| length <= UNSYNCHRONIZED_REPORT_LIMIT),
                pieces.iter().sum::<usize>(),
            ),
            (vec![], true, garbage.len() + part_of_a_pack_header.len())
        );
    }
}
