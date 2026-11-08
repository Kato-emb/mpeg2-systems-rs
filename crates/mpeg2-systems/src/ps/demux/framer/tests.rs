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
            | Output::PackHeader(_)
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
