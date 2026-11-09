//! Time stamps and clock references, the two clocks a stream carries
//!
//! A [`Timestamp`] counts the 90 kHz clock in 33 bits; a [`ClockReference`]
//! counts the 27 MHz system clock as a 33-bit base and a 9-bit extension. Both
//! wrap around, so neither has an order of its own: which of two values comes
//! first is the shorter way round the circle from one to the other, which
//! [`Timestamp::distance_from`] and [`ClockReference::distance_from`] measure.

use core::fmt;
#[cfg(feature = "alloc")]
use core::time::Duration;

#[cfg(feature = "alloc")]
use crate::field::FieldWriter;
use crate::field::{self, FieldReader};

/// Failure to make a [`Timestamp`] of a value 33 bits cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimestampOutOfRange {
    value: u64,
}

impl TimestampOutOfRange {
    /// Returns the value given, 2^33 or more
    #[must_use]
    pub const fn value(self) -> u64 {
        self.value
    }
}

impl fmt::Display for TimestampOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "time stamp {} does not fit in 33 bits",
            self.value
        )
    }
}

impl core::error::Error for TimestampOutOfRange {}

/// Failure to make a [`ClockReference`] of a base or an extension its bits cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClockReferenceOutOfRange {
    base: u64,
    extension: u16,
}

impl ClockReferenceOutOfRange {
    /// Returns the base given, which 33 bits may not carry
    #[must_use]
    pub const fn base(self) -> u64 {
        self.base
    }

    /// Returns the extension given, which 9 bits may not carry
    #[must_use]
    pub const fn extension(self) -> u16 {
        self.extension
    }
}

impl fmt::Display for ClockReferenceOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { base, extension } = *self;
        match (
            base >= Timestamp::MODULUS,
            extension >= ClockReference::EXTENSION_LIMIT,
        ) {
            (true, true) => write!(
                formatter,
                "clock reference base {base} does not fit in 33 bits \
                 and extension {extension} does not fit in 9 bits"
            ),
            (true, false) => write!(
                formatter,
                "clock reference base {base} does not fit in 33 bits"
            ),
            (false, _) => write!(
                formatter,
                "clock reference extension {extension} does not fit in 9 bits"
            ),
        }
    }
}

