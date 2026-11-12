//! The display mode of a caption

use mpeg2_systems::field::FourBitFieldOutOfRange;

/// Display mode, whether a caption is shown on its own when received and
/// when played back from a recording
///
/// `DMF`, ARIB STD-B24 Volume 1 Part 3 Table 9-5 and §9.6.1
///
/// The 4 bits are two halves: b4b3 is the mode on reception and b2b1 the
/// mode on playback of a recording. In each, `'00'` shows the caption on
/// its own, `'01'` does not, and `'10'` lets the viewer choose; `'11'`
/// shows or hides it under a condition on reception and is reserved on
/// playback. In a data component descriptor, `'1111'` says that the DMF of
/// the caption management data changes, and `'0011'` that the elementary
/// stream has a language whose captions are shown on their own.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::FourBitFieldOutOfRange;
/// use mpeg2_systems_arib::caption::Dmf;
///
/// assert_eq!(Dmf::try_from(0b1010).map(Dmf::get), Ok(0b1010));
/// // A DMF is 4 bits
/// assert_eq!(Dmf::try_from(16).map_err(FourBitFieldOutOfRange::value), Err(16));
/// ```
#[doc(alias("DMF", "display_mode"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Dmf(pub(crate) u8);

impl Dmf {
    /// Returns the 4 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Dmf {
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
