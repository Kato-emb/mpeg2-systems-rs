//! ISO 639 language descriptor, which names the languages of a program
//! element

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{DescriptorTag, Error, RawDescriptor, body_of};
#[cfg(feature = "alloc")]
use super::{field_error, write_descriptor};

/// Bytes of an entry: `ISO_639_language_code` and `audio_type`
const ENTRY_LENGTH: usize = 4;

/// Purpose of an audio stream in a language
///
/// `audio_type`, ISO/IEC 13818-1 §2.6.19, Table 2-63
///
/// 0x04 to 0x7F are user private, which
/// [`is_user_private`](Self::is_user_private) tests, and 0x85 to 0xFF are
/// reserved.
#[doc(alias = "audio_type")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioType(u8);

impl AudioType {
    /// Undefined: the purpose of the stream is unknown
    pub const UNDEFINED: Self = Self(0x00);
    /// Clean effects: the program element has no language
    pub const CLEAN_EFFECTS: Self = Self(0x01);
    /// Hearing impaired
    pub const HEARING_IMPAIRED: Self = Self(0x02);
    /// Visual impaired commentary
    pub const VISUAL_IMPAIRED_COMMENTARY: Self = Self(0x03);
    /// Primary: the default recommended language of the program
    pub const PRIMARY: Self = Self(0x80);
    /// Native: the language the program was produced in
    pub const NATIVE: Self = Self(0x81);
    /// Emergency event information
    pub const EMERGENCY: Self = Self(0x82);
    /// Primary commentary
    pub const PRIMARY_COMMENTARY: Self = Self(0x83);
    /// Alternate commentary
    pub const ALTERNATE_COMMENTARY: Self = Self(0x84);

    /// Returns the 8-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns whether the audio type is user private, `0x04` to `0x7F`
    #[must_use]
    pub const fn is_user_private(self) -> bool {
        matches!(self.0, 0x04..=0x7f)
    }
}

impl From<u8> for AudioType {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

/// Entry of an [`Iso639LanguageDescriptor`]: a language and the purpose of
/// the audio in it
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Language {
    /// Code of ISO 639-2, three characters of ISO 8859-1
    pub iso_639_language_code: [u8; 3],
    /// Purpose of the audio in the language
    pub audio_type: AudioType,
}

impl Language {
    /// Creates the entry of a language
    #[must_use]
    pub const fn new(iso_639_language_code: [u8; 3], audio_type: AudioType) -> Self {
        Self {
            iso_639_language_code,
            audio_type,
        }
    }

    /// Returns the bytes of the entry, refusing a reserved `audio_type`
    #[cfg(feature = "alloc")]
    const fn to_bytes(self) -> Result<[u8; ENTRY_LENGTH], Error> {
        let audio_type = self.audio_type.get();
        if audio_type >= 0x85 {
            return Err(Error::Forbidden {
                field: "audio_type",
                value: audio_type as u32,
            });
        }
        let [first, second, third] = self.iso_639_language_code;

        Ok([first, second, third, audio_type])
    }
}

/// The languages of an [`Iso639LanguageDescriptor`], borrowed
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
    /// [`Forbidden`](Error::Forbidden): an `audio_type` of 0x85 to 0xFF,
    /// reserved.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::descriptor::iso_639_language::{
    ///     AudioType, Iso639LanguageDescriptor, Language, LanguageLoop,
    /// };
    ///
    /// let mut languages = Vec::new();
    /// LanguageLoop::write_from(
    ///     [
    ///         Language::new(*b"eng", AudioType::HEARING_IMPAIRED),
    ///         Language::new(*b"spa", AudioType::VISUAL_IMPAIRED_COMMENTARY),
    ///     ],
    ///     &mut languages,
    /// )?;
    /// let descriptor = Iso639LanguageDescriptor::new(LanguageLoop::from(languages.as_slice()));
    ///
    /// let mut written = Vec::new();
    /// descriptor.write(&mut written)?;
    /// assert_eq!(
    ///     written,
    ///     [0x0a, 0x08, 0x65, 0x6e, 0x67, 0x02, 0x73, 0x70, 0x61, 0x03]
    /// );
    /// # }
    /// # Ok::<(), mpeg2_systems::descriptor::Error>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from(
        languages: impl IntoIterator<Item = Language>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        let start = out.len();
        for language in languages {
            match language.to_bytes() {
                Ok(bytes) => out.extend_from_slice(&bytes),
                Err(error) => {
                    out.truncate(start);
                    return Err(error);
                }
            }
        }

        Ok(())
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
/// Bytes too few for an entry at the end are an [`Error::BodyLength`], and
/// the last item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LanguageIter<'input>(&'input [u8]);

impl Iterator for LanguageIter<'_> {
    type Item = Result<Language, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }

        let Some(([first, second, third, audio_type], rest)) =
            self.0.split_first_chunk::<ENTRY_LENGTH>()
        else {
            let length = self.0.len();
            self.0 = &[];
            return Some(Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::ISO_639_LANGUAGE,
                length,
            }));
        };
        self.0 = rest;

        Some(Ok(Language::new(
            [*first, *second, *third],
            AudioType(*audio_type),
        )))
    }
}

