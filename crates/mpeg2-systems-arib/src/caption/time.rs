//! The time control of caption data: its mode, and the time of day or the
//! offset it is shown by

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use mpeg2_systems::field::{FieldReader, TwoBitFieldOutOfRange};

use super::Error;

/// Time control mode, what the time of a caption is counted from
///
/// `TMD`, ARIB STD-B24 Volume 1 Part 3 §9.3.1, Table 9-4
///
/// `'11'` is reserved, so it is read but not written.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::TwoBitFieldOutOfRange;
/// use mpeg2_systems_arib::caption::Tmd;
///
/// assert_eq!(Tmd::try_from(0b10), Ok(Tmd::OFFSET_TIME));
/// // A TMD is 2 bits
/// assert_eq!(Tmd::try_from(4).map_err(TwoBitFieldOutOfRange::value), Err(4));
/// ```
#[doc(alias = "TMD")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tmd(u8);

impl Tmd {
    /// `'00'`: the caption is not timed by the clock
    pub const FREE: Self = Self(0b00);
    /// `'01'`: the caption is shown at the time of the clock, which the TDT
    /// sets, its PTS giving the time
    pub const REAL_TIME: Self = Self(0b01);
    /// `'10'`: the caption is shown at its time plus an offset, by the clock
    /// the TDT sets
    pub const OFFSET_TIME: Self = Self(0b10);

    /// Returns the 2 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Tmd {
    type Error = TwoBitFieldOutOfRange;

    /// Takes the 2 bits of `value`
    ///
    /// # Errors
    ///
    /// [`TwoBitFieldOutOfRange`]: `value` is 4 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        TwoBitFieldOutOfRange::check(value).map(Self)
    }
}

/// Time of the caption data, as hours, minutes, seconds and milliseconds in
/// nine binary-coded decimal digits
///
/// `OTM` (offset time) of the caption management data and `STM` (presentation
/// start time) of the caption statement data, ARIB STD-B24 Volume 1 Part 3
/// §9.3.1 and §9.3.2
///
/// The 36 bits are kept as read, so a time is written back as it came, even
/// with a digit above 9, which the `write` of
/// [`CaptionManagementData`](super::management::CaptionManagementData::write)
/// and of [`CaptionData`](super::statement::CaptionData::write) refuses. The
/// accessors read the digits as decimal without checking them.
///
/// # Examples
///
/// ```
/// use mpeg2_systems_arib::caption::BcdTime;
///
/// let time = BcdTime::new(1, 23, 45, 678)?;
/// assert_eq!(time.get(), 0x0_1234_5678);
/// assert_eq!(
///     (time.hour(), time.minute(), time.second(), time.millisecond()),
///     (1, 23, 45, 678)
/// );
/// // Minutes go to 59
/// assert!(BcdTime::new(1, 60, 0, 0).is_err());
/// # Ok::<(), mpeg2_systems_arib::caption::BcdTimeOutOfRange>(())
/// ```
#[doc(alias("OTM", "STM"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BcdTime(u64);

impl BcdTime {
    /// Creates the time of `hour`, `minute`, `second` and `millisecond`
    ///
    /// # Errors
    ///
    /// [`BcdTimeOutOfRange`]: `hour` is above 99, `minute` or `second`
    /// above 59, or `millisecond` above 999.
    pub const fn new(
        hour: u8,
        minute: u8,
        second: u8,
        millisecond: u16,
    ) -> Result<Self, BcdTimeOutOfRange> {
        if hour > 99 || minute > 59 || second > 59 || millisecond > 999 {
            return Err(BcdTimeOutOfRange {
                hour,
                minute,
                second,
                millisecond,
            });
        }
        let (hour, minute, second, millisecond) = (
            hour as u64,
            minute as u64,
            second as u64,
            millisecond as u64,
        );

        Ok(Self(
            ((hour / 10) << 32)
                | ((hour % 10) << 28)
                | ((minute / 10) << 24)
                | ((minute % 10) << 20)
                | ((second / 10) << 16)
                | ((second % 10) << 12)
                | ((millisecond / 100) << 8)
                | ((millisecond / 10 % 10) << 4)
                | (millisecond % 10),
        ))
    }

    /// Returns the 36 bits
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the hours, the first two digits
    #[must_use]
    pub fn hour(self) -> u8 {
        self.digit(0)
            .saturating_mul(10)
            .saturating_add(self.digit(1))
    }

