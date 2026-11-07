//! DSM trick mode: the trick mode of a video stream, in the optional PES
//! header

use core::fmt;

/// Trick mode a video stream is in, and the five bits whose meaning it sets
///
/// `trick_mode_control` and the fields after it, ISO/IEC 13818-1 §2.4.3.7,
/// Table 2-24
///
/// The byte is kept whole, so a reserved `trick_mode_control` keeps its
/// `reserved` bits; the accessors read a field only under the modes that
/// have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DsmTrickMode(pub(super) u8);

impl DsmTrickMode {
    /// Returns the byte, `trick_mode_control` in its three high bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns the trick mode
    #[must_use]
    pub const fn trick_mode_control(self) -> TrickModeControl {
        TrickModeControl(self.0.wrapping_shr(5))
    }

    /// Returns `field_id`, under fast forward, freeze frame and fast reverse
    #[must_use]
    pub const fn field_id(self) -> Option<u8> {
        match self.trick_mode_control() {
            TrickModeControl::FAST_FORWARD
            | TrickModeControl::FREEZE_FRAME
            | TrickModeControl::FAST_REVERSE => Some(self.0.wrapping_shr(3) & 0b11),
            _ => None,
        }
    }

    /// Returns `intra_slice_refresh`, under fast forward and fast reverse
    #[must_use]
    pub const fn intra_slice_refresh(self) -> Option<bool> {
        match self.trick_mode_control() {
            TrickModeControl::FAST_FORWARD | TrickModeControl::FAST_REVERSE => {
                Some(self.0 & 0b100 != 0)
            }
            _ => None,
        }
    }

    /// Returns `frequency_truncation`, under fast forward and fast reverse
    #[must_use]
    pub const fn frequency_truncation(self) -> Option<u8> {
        match self.trick_mode_control() {
            TrickModeControl::FAST_FORWARD | TrickModeControl::FAST_REVERSE => Some(self.0 & 0b11),
            _ => None,
        }
    }

    /// Returns `rep_cntrl`, under slow motion and slow reverse
    #[must_use]
    pub const fn rep_cntrl(self) -> Option<u8> {
        match self.trick_mode_control() {
            TrickModeControl::SLOW_MOTION | TrickModeControl::SLOW_REVERSE => {
                Some(self.0 & 0b1_1111)
            }
            _ => None,
        }
    }
}

impl From<u8> for DsmTrickMode {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

/// Trick mode of a video stream
///
/// `trick_mode_control`, ISO/IEC 13818-1 §2.4.3.7, Table 2-24
///
/// [`DsmTrickMode::trick_mode_control`] reads it from a trick mode byte;
/// `'101'` to `'111'` are reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrickModeControl(u8);

impl TrickModeControl {
    /// `'000'`, fast forward
    pub const FAST_FORWARD: Self = Self(0b000);
    /// `'001'`, slow motion
    pub const SLOW_MOTION: Self = Self(0b001);
    /// `'010'`, freeze frame
    pub const FREEZE_FRAME: Self = Self(0b010);
    /// `'011'`, fast reverse
    pub const FAST_REVERSE: Self = Self(0b011);
    /// `'100'`, slow reverse
    pub const SLOW_REVERSE: Self = Self(0b100);

    /// Returns the 3-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{DsmTrickMode, TrickModeControl, TrickModeControlOutOfRange};

    /// Returns what each accessor reads from `byte`
    fn fields(
        byte: u8,
    ) -> (
        TrickModeControl,
        Option<u8>,
        Option<bool>,
        Option<u8>,
        Option<u8>,
    ) {
        let mode = DsmTrickMode::from(byte);
        (
            mode.trick_mode_control(),
            mode.field_id(),
            mode.intra_slice_refresh(),
            mode.frequency_truncation(),
            mode.rep_cntrl(),
        )
    }

    #[test]
    fn a_trick_mode_control_takes_any_3_bit_value_and_refuses_a_wider_one() {
        assert_eq!(
            [
                TrickModeControl::try_from(0b111),
                TrickModeControl::try_from(8)
            ],
            [
                Ok(TrickModeControl(0b111)),
                Err(TrickModeControlOutOfRange { value: 8 })
            ]
        );
    }

    #[test]
    #[expect(
        clippy::unusual_byte_groupings,
        reason = "the digits are grouped by the fields of the trick mode byte"
    )]
    fn each_trick_mode_gives_meaning_only_to_its_own_fields() {
        assert_eq!(
            [
                fields(0b000_10_1_01),
                fields(0b001_10101),
                fields(0b010_11_000),
                fields(0b011_01_0_11),
                fields(0b100_00011),
                fields(0b101_11111),
            ],
            [
                (
                    TrickModeControl::FAST_FORWARD,
                    Some(0b10),
                    Some(true),
                    Some(0b01),
                    None
                ),
                (
                    TrickModeControl::SLOW_MOTION,
                    None,
                    None,
                    None,
                    Some(0b10101)
                ),
                (TrickModeControl::FREEZE_FRAME, Some(0b11), None, None, None),
                (
                    TrickModeControl::FAST_REVERSE,
                    Some(0b01),
                    Some(false),
                    Some(0b11),
                    None
                ),
                (
                    TrickModeControl::SLOW_REVERSE,
                    None,
                    None,
                    None,
                    Some(0b00011)
                ),
                (TrickModeControl(0b101), None, None, None, None),
            ]
        );
    }
}

impl TryFrom<u8> for TrickModeControl {
    type Error = TrickModeControlOutOfRange;

    /// Takes a 3-bit value
    ///
    /// # Errors
    ///
    /// [`TrickModeControlOutOfRange`]: `value` is 8 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < 8 {
            Ok(Self(value))
        } else {
            Err(TrickModeControlOutOfRange { value })
        }
    }
}

/// Failure to make a [`TrickModeControl`] of a value 3 bits cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrickModeControlOutOfRange {
    value: u8,
}

impl TrickModeControlOutOfRange {
    /// Returns the value given, 8 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }
}

impl fmt::Display for TrickModeControlOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "trick_mode_control {} does not fit in 3 bits",
            self.value
        )
    }
}

impl core::error::Error for TrickModeControlOutOfRange {}