/// ISO 639 language descriptor, which names the languages of a program
/// element
///
/// `ISO_639_language_descriptor`, ISO/IEC 13818-1 §2.6.18, Table 2-62,
/// semantics §2.6.19
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::iso_639_language::{
///     AudioType, Iso639LanguageDescriptor, Language,
/// };
///
/// // English for the hearing impaired, then Spanish visual impaired commentary
/// let bytes = [
///     0x0a, 0x08, 0x65, 0x6e, 0x67, 0x02, 0x73, 0x70, 0x61, 0x03,
/// ];
/// let descriptor = Iso639LanguageDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// let mut languages = descriptor.languages.iter();
/// assert_eq!(
///     languages.next().transpose()?,
///     Some(Language::new(*b"eng", AudioType::HEARING_IMPAIRED))
/// );
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "ISO_639_language_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Iso639LanguageDescriptor<'input> {
    /// The languages and the purposes of the audio in them
    pub languages: LanguageLoop<'input>,
}

impl<'input> Iso639LanguageDescriptor<'input> {
    /// Creates the descriptor of the `languages`
    #[must_use]
    pub const fn new(languages: LanguageLoop<'input>) -> Self {
        Self { languages }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        super::HEADER_LENGTH.saturating_add(self.languages.0.len())
    }

    /// Appends the descriptor to `out`, its languages written from the
    /// entries the loop reads
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`DescriptorLength`](Error::DescriptorLength): the body would be
    ///   longer than 255 bytes.
    /// * [`BodyLength`](Error::BodyLength): the loop ends inside an entry.
    /// * [`Forbidden`](Error::Forbidden): an `audio_type` of 0x85 to 0xFF,
    ///   reserved.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write_descriptor(
            out,
            DescriptorTag::ISO_639_LANGUAGE,
            self.languages.0.len(),
            |writer| {
                for language in self.languages {
                    writer
                        .write_bytes(&language?.to_bytes()?)
                        .map_err(field_error("ISO_639_language_code"))?;
                }
                Ok(())
            },
        )
    }
}

impl<'input> TryFrom<&RawDescriptor<'input>> for Iso639LanguageDescriptor<'input> {
    type Error = Error;

    /// Takes the languages of an ISO 639 language descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x0A.
    /// * [`BodyLength`](Error::BodyLength): the body is not a whole number of
    ///   4-byte entries.
    fn try_from(raw: &RawDescriptor<'input>) -> Result<Self, Self::Error> {
        let body = body_of(raw, DescriptorTag::ISO_639_LANGUAGE)?;
        if !body.len().is_multiple_of(ENTRY_LENGTH) {
            return Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::ISO_639_LANGUAGE,
                length: body.len(),
            });
        }

        Ok(Self::new(LanguageLoop(body)))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;
    use core::array;

    use super::{AudioType, Iso639LanguageDescriptor, Language, LanguageLoop};
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    /// English for the hearing impaired, then Spanish visual impaired
    /// commentary, the example of §2.6.19
    const TWO_LANGUAGES: [u8; 10] = [0x0a, 0x08, 0x65, 0x6e, 0x67, 0x02, 0x73, 0x70, 0x61, 0x03];

    #[test]
    fn each_language_is_read_with_its_audio_type() {
        let descriptor =
            Iso639LanguageDescriptor::try_from(&RawDescriptor::read(&TWO_LANGUAGES).unwrap())
                .unwrap();
        let mut languages = descriptor.languages.iter();

        assert_eq!(
            array::from_fn(|_| languages.next()),
            [
                Some(Ok(Language::new(*b"eng", AudioType::HEARING_IMPAIRED))),
                Some(Ok(Language::new(
                    *b"spa",
                    AudioType::VISUAL_IMPAIRED_COMMENTARY
                ))),
                None
            ]
        );
    }

    #[test]
    fn a_body_that_is_not_whole_entries_is_refused_and_its_loop_ends_at_the_fraction() {
        let body = [0x65, 0x6e, 0x67, 0x02, 0x73, 0x70];
        let mut languages = LanguageLoop::from(body.as_slice()).iter();

        assert_eq!(
            (
                Iso639LanguageDescriptor::try_from(&RawDescriptor::new(
                    DescriptorTag::ISO_639_LANGUAGE,
                    &body
                )),
                array::from_fn(|_| languages.next())
            ),
            (
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::ISO_639_LANGUAGE,
                    length: 6
                }),
                [
                    Some(Ok(Language::new(*b"eng", AudioType::HEARING_IMPAIRED))),
                    Some(Err(Error::BodyLength {
                        descriptor_tag: DescriptorTag::ISO_639_LANGUAGE,
                        length: 2
                    })),
                    None
                ]
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_descriptor_built_from_its_languages_is_written_as_laid_out_by_hand() {
        let mut languages = Vec::new();
        LanguageLoop::write_from(
            [
                Language::new(*b"eng", AudioType::HEARING_IMPAIRED),
                Language::new(*b"spa", AudioType::VISUAL_IMPAIRED_COMMENTARY),
            ],
            &mut languages,
        )
        .unwrap();
        let mut written = Vec::new();
        Iso639LanguageDescriptor::new(LanguageLoop::from(languages.as_slice()))
            .write(&mut written)
            .unwrap();

        assert_eq!(written, TWO_LANGUAGES);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_reserved_audio_type_is_refused_before_a_byte_is_written() {
        let reserved = [0x65, 0x6e, 0x67, 0x85];
        let mut written = Vec::new();

        assert_eq!(
            [
                LanguageLoop::write_from(
                    [Language::new(*b"eng", AudioType::from(0x85))],
                    &mut written
                ),
                Iso639LanguageDescriptor::new(LanguageLoop::from(reserved.as_slice()))
                    .write(&mut written),
            ],
            [
                Err(Error::Forbidden {
                    field: "audio_type",
                    value: 0x85
                }),
                Err(Error::Forbidden {
                    field: "audio_type",
                    value: 0x85
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
