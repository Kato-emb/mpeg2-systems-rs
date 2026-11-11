use alloc::vec;
use alloc::vec::Vec;

use super::gap::Line;
use super::{Pack, Rate};
use crate::ps::mux::csps::Constraints;
use crate::time::{SystemTimeClock, Timestamp};

/// A `program_mux_rate` of 1 260 000 bytes a second
const RATE: u32 = 25_200;

/// Ticks of the STC that 7 bytes take to arrive at [`RATE`]
const TICKS_OF_SEVEN_BYTES: u64 = 150;

fn timestamp(value: u64) -> Timestamp {
    Timestamp::try_from(value).unwrap()
}

#[test]
fn the_padding_fills_the_bytes_missing_exactly_in_packs_of_at_most_the_longest_from_a_pack_header_on()
 {
    let before = Pack::after(None, SystemTimeClock::first(timestamp(90_000)), 100, 1);
    let lengths = |bytes: u64| {
        let time = before
            .at
            .add_ticks((100 + bytes) * TICKS_OF_SEVEN_BYTES / 7);
        Rate::fixed(RATE, 2_048, Constraints::none())
            .gap(before, time, 0)
            .map(|padding| {
                (
                    padding.iter().map(|pack| pack.length).collect::<Vec<_>>(),
                    padding.bytes(),
                    padding.last().next_offset() - 100,
                )
            })
    };

    // Below a pack header, none; from it on, the bytes missing
    assert_eq!(
        [0, 1, 13, 14, 2_048, 2_049, 2_061, 2_062, 4_097].map(lengths),
        [
            None,
            None,
            None,
            Some((vec![14], 14, 14)),
            Some((vec![2_048], 2_048, 2_048)),
            Some((vec![2_035, 14], 2_049, 2_049)),
            Some((vec![2_047, 14], 2_061, 2_061)),
            Some((vec![2_048, 14], 2_062, 2_062)),
            Some((vec![2_048, 2_035, 14], 4_097, 4_097)),
        ]
    );
}

#[test]
fn the_line_is_taken_in_whole_ticks_rounded_down_past_the_width_of_u64() {
    // 2^40 bytes: 27 000 000 × 2^40 is past u64
    let far = 1_u64 << 40;
    let far_ticks = u64::try_from(u128::from(far) * 150 / 7).unwrap();

    assert_eq!(
        (
            [0, 1, 6, 7, far].map(|bytes| Line::new(RATE).ticks(bytes)),
            Line::new(1).ticks(u64::MAX),
            [128, 129, 150, 151].map(|ticks| Line::new(RATE).bytes(ticks)),
        ),
        ([0, 21, 128, 150, far_ticks], u64::MAX, [6, 7, 7, 8])
    );
}
