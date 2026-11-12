//! The languages of caption management data

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use mpeg2_systems::field::FieldReader;

use super::language_field::{Dc, Format, LanguageTag, RollupMode, Tcs};
#[cfg(feature = "alloc")]
use crate::caption::write_or_truncate;
use crate::caption::{Dmf, Error};

/// Entry of the caption management data: a language and how its captions
/// are shown
///
/// The language loop of `caption_management_data`, ARIB STD-B24 Volume 1
/// Part 3 §9.3.1, Table 9-3
///
/// The 1 `reserved` bit after `language_tag` is not kept: it is written
/// `'1'`. `dc` is present exactly when `dmf` is `'1100'`, `'1101'` or
/// `'1110'`, the display modes that depend on a condition.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Language {
    /// Number of the language, 0 for the first
    pub language_tag: LanguageTag,
    /// How the captions of the language are shown on reception and on
    /// playback of a recording
    pub dmf: Dmf,
    /// Condition the captions are shown on, for the display modes that
    /// depend on one
    pub dc: Option<Dc>,
    /// Code of ISO 639-2, three characters of ISO 8859-1
    pub iso_639_language_code: [u8; 3],
    /// How the captions are laid out first
    pub format: Format,
    /// Coding of the characters
    pub tcs: Tcs,
    /// Whether the captions roll up
    pub rollup_mode: RollupMode,
}

impl Language {
    /// Creates the entry of a language, without a display condition
    #[must_use]
    pub const fn new(
        language_tag: LanguageTag,
        dmf: Dmf,
        iso_639_language_code: [u8; 3],
        format: Format,
        tcs: Tcs,
        rollup_mode: RollupMode,
    ) -> Self {
        Self {
            language_tag,
            dmf,
            dc: None,
            iso_639_language_code,
            format,
            tcs,
            rollup_mode,
        }
    }

    /// Returns the value with `dc` present, set to the one given
    #[must_use]
    pub const fn with_dc(mut self, dc: Dc) -> Self {
        self.dc = Some(dc);
        self
    }

    /// Appends the entry to `out`, refusing reserved values and a `DC` that
    /// does not match the `DMF`
    #[cfg(feature = "alloc")]
    fn write(self, out: &mut Vec<u8>) -> Result<(), Error> {
        let forbidden = |field, value: u8| Error::Forbidden {
            field,
            value: u64::from(value),
        };
        let dmf = self.dmf.get();
        if dmf & 0b11 == 0b11 {
            return Err(forbidden("DMF", dmf));
        }
        if self.dc.is_some() != matches!(dmf, 0b1100..=0b1110) {
            return Err(Error::Presence { field: "DC" });
        }
        let format = self.format.get();
        if matches!(format, 0b0101 | 0b1110 | 0b1111) {
            return Err(forbidden("Format", format));
        }
        if self.tcs.get() >= 0b10 {
            return Err(forbidden("TCS", self.tcs.get()));
        }
        if self.rollup_mode.get() >= 0b10 {
            return Err(forbidden("rollup_mode", self.rollup_mode.get()));
        }
        out.push(self.language_tag.get() << 5 | 0b1_0000 | dmf);
        if let Some(dc) = self.dc {
            out.push(dc.get());
        }
        out.extend_from_slice(&self.iso_639_language_code);
        out.push(format << 4 | self.tcs.get() << 2 | self.rollup_mode.get());

        Ok(())
    }
}

/// The languages of caption management data, borrowed
///
/// The bytes are read entry by entry as [`iter`](Self::iter) walks them, so
/// a view of any bytes can be made; an entry the bytes end inside is an
/// error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LanguageLoop<'input>(&'input [u8]);