impl core::error::Error for ClockReferenceOutOfRange {}

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
/// use mpeg2_systems::time::{Timestamp, TimestampOutOfRange};
///
/// // One tick before the counter wraps, and five ticks after it
/// let before_wrap = Timestamp::try_from(Timestamp::MODULUS - 1)?;
/// let after_wrap = Timestamp::try_from(4)?;
/// assert_eq!(after_wrap.distance_from(before_wrap), 5);
/// assert!(after_wrap.is_after(before_wrap));
///
/// // A value of more than 33 bits is refused, and the error carries it
/// assert_eq!(
///     Timestamp::try_from(Timestamp::MODULUS).map_err(TimestampOutOfRange::value),
///     Err(Timestamp::MODULUS)
/// );
/// # Ok::<(), TimestampOutOfRange>(())
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

    /// Takes the 33 bits of a time stamp field, keeping only those bits
    pub(crate) const fn from_33_bits(value: u64) -> Self {
        Self(value & Self::MODULUS.wrapping_sub(1))
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

/// A time of the system time clock (STC), the 27 MHz clock all the timing
/// of ISO/IEC 13818-1 is defined in, counted on past the wrap of the fields
/// that sample it
///
/// A PTS or a DTS ([`Timestamp`]) and a PCR or an SCR ([`ClockReference`])
/// are samples of the STC, each the time of the STC taken modulo its period
/// (equations 2-2 and 2-11, Annex D.1.3); a `SystemTimeClock` is
/// the time before the modulo is taken. The first time is placed one whole
/// period of [`Timestamp::MODULUS`] in, so that a time behind it is still a
/// time.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SystemTimeClock(u64);

#[cfg(feature = "alloc")]
impl SystemTimeClock {
    /// Ticks of the STC in one tick of the 90 kHz clock of PTS and DTS
    const TICKS_PER_TIMESTAMP_TICK: u64 = 300;

    /// Returns the time `timestamp` samples, the first time placed
    pub(crate) const fn first(timestamp: Timestamp) -> Self {
        Self(
            Timestamp::MODULUS
                .saturating_add(timestamp.get())
                .saturating_mul(Self::TICKS_PER_TIMESTAMP_TICK),
        )
    }

    /// Returns the time `timestamp` samples that is nearest `self`
    pub(crate) const fn expand(self, timestamp: Timestamp) -> Self {
        let reference = self.0 / Self::TICKS_PER_TIMESTAMP_TICK;
        Self(
            reference
                .saturating_add_signed(timestamp.distance_from(Timestamp::from_33_bits(reference)))
                .saturating_mul(Self::TICKS_PER_TIMESTAMP_TICK),
        )
    }

    /// Returns the PTS or the DTS that samples the time
    pub(crate) const fn to_timestamp(self) -> Timestamp {
        Timestamp::from_33_bits(self.0 / Self::TICKS_PER_TIMESTAMP_TICK)
    }

    /// Returns the PCR or the SCR that samples the time `delay` before it
    pub(crate) fn to_clock_reference(self, delay: Duration) -> ClockReference {
        // Why not `self - delay` wrapping: it wraps at 2^64, which is not a
        // multiple of the period, so both are taken modulo it first.
        ClockReference::from_ticks(
            (self.0 % ClockReference::MODULUS)
                .wrapping_add(ClockReference::MODULUS)
                .wrapping_sub(Self::ticks_in(delay) % ClockReference::MODULUS),
        )
    }

    /// Returns the time `ticks` ticks of the STC after it
    pub(crate) const fn add_ticks(self, ticks: u64) -> Self {
        Self(self.0.saturating_add(ticks))
    }

    /// Returns the ticks of the STC from `earlier` to it, 0 if `earlier` is
    /// not before it
    pub(crate) const fn ticks_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    /// Returns the ticks of the 90 kHz clock of PTS and DTS from `earlier`
    /// to it, 0 if `earlier` is not before it
    pub(crate) const fn timestamp_ticks_since(self, earlier: Self) -> u64 {
        self.ticks_since(earlier) / Self::TICKS_PER_TIMESTAMP_TICK
    }

    /// Returns `duration` in ticks of the STC
    pub(crate) fn ticks_in(duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos().saturating_mul(27_000_000) / 1_000_000_000)
            .unwrap_or(u64::MAX)
    }
}

impl TryFrom<u64> for Timestamp {
    type Error = TimestampOutOfRange;

