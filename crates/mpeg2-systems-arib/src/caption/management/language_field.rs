//! The value types of the fields of a language of caption management data

use mpeg2_systems::field::{FourBitFieldOutOfRange, TwoBitFieldOutOfRange};

use crate::caption::ThreeBitFieldOutOfRange;

/// Number of a language of the captions, 0 for the first and 7 for the
/// eighth
///
/// `language_tag`, ARIB STD-B24 Volume 1 Part 3 §9.3.1
///
/// # Examples
///
/// ```
/// use mpeg2_systems_arib::caption::ThreeBitFieldOutOfRange;
/// use mpeg2_systems_arib::caption::management::LanguageTag;
///
/// assert_eq!(LanguageTag::try_from(7).map(LanguageTag::get), Ok(7));
/// // A language_tag is 3 bits
/// assert_eq!(LanguageTag::try_from(8).map_err(ThreeBitFieldOutOfRange::value), Err(8));
/// ```
#[doc(alias = "language_tag")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LanguageTag(u8);

impl LanguageTag {
    /// Returns the 3 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Takes the 3 bits of a `language_tag` field, keeping only those bits
    pub(super) const fn from_bits_truncate(value: u8) -> Self {
        Self(value & 0b111)
    }
}

impl TryFrom<u8> for LanguageTag {
    type Error = ThreeBitFieldOutOfRange;

    /// Takes the 3 bits of `value`
    ///
    /// # Errors
    ///
    /// [`ThreeBitFieldOutOfRange`]: `value` is 8 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        ThreeBitFieldOutOfRange::check(value).map(Self)
    }
}

/// Display condition, on which the captions of a display mode that depends
/// on one are shown or hidden
///
/// `DC`, ARIB STD-B24 Volume 1 Part 3 §9.3.1, Table 9-6
///
/// Table 9-6 leaves 0x01 to 0xFF to be specified elsewhere.
#[doc(alias = "DC")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Dc(u8);

impl Dc {
    /// 0x00: the message shown when rain attenuates the signal
    pub const RAIN_ATTENUATION_MESSAGE: Self = Self(0x00);

    /// Returns the 8 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<u8> for Dc {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

/// Display format, how the captions are laid out on the screen first
///
/// `Format`, ARIB STD-B24 Volume 1 Part 3 §9.3.1, Table 9-7
///
/// `'0101'`, `'1110'` and `'1111'` are not defined, so they are read but not
/// written.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::FourBitFieldOutOfRange;
/// use mpeg2_systems_arib::caption::management::Format;
///
/// assert_eq!(Format::try_from(0b1000), Ok(Format::HORIZONTAL_960_X_540));
/// // A Format is 4 bits
/// assert_eq!(Format::try_from(16).map_err(FourBitFieldOutOfRange::value), Err(16));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Format(u8);

impl Format {
    /// `'0000'`: horizontal writing in standard density
    pub const HORIZONTAL_STANDARD_DENSITY: Self = Self(0b0000);
    /// `'0001'`: vertical writing in standard density
    pub const VERTICAL_STANDARD_DENSITY: Self = Self(0b0001);
    /// `'0010'`: horizontal writing in high density
    pub const HORIZONTAL_HIGH_DENSITY: Self = Self(0b0010);
    /// `'0011'`: vertical writing in high density
    pub const VERTICAL_HIGH_DENSITY: Self = Self(0b0011);
    /// `'0100'`: horizontal writing of a Western language
    pub const HORIZONTAL_WESTERN_LANGUAGE: Self = Self(0b0100);
    /// `'0110'`: horizontal writing in 1920 x 1080
    pub const HORIZONTAL_1920_X_1080: Self = Self(0b0110);
    /// `'0111'`: vertical writing in 1920 x 1080
    pub const VERTICAL_1920_X_1080: Self = Self(0b0111);
    /// `'1000'`: horizontal writing in 960 x 540
    pub const HORIZONTAL_960_X_540: Self = Self(0b1000);
    /// `'1001'`: vertical writing in 960 x 540
    pub const VERTICAL_960_X_540: Self = Self(0b1001);
    /// `'1010'`: horizontal writing in 720 x 480
    pub const HORIZONTAL_720_X_480: Self = Self(0b1010);
    /// `'1011'`: vertical writing in 720 x 480
    pub const VERTICAL_720_X_480: Self = Self(0b1011);
    /// `'1100'`: horizontal writing in 1280 x 720
    pub const HORIZONTAL_1280_X_720: Self = Self(0b1100);
    /// `'1101'`: vertical writing in 1280 x 720
    pub const VERTICAL_1280_X_720: Self = Self(0b1101);