impl<'input> LanguageLoop<'input> {
    /// Returns the bytes of the loop
    #[must_use]
    pub const fn as_bytes(self) -> &'input [u8] {
        self.0
    }

    /// Returns whether the loop has no bytes
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// Returns an iterator over the entries, each read as it is reached
    #[must_use]
    pub const fn iter(self) -> LanguageIter<'input> {
        LanguageIter(self.0)
    }

    /// Appends the bytes of a loop of `languages` to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): a `DMF` whose playback half is
    ///   `'11'`, reserved (Table 9-5), a `Format` Table 9-7 does not define,
    ///   or a reserved `TCS` or `rollup_mode`.
    /// * [`Presence`](Error::Presence): `DC` is present with another `DMF`
    ///   than `'1100'`, `'1101'` and `'1110'`, or absent with one of them.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems_arib::caption::Dmf;
    /// use mpeg2_systems_arib::caption::management::{
    ///     Format, Language, LanguageLoop, LanguageTag, RollupMode, Tcs,
    /// };
    ///
    /// let mut bytes = Vec::new();
    /// LanguageLoop::write_from(
    ///     [Language::new(
    ///         LanguageTag::try_from(0)?,
    ///         Dmf::try_from(0b1010)?,
    ///         *b"jpn",
    ///         Format::HORIZONTAL_960_X_540,
    ///         Tcs::EIGHT_BIT_CODE,
    ///         RollupMode::NON_ROLL_UP,
    ///     )],
    ///     &mut bytes,
    /// )?;
    /// assert_eq!(bytes, [0x1a, 0x6a, 0x70, 0x6e, 0x80]);
    /// # }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from(
        languages: impl IntoIterator<Item = Language>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        Self::write_entries(languages.into_iter().map(Ok), out)
    }

    /// Appends the bytes of `entries` to `out`, leaving `out` as it was if an
    /// entry is an error or is refused
    #[cfg(feature = "alloc")]
    pub(super) fn write_entries(
        entries: impl IntoIterator<Item = Result<Language, Error>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        write_or_truncate(out, |out| {
            for language in entries {
                language?.write(out)?;
            }

            Ok(())
        })
    }
}

impl<'input> From<&'input [u8]> for LanguageLoop<'input> {
    fn from(bytes: &'input [u8]) -> Self {
        Self(bytes)
    }
}

impl<'input> IntoIterator for LanguageLoop<'input> {
    type Item = Result<Language, Error>;
    type IntoIter = LanguageIter<'input>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the entries of a [`LanguageLoop`]
///
/// An entry the bytes end inside is an error, and the last item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LanguageIter<'input>(&'input [u8]);

impl Iterator for LanguageIter<'_> {
    type Item = Result<Language, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }

        // Why end the walk on an error: the length of an entry depends on
        // its DMF, so the entries after one that cannot be read cannot be
        // located.
        let (item, rest) = match frame(self.0) {
            Ok((language, rest)) => (Ok(language), rest),
            Err(error) => (Err(error), &[][..]),
        };
        self.0 = rest;

        Some(item)
    }
}

/// Splits `bytes` after the `num_languages` entries at their head, into the
/// loop of them and the bytes after it
pub(super) fn read_loop(
    bytes: &[u8],
    num_languages: u8,
) -> Result<(LanguageLoop<'_>, &[u8]), Error> {
    let mut after_languages = bytes;
    for _ in 0..num_languages {
        let (_, next) = frame(after_languages)?;
        after_languages = next;
    }
    let languages = bytes
        .get(..bytes.len().saturating_sub(after_languages.len()))
        .unwrap_or_default();

    Ok((LanguageLoop::from(languages), after_languages))
}