    /// Takes a count of 90 kHz ticks
    ///
    /// # Errors
    ///
    /// [`TimestampOutOfRange`]: `value` is 2^33 or more.
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value < Self::MODULUS {
            Ok(Self(value))
        } else {
            Err(TimestampOutOfRange { value })
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
/// use mpeg2_systems::time::{ClockReference, ClockReferenceOutOfRange};
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
/// // An extension of more than 9 bits is refused, and the error carries both fields
/// let error = ClockReference::new(0, 512).unwrap_err();
/// assert_eq!((error.base(), error.extension()), (0, 512));
/// # Ok::<(), ClockReferenceOutOfRange>(())
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

    /// Number of values the 9 bits of the extension carry, 2^9
    const EXTENSION_LIMIT: u16 = 1 << 9;

    /// Number of values the specification lets the extension take, 0 to 299
    #[cfg(feature = "alloc")]
    pub(crate) const EXTENSION_MODULUS: u16 = 300;

    /// Creates a clock reference from its base and extension fields
    ///
    /// # Errors
    ///
    /// [`ClockReferenceOutOfRange`]: `base` is 2^33 or more, or `extension` is
    /// 512 or more.
    pub const fn new(base: u64, extension: u16) -> Result<Self, ClockReferenceOutOfRange> {
        if base >= Timestamp::MODULUS || extension >= Self::EXTENSION_LIMIT {
            return Err(ClockReferenceOutOfRange { base, extension });
        }

        Ok(Self { base, extension })
    }

    /// Takes the 33 bits of base and 9 of extension of a clock reference
    /// field, keeping only those bits
    pub(crate) const fn from_fields(base: u64, extension: u16) -> Self {
        Self {
            base: base & Timestamp::MODULUS.wrapping_sub(1),
            extension: extension & Self::EXTENSION_LIMIT.wrapping_sub(1),
        }
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

    /// Returns the clock reference that counts `ticks` 27 MHz ticks, round
    /// the circle of [`MODULUS`](Self::MODULUS) ticks
    #[cfg(feature = "alloc")]
    pub(crate) fn from_ticks(ticks: u64) -> Self {
        let extension_modulus = u64::from(Self::EXTENSION_MODULUS);
        Self::from_fields(
            ticks.checked_div(extension_modulus).unwrap_or(0),
            ticks
                .checked_rem(extension_modulus)
                .and_then(|extension| u16::try_from(extension).ok())
                .unwrap_or(0),
        )
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

/// Reads 33 bits laid out as 3, a `marker_bit`, 15, a `marker_bit`, 15 and a
/// `marker_bit`, ignoring the markers
pub(crate) fn read_marked_33_bits(reader: &mut FieldReader<'_>) -> Result<u64, field::Error> {
    let high = reader.read_bits::<3>()?;
    reader.read_bits::<1>()?;
    let middle = reader.read_bits::<15>()?;
    reader.read_bits::<1>()?;
    let low = reader.read_bits::<15>()?;
    reader.read_bits::<1>()?;

    Ok(high.wrapping_shl(30) | middle.wrapping_shl(15) | low)
}

/// Writes 33 bits as 3, a `marker_bit`, 15, a `marker_bit`, 15 and a `marker_bit`
#[cfg(feature = "alloc")]
pub(crate) fn write_marked_33_bits(
    writer: &mut FieldWriter<'_>,
    value: u64,
) -> Result<(), field::Error> {
    writer.write_bits::<3>(value.wrapping_shr(30) & 0b111)?;
    writer.write_bool(true)?;
    writer.write_bits::<15>(value.wrapping_shr(15) & 0x7fff)?;
    writer.write_bool(true)?;
    writer.write_bits::<15>(value & 0x7fff)?;
    writer.write_bool(true)
}

/// Reads a clock reference laid out as 33 marked bits, 9 of extension and a
/// `marker_bit`, ignoring the markers, as an SCR and an ESCR are
pub(crate) fn read_marked_clock_reference(
    reader: &mut FieldReader<'_>,
) -> Result<ClockReference, field::Error> {
    let base = read_marked_33_bits(reader)?;
    let extension = reader.read_u16::<9>()?;
    reader.read_bits::<1>()?;

    Ok(ClockReference::from_fields(base, extension))
}

/// Writes a clock reference as 33 marked bits, 9 of extension and a
/// `marker_bit`, as an SCR and an ESCR are
#[cfg(feature = "alloc")]
pub(crate) fn write_marked_clock_reference(
    writer: &mut FieldWriter<'_>,
    reference: ClockReference,
) -> Result<(), field::Error> {
    write_marked_33_bits(writer, reference.base())?;
    writer.write_bits::<9>(reference.extension())?;
    writer.write_bool(true)
}

#[cfg(test)]
mod tests {
    use super::{ClockReference, ClockReferenceOutOfRange, Timestamp, TimestampOutOfRange};
    #[cfg(feature = "alloc")]
    use alloc::string::ToString;

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
                Err(TimestampOutOfRange {
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
                Err(ClockReferenceOutOfRange {
                    base: Timestamp::MODULUS,
                    extension: 0
                }),
                Err(ClockReferenceOutOfRange {
                    base: 0,
                    extension: 512
                })
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn an_out_of_range_clock_reference_names_the_field_that_does_not_fit() {
        let messages = [
            ClockReference::new(Timestamp::MODULUS, 0),
            ClockReference::new(0, 512),
            ClockReference::new(Timestamp::MODULUS, 512),
        ]
        .map(|result| result.map_err(|error| error.to_string()));

        assert_eq!(
            messages,
            [
                Err("clock reference base 8589934592 does not fit in 33 bits".to_string()),
                Err("clock reference extension 512 does not fit in 9 bits".to_string()),
                Err("clock reference base 8589934592 does not fit in 33 bits \
                     and extension 512 does not fit in 9 bits"
                    .to_string()),
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
