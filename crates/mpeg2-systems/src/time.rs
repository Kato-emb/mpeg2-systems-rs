//! Time stamps and clock references, the two clocks a stream carries
//!
//! A [`Timestamp`] counts the 90 kHz clock in 33 bits; a [`ClockReference`]
//! counts the 27 MHz system clock as a 33-bit base and a 9-bit extension. Both
//! wrap around, so neither has an order of its own: which of two values comes
//! first is the shorter way round the circle from one to the other, which
//! [`Timestamp::distance_from`] and [`ClockReference::distance_from`] measure.

use core::fmt;

/// Failure to construct a time stamp or a clock reference
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// A time stamp of 2^33 or more, which 33 bits cannot carry
    Timestamp {
        /// The value given
        value: u64,
    },
    /// A clock reference base of 2^33 or more, which 33 bits cannot carry
    ClockReferenceBase {
        /// The base given
        base: u64,
    },
    /// A clock reference extension of 512 or more, which 9 bits cannot carry
    ClockReferenceExtension {
        /// The extension given
        extension: u16,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timestamp { value } => {
                write!(formatter, "time stamp {value} does not fit in 33 bits")
            }
            Self::ClockReferenceBase { base } => {
                write!(
                    formatter,
                    "clock reference base {base} does not fit in 33 bits"
                )
            }
            Self::ClockReferenceExtension { extension } => write!(
                formatter,
                "clock reference extension {extension} does not fit in 9 bits"
            ),
        }
    }
}

impl core::error::Error for Error {}

/// Returns the signed distance from `other` to `value` the shorter way round a
/// circle of `modulus` ticks, the half-way point counting as behind
const fn shortest_distance(value: u64, other: u64, modulus: u64) -> i64 {
    // value and other are below modulus, and modulus is below 2^42, so no
    // step wraps and every result fits in an i64.
    let forward = if value >= other {
        value.wrapping_sub(other)
    } else {
        modulus.wrapping_sub(other).wrapping_add(value)
    };
    let backward = modulus.wrapping_sub(forward);

    if forward < backward {
        forward.cast_signed()
    } else {
        0_i64.wrapping_sub(backward.cast_signed())
    }
}

// Why not deriving Ord on Timestamp and ClockReference: a wrapping counter has
// no total order; "before" is only meaningful as the shortest signed distance
// on the circle.

/// Time stamp, the 90 kHz time at which a presentation unit is due to be decoded or presented
///
/// `PTS` and `DTS`, ISO/IEC 13818-1 §2.4.3.7
///
/// The value is a count of 90 kHz ticks in 33 bits, which wraps around about
/// every 26.5 hours. The crate does not convert it to seconds or to wall-clock
/// time, nor unwrap it into a continuous timeline.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::time::{Error, Timestamp};
///
/// // One tick before the counter wraps, and five ticks after it
/// let before_wrap = Timestamp::try_from(Timestamp::MODULUS - 1)?;
/// let after_wrap = Timestamp::try_from(4)?;
/// assert_eq!(after_wrap.distance_from(before_wrap), 5);
/// assert!(after_wrap.is_after(before_wrap));
///
/// // A value of more than 33 bits is refused
/// assert_eq!(
///     Timestamp::try_from(Timestamp::MODULUS),
///     Err(Error::Timestamp { value: Timestamp::MODULUS })
/// );
/// # Ok::<(), Error>(())
/// ```
#[doc(alias("PTS", "DTS", "TimeStamp", "time stamp"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timestamp(u64);

impl Timestamp {
    /// Number of distinct time stamps, 2^33
    pub const MODULUS: u64 = 1 << 33;

    /// Returns the count of 90 kHz ticks
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the signed distance from `other` to `self`, in 90 kHz ticks
    ///
    /// The distance is the shorter way round the 33-bit circle: positive when
    /// `self` is ahead of `other`, negative when behind. A distance of exactly
    /// half the circle counts as behind.
    #[must_use]
    pub const fn distance_from(self, other: Self) -> i64 {
        shortest_distance(self.0, other.0, Self::MODULUS)
    }

    /// Returns whether `self` is ahead of `other` the shorter way round the circle
    #[must_use]
    pub const fn is_after(self, other: Self) -> bool {
        self.distance_from(other) > 0
    }
}

impl TryFrom<u64> for Timestamp {
    type Error = Error;