/// Reads the entry at the head of `bytes`, and returns it with the bytes
/// after it
fn frame(bytes: &[u8]) -> Result<(Language, &[u8]), Error> {
    let mut reader = FieldReader::new(bytes);
    let field = |field| move |error| Error::Field { field, error };
    // Why not name each field of a byte in its error: the byte is read whole
    // or not at all, so only the first field of it can be cut off.
    let language_tag = reader.read_u8::<3>().map_err(field("language_tag"))?;
    // Why not keep the reserved bit: it is written '1'.
    reader.read_u8::<1>().map_err(field("language_tag"))?;
    let dmf = Dmf::from_4_bits(reader.read_u8::<4>().map_err(field("language_tag"))?);
    let dc = if matches!(dmf.get(), 0b1100..=0b1110) {
        Some(Dc::from(reader.read_u8::<8>().map_err(field("DC"))?))
    } else {
        None
    };
    let code = reader
        .read_bytes(3)
        .map_err(field("ISO_639_language_code"))?;
    let format = reader.read_u8::<4>().map_err(field("Format"))?;
    let tcs = reader.read_u8::<2>().map_err(field("Format"))?;
    let rollup_mode = reader.read_u8::<2>().map_err(field("Format"))?;
    let rest = reader
        .read_bytes(reader.remaining_bytes())
        .unwrap_or_default();
    let mut language = Language::new(
        LanguageTag::from_3_bits(language_tag),
        dmf,
        code.try_into().unwrap_or_default(),
        Format::from_4_bits(format),
        Tcs::from_2_bits(tcs),
        RollupMode::from_2_bits(rollup_mode),
    );
    language.dc = dc;

    Ok((language, rest))
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec;
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{Language, LanguageLoop};
    use crate::caption::Dmf;
    #[cfg(feature = "alloc")]
    use crate::caption::Error;
    use crate::caption::management::{Dc, Format, LanguageTag, RollupMode, Tcs};

    /// The language of the caption management data of the Task 26 spike,
    /// which ffprobe's libaribcaption read: `language_tag` 0 / reserved
    /// `'1'` / DMF `'1010'` (0x1A), `jpn`, Format `'1000'` / TCS `'00'` /
    /// `rollup_mode` `'00'` (0x80)
    const SPIKE: [u8; 5] = [0x1a, 0x6a, 0x70, 0x6e, 0x80];

    /// Built by hand from Table 9-3: `language_tag` 0 / reserved `'1'` /
    /// DMF `'1100'` (0x1C), DC 0x00, `jpn`, Format `'1000'` / TCS `'00'` /
    /// `rollup_mode` `'00'` (0x80)
    const WITH_DC: [u8; 6] = [0x1c, 0x00, 0x6a, 0x70, 0x6e, 0x80];

    fn japanese(dmf: u8) -> Language {
        Language::new(
            LanguageTag::try_from(0).unwrap(),
            Dmf::try_from(dmf).unwrap(),
            *b"jpn",
            Format::HORIZONTAL_960_X_540,
            Tcs::EIGHT_BIT_CODE,
            RollupMode::NON_ROLL_UP,
        )
    }

    #[test]
    fn a_language_loop_yields_each_language_with_its_display_condition() {
        let mut with_dc = japanese(0b1100);
        with_dc.dc = Some(Dc::RAIN_ATTENUATION_MESSAGE);

        assert_eq!(
            [&SPIKE[..], &WITH_DC[..]].map(|bytes| {
                let mut languages = LanguageLoop::from(bytes).iter();
                [languages.next(), languages.next()]
            }),
            [
                [Some(Ok(japanese(0b1010))), None],
                [Some(Ok(with_dc)), None]
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_language_is_written_back_as_read() {
        let mut with_dc = japanese(0b1100);
        with_dc.dc = Some(Dc::RAIN_ATTENUATION_MESSAGE);
        let mut written = Vec::new();

        assert_eq!(
            LanguageLoop::write_from([japanese(0b1010), with_dc], &mut written),
            Ok(())
        );
        assert_eq!(written, [&SPIKE[..], &WITH_DC[..]].concat());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_reserved_or_inconsistent_language_is_refused_and_leaves_the_output_as_it_was() {
        let mut tcs = japanese(0b1010);
        tcs.tcs = Tcs::try_from(0b10).unwrap();
        let mut rollup_mode = japanese(0b1010);
        rollup_mode.rollup_mode = RollupMode::try_from(0b11).unwrap();
        let mut format = japanese(0b1010);
        format.format = Format::try_from(0b0101).unwrap();
        let mut dc = japanese(0b1010);
        dc.dc = Some(Dc::RAIN_ATTENUATION_MESSAGE);

        assert_eq!(
            [
                tcs,
                rollup_mode,
                format,
                japanese(0b0111),
                japanese(0b1100),
                dc
            ]
            .map(|language| {
                let mut written = Vec::from([0x47]);
                let result = LanguageLoop::write_from([japanese(0b1010), language], &mut written);
                (result, written)
            }),
            [
                Err(Error::Forbidden {
                    field: "TCS",
                    value: 0b10
                }),
                Err(Error::Forbidden {
                    field: "rollup_mode",
                    value: 0b11
                }),
                Err(Error::Forbidden {
                    field: "Format",
                    value: 0b0101
                }),
                Err(Error::Forbidden {
                    field: "DMF",
                    value: 0b0111
                }),
                Err(Error::Presence { field: "DC" }),
                Err(Error::Presence { field: "DC" }),
            ]
            .map(|error| (error, vec![0x47]))
        );
    }
}