    /// Returns the 4 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Takes the 4 bits of a `Format` field, keeping only those bits
    pub(super) const fn from_bits_truncate(value: u8) -> Self {
        Self(value & 0b1111)
    }
}

impl TryFrom<u8> for Format {
    type Error = FourBitFieldOutOfRange;

    /// Takes the 4 bits of `value`
    ///
    /// # Errors
    ///
    /// [`FourBitFieldOutOfRange`]: `value` is 16 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        FourBitFieldOutOfRange::check(value).map(Self)
    }
}

/// Character coding of the captions
///
/// `TCS`, ARIB STD-B24 Volume 1 Part 3 §9.3.1, Table 9-8
///
/// `'10'` and `'11'` are reserved, so they are read but not written.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::TwoBitFieldOutOfRange;
/// use mpeg2_systems_arib::caption::management::Tcs;
///
/// assert_eq!(Tcs::try_from(0b01), Ok(Tcs::UCS));
/// // A TCS is 2 bits
/// assert_eq!(Tcs::try_from(4).map_err(TwoBitFieldOutOfRange::value), Err(4));
/// ```
#[doc(alias = "TCS")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tcs(u8);

impl Tcs {
    /// `'00'`: the 8-unit code
    pub const EIGHT_BIT_CODE: Self = Self(0b00);
    /// `'01'`: a coding of UCS, such as UTF-8
    pub const UCS: Self = Self(0b01);

    /// Returns the 2 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Takes the 2 bits of a `TCS` field, keeping only those bits
    pub(super) const fn from_bits_truncate(value: u8) -> Self {
        Self(value & 0b11)
    }
}

impl TryFrom<u8> for Tcs {
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

/// Roll-up mode, whether the captions roll up
///
/// `rollup_mode`, ARIB STD-B24 Volume 1 Part 3 §9.3.1, Table 9-9
///
/// `'10'` and `'11'` are reserved for future use, so they are read but not
/// written.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::TwoBitFieldOutOfRange;
/// use mpeg2_systems_arib::caption::management::RollupMode;
///
/// assert_eq!(RollupMode::try_from(0b01), Ok(RollupMode::ROLL_UP));
/// // A rollup_mode is 2 bits
/// assert_eq!(RollupMode::try_from(4).map_err(TwoBitFieldOutOfRange::value), Err(4));
/// ```
#[doc(alias = "rollup_mode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RollupMode(u8);

impl RollupMode {
    /// `'00'`: the captions do not roll up
    pub const NON_ROLL_UP: Self = Self(0b00);
    /// `'01'`: the captions roll up
    pub const ROLL_UP: Self = Self(0b01);

    /// Returns the 2 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Takes the 2 bits of a `rollup_mode` field, keeping only those bits
    pub(super) const fn from_bits_truncate(value: u8) -> Self {
        Self(value & 0b11)
    }
}

impl TryFrom<u8> for RollupMode {
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

#[cfg(test)]
mod tests {
    use super::{Format, LanguageTag, RollupMode, Tcs};

    #[test]
    fn a_language_tag_keeps_only_the_low_3_bits_of_a_wider_value() {
        assert_eq!(
            [
                LanguageTag::from_bits_truncate(0b111),
                LanguageTag::from_bits_truncate(0b1010)
            ],
            [LanguageTag(0b111), LanguageTag(0b010)]
        );
    }

    #[test]
    fn a_format_keeps_only_the_low_4_bits_of_a_wider_value() {
        assert_eq!(
            [
                Format::from_bits_truncate(0b1111),
                Format::from_bits_truncate(0xa5)
            ],
            [Format(0b1111), Format(0b0101)]
        );
    }

    #[test]
    fn a_tcs_keeps_only_the_low_2_bits_of_a_wider_value() {
        assert_eq!(
            [
                Tcs::from_bits_truncate(0b11),
                Tcs::from_bits_truncate(0b1110)
            ],
            [Tcs(0b11), Tcs(0b10)]
        );
    }

    #[test]
    fn a_rollup_mode_keeps_only_the_low_2_bits_of_a_wider_value() {
        assert_eq!(
            [
                RollupMode::from_bits_truncate(0b11),
                RollupMode::from_bits_truncate(0b1110)
            ],
            [RollupMode(0b11), RollupMode(0b10)]
        );
    }
}