    /// Takes a count of 90 kHz ticks
    ///
    /// # Errors
    ///
    /// * [`Timestamp`](Error::Timestamp): `value` is 2^33
    ///   or more.
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value < Self::MODULUS {
            Ok(Self(value))
        } else {
            Err(Error::Timestamp { value })
        }
    }
}

/// Clock reference, the 27 MHz time a decoder locks its system clock to
///
/// `program_clock_reference_base` and `_extension`, ISO/IEC 13818-1 §2.4.3.5;
/// `system_clock_reference_base` and `_extension`, §2.5.3.4
///
/// The base counts the system clock of §2.4.2.2 divided by 300, which is the
/// 90 kHz clock of a [`Timestamp`], in 33 bits; the extension counts the 27 MHz
/// ticks between two ticks of the base, from 0 to 299. The same pair carries
/// `OPCR` and `ESCR`. The 9 bits of the extension hold up to 511, and a value
/// of 300 or more, which the specification does not allow, is kept as found.
/// The crate does not convert a clock reference to seconds or to wall-clock
/// time, nor unwrap it into a continuous timeline.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::time::{ClockReference, Error};
///
/// // Two ticks of the base and 100 of the extension
/// let pcr = ClockReference::new(2, 100)?;
/// assert_eq!(pcr.ticks(), 700);
///
/// // The last tick before the clock wraps, and five ticks after it
/// let before_wrap = ClockReference::new((1 << 33) - 1, 299)?;
/// let after_wrap = ClockReference::new(0, 5)?;
/// assert_eq!(after_wrap.distance_from(before_wrap), 6);
///
/// // An extension of more than 9 bits is refused
/// assert_eq!(
///     ClockReference::new(0, 512),
///     Err(Error::ClockReferenceExtension { extension: 512 })
/// );
/// # Ok::<(), Error>(())
/// ```
#[doc(alias("PCR", "OPCR", "SCR", "ESCR"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClockReference {
    base: u64,
    extension: u16,
}

impl ClockReference {
    /// Number of distinct clock references in 27 MHz ticks, 2^33 × 300
    pub const MODULUS: u64 = Timestamp::MODULUS * 300;

    /// Creates a clock reference from its base and extension fields
    ///
    /// # Errors
    ///
    /// * [`ClockReferenceBase`](Error::ClockReferenceBase): `base` is 2^33 or more.
    /// * [`ClockReferenceExtension`](Error::ClockReferenceExtension): `extension` is
    ///   512 or more.
    pub const fn new(base: u64, extension: u16) -> Result<Self, Error> {
        if base >= Timestamp::MODULUS {
            return Err(Error::ClockReferenceBase { base });
        }
        if extension >= 512 {
            return Err(Error::ClockReferenceExtension { extension });
        }

        Ok(Self { base, extension })
    }

    /// Returns the base, in ticks of the 90 kHz clock
    #[must_use]
    pub const fn base(self) -> u64 {
        self.base
    }

    /// Returns the extension, in ticks of the 27 MHz clock
    #[must_use]
    pub const fn extension(self) -> u16 {
        self.extension
    }

    /// Returns the count of 27 MHz ticks, `base` × 300 + `extension`
    ///
    /// An extension of 300 or more counts as it is, so near the top of the
    /// base the count can pass [`MODULUS`](Self::MODULUS).
    #[must_use]
    pub fn ticks(self) -> u64 {
        // base < 2^33 and extension < 2^9 keep the count below 2^42.
        self.base
            .wrapping_mul(300)
            .wrapping_add(u64::from(self.extension))
    }

    /// Returns the signed distance from `other` to `self`, in 27 MHz ticks
    ///
    /// The distance is the shorter way round the circle of
    /// [`MODULUS`](Self::MODULUS) ticks: positive when `self` is ahead of
    /// `other`, negative when behind. A distance of exactly half the circle
    /// counts as behind.
    #[must_use]
    pub fn distance_from(self, other: Self) -> i64 {
        shortest_distance(
            self.ticks_on_circle(),
            other.ticks_on_circle(),
            Self::MODULUS,
        )
    }

    /// Returns whether `self` is ahead of `other` the shorter way round the circle
    #[must_use]
    pub fn is_after(self, other: Self) -> bool {
        self.distance_from(other) > 0
    }