    /// Returns the minutes, the third and fourth digits
    #[must_use]
    pub fn minute(self) -> u8 {
        self.digit(2)
            .saturating_mul(10)
            .saturating_add(self.digit(3))
    }

    /// Returns the seconds, the fifth and sixth digits
    #[must_use]
    pub fn second(self) -> u8 {
        self.digit(4)
            .saturating_mul(10)
            .saturating_add(self.digit(5))
    }

    /// Returns the milliseconds, the last three digits
    #[must_use]
    pub fn millisecond(self) -> u16 {
        u16::from(self.digit(6))
            .saturating_mul(100)
            .saturating_add(u16::from(self.digit(7)).saturating_mul(10))
            .saturating_add(u16::from(self.digit(8)))
    }

    /// Returns digit `index`, 0 the first of the nine
    fn digit(self, index: u32) -> u8 {
        let shift = 32_u32.saturating_sub(index.saturating_mul(4));
        u8::try_from(self.0.checked_shr(shift).unwrap_or_default() & 0x0f).unwrap_or_default()
    }

    /// Returns whether every digit is 9 or less
    #[cfg(feature = "alloc")]
    fn is_decimal(self) -> bool {
        (0..9).all(|index| self.digit(index) <= 9)
    }
}

/// Failure to make a [`BcdTime`] of a time its two digits of hours, minutes
/// and seconds and three of milliseconds cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BcdTimeOutOfRange {
    hour: u8,
    minute: u8,
    second: u8,
    millisecond: u16,
}

impl BcdTimeOutOfRange {
    /// Returns the hours given
    #[must_use]
    pub const fn hour(self) -> u8 {
        self.hour
    }

    /// Returns the minutes given
    #[must_use]
    pub const fn minute(self) -> u8 {
        self.minute
    }

    /// Returns the seconds given
    #[must_use]
    pub const fn second(self) -> u8 {
        self.second
    }

    /// Returns the milliseconds given
    #[must_use]
    pub const fn millisecond(self) -> u16 {
        self.millisecond
    }
}

impl fmt::Display for BcdTimeOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "time {}:{}:{}.{} has an hour above 99, a minute or second above 59, or a \
             millisecond above 999",
            self.hour, self.minute, self.second, self.millisecond
        )
    }
}

impl core::error::Error for BcdTimeOutOfRange {}

/// Reads `TMD` and its `Reserved` bits, then the time `field` and its
/// `Reserved` bits when `timed` says the TMD has one
pub(super) fn read_header(
    reader: &mut FieldReader<'_>,
    field: &'static str,
    timed: fn(Tmd) -> bool,
) -> Result<(Tmd, Option<BcdTime>), Error> {
    let error = |field| move |error| Error::Field { field, error };
    let tmd = Tmd(reader.read_u8::<2>().map_err(error("TMD"))?);
    // Why not keep the Reserved bits: they are written '1'.
    reader.read_u8::<6>().map_err(error("TMD"))?;
    if !timed(tmd) {
        return Ok((tmd, None));
    }
    let time = BcdTime(reader.read_bits::<36>().map_err(error(field))?);
    reader.read_u8::<4>().map_err(error(field))?;

    Ok((tmd, Some(time)))
}

/// Refuses a reserved `TMD`, a time `field` present or absent against what
/// `timed` says, and a time with a digit above 9
#[cfg(feature = "alloc")]
pub(super) fn check_header(
    tmd: Tmd,
    time: Option<BcdTime>,
    field: &'static str,
    timed: fn(Tmd) -> bool,
) -> Result<(), Error> {
    if tmd.get() == 0b11 {
        return Err(Error::Forbidden {
            field: "TMD",
            value: u64::from(tmd.get()),
        });
    }
    match time {
        Some(_) if !timed(tmd) => Err(Error::Presence { field }),
        None if timed(tmd) => Err(Error::Presence { field }),
        Some(time) if !time.is_decimal() => Err(Error::Forbidden {
            field,
            value: time.get(),
        }),
        Some(_) | None => Ok(()),
    }
}

/// Appends `TMD`, the time, if any, and their `Reserved` bits, all `'1'`
#[cfg(feature = "alloc")]
pub(super) fn write_header(out: &mut Vec<u8>, tmd: Tmd, time: Option<BcdTime>) {
    out.push(tmd.get() << 6 | 0b11_1111);
    if let Some(time) = time {
        let [_, _, _, bytes @ ..] = (time.get() << 4 | 0x0f).to_be_bytes();
        out.extend_from_slice(&bytes);
    }
}