    /// Returns [`ticks`](Self::ticks) taken round the circle once more if it passed it
    fn ticks_on_circle(self) -> u64 {
        // Why not `% MODULUS`: an out-of-spec extension passes MODULUS by less
        // than 512, so at most one turn is ever taken off.
        let ticks = self.ticks();
        if ticks >= Self::MODULUS {
            ticks.wrapping_sub(Self::MODULUS)
        } else {
            ticks
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ClockReference, Error, Timestamp};

    fn timestamp(value: u64) -> Timestamp {
        Timestamp::try_from(value).unwrap()
    }

    fn clock_reference(base: u64, extension: u16) -> ClockReference {
        ClockReference::new(base, extension).unwrap()
    }

    #[test]
    fn a_timestamp_takes_any_33_bit_value_and_refuses_a_wider_one() {
        assert_eq!(
            [
                Timestamp::try_from(Timestamp::MODULUS - 1).map(Timestamp::get),
                Timestamp::try_from(Timestamp::MODULUS).map(Timestamp::get),
            ],
            [
                Ok(Timestamp::MODULUS - 1),
                Err(Error::Timestamp {
                    value: Timestamp::MODULUS
                })
            ]
        );
    }

    #[test]
    fn timestamp_distance_is_signed() {
        assert_eq!(
            (
                timestamp(1000).distance_from(timestamp(400)),
                timestamp(400).distance_from(timestamp(1000))
            ),
            (600, -600)
        );
    }

    #[test]
    fn timestamp_distance_crosses_the_wrap_point() {
        let before_wrap = timestamp(Timestamp::MODULUS - 5);
        let after_wrap = timestamp(5);

        assert_eq!(
            (
                after_wrap.distance_from(before_wrap),
                before_wrap.distance_from(after_wrap)
            ),
            (10, -10)
        );
    }

    #[test]
    fn timestamp_distance_of_exactly_half_the_circle_is_negative() {
        let half = timestamp(Timestamp::MODULUS / 2);

        assert_eq!(
            (
                half.distance_from(timestamp(0)),
                timestamp(0).distance_from(half)
            ),
            (-(1_i64 << 32), -(1_i64 << 32))
        );
    }

    #[test]
    fn timestamp_is_after_follows_the_shortest_distance() {
        let before_wrap = timestamp(Timestamp::MODULUS - 5);
        let after_wrap = timestamp(5);

        assert_eq!(
            (
                after_wrap.is_after(before_wrap),
                before_wrap.is_after(after_wrap),
                after_wrap.is_after(after_wrap)
            ),
            (true, false, false)
        );
    }

    #[test]
    fn a_clock_reference_takes_a_33_bit_base_and_a_9_bit_extension() {
        assert_eq!(
            [
                ClockReference::new(Timestamp::MODULUS - 1, 511)
                    .map(|reference| (reference.base(), reference.extension())),
                ClockReference::new(Timestamp::MODULUS, 0)
                    .map(|reference| (reference.base(), reference.extension())),
                ClockReference::new(0, 512)
                    .map(|reference| (reference.base(), reference.extension())),
            ],
            [
                Ok((Timestamp::MODULUS - 1, 511)),
                Err(Error::ClockReferenceBase {
                    base: Timestamp::MODULUS
                }),
                Err(Error::ClockReferenceExtension { extension: 512 })
            ]
        );
    }

    #[test]
    fn clock_reference_ticks_combine_base_and_extension() {
        assert_eq!(clock_reference(2, 100).ticks(), 700);
    }

    #[test]
    fn clock_reference_distance_crosses_the_wrap_point() {
        let before_wrap = clock_reference((1 << 33) - 1, 299);
        let after_wrap = clock_reference(0, 5);

        assert_eq!(
            (
                after_wrap.distance_from(before_wrap),
                before_wrap.distance_from(after_wrap),
                after_wrap.is_after(before_wrap)
            ),
            (6, -6, true)
        );
    }

    #[test]
    fn clock_reference_distance_of_exactly_half_the_circle_is_negative() {
        let half = clock_reference(1 << 32, 0);

        assert_eq!(
            half.distance_from(clock_reference(0, 0)),
            0_i64.wrapping_sub_unsigned(ClockReference::MODULUS / 2)
        );
    }

    #[test]
    fn clock_reference_with_an_out_of_spec_extension_still_wraps() {
        let top_of_range = clock_reference((1 << 33) - 1, 511);
        let after_wrap = clock_reference(0, 0);

        assert_eq!(after_wrap.distance_from(top_of_range), -211);
    }
}
