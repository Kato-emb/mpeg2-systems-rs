//! Caption management data, which names the languages of the captions and
//! how they are shown

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use mpeg2_systems::field::FieldReader;

use super::Error;
use super::data_group::DataGroup;
#[cfg(feature = "alloc")]
use super::data_unit::DataUnitParameter;
use super::data_unit::{self, DataUnitLoop};
use super::time::{self, BcdTime, Tmd};
#[cfg(feature = "alloc")]
use super::write_or_truncate;

mod language;
mod language_field;

pub use language::{Language, LanguageIter, LanguageLoop};
pub use language_field::{Dc, Format, LanguageTag, RollupMode, Tcs};

/// Most languages caption management data names, §9.3
#[cfg(feature = "alloc")]
const MAX_LANGUAGES: u8 = 8;

/// Whether caption management data of `tmd` has an `OTM`
fn has_otm(tmd: Tmd) -> bool {
    tmd == Tmd::OFFSET_TIME
}

/// Whether Table 9-13 leaves a data unit of `parameter` out of caption
/// management
#[cfg(feature = "alloc")]
fn not_for_management(parameter: DataUnitParameter) -> bool {
    matches!(
        parameter,
        DataUnitParameter::GEOMETRIC
            | DataUnitParameter::SYNTHESIZED_SOUND
            | DataUnitParameter::BIT_MAP
    )
}

/// Caption management data, the languages of the captions of an elementary
/// stream and how each is shown
///
/// `caption_management_data`, ARIB STD-B24 Volume 1 Part 3 §9.3.1, Table 9-3
///
/// `num_languages` and `data_unit_loop_length` follow from the loops, so
/// neither is a field. `otm` is present exactly when `tmd` is
/// [`Tmd::OFFSET_TIME`]; [`write`](Self::write) refuses it otherwise.
///
/// # Examples
///
/// ```
/// use mpeg2_systems_arib::caption::data_group::{DataGroup, DataGroupId};
/// use mpeg2_systems_arib::caption::data_unit::DataUnitLoop;
/// use mpeg2_systems_arib::caption::management::{CaptionManagementData, LanguageLoop};
/// use mpeg2_systems_arib::caption::{BcdTime, Tmd};
///
/// // Offset time 01:23:45.678, one language, jpn, and no data unit
/// let bytes = [
///     0xbf, 0x01, 0x23, 0x45, 0x67, 0x8f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00, 0x00, 0x00,
/// ];
/// let group = DataGroup::new(DataGroupId::try_from(0x00)?, &bytes);
/// let data = CaptionManagementData::try_from(&group)?;
/// assert_eq!(
///     data,
///     CaptionManagementData::new(
///         Tmd::OFFSET_TIME,
///         LanguageLoop::from(&bytes[7..12]),
///         DataUnitLoop::from(&[][..])
///     )
///     .with_otm(BcdTime::new(1, 23, 45, 678)?)
/// );
///
/// // Written back (with the `alloc` feature) as the data of a data group
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// data.write(&mut written)?;
/// assert_eq!(written, bytes);
/// let mut group_bytes = Vec::new();
/// DataGroup::new(DataGroupId::try_from(0x00)?, &written).write(&mut group_bytes)?;
/// # }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[doc(alias = "caption_management_data")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptionManagementData<'input> {
    /// What the time of the captions is counted from
    pub tmd: Tmd,
    /// Offset added to the time of the captions, when `tmd` is
    /// [`Tmd::OFFSET_TIME`]
    pub otm: Option<BcdTime>,
    /// The languages of the captions
    pub languages: LanguageLoop<'input>,
    /// Data units every caption of the elementary stream uses
    pub data_units: DataUnitLoop<'input>,
}

impl<'input> CaptionManagementData<'input> {
    /// Creates the caption management data of `languages` and `data_units`,
    /// without an `OTM`
    #[must_use]
    pub const fn new(
        tmd: Tmd,
        languages: LanguageLoop<'input>,
        data_units: DataUnitLoop<'input>,
    ) -> Self {
        Self {
            tmd,
            otm: None,
            languages,
            data_units,
        }
    }

    /// Returns the value with `otm` present, set to the one given
    #[must_use]
    pub const fn with_otm(mut self, otm: BcdTime) -> Self {
        self.otm = Some(otm);
        self
    }

    /// Appends the caption management data to `out`, as the
    /// `data_group_data_byte` of a data group
    ///
    /// Each language and data unit is written again from its fields, so the
    /// reserved bits are `'1'` whatever the bytes of the loops hold. A write
    /// that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `TMD` is `'11'`, reserved; a digit
    ///   of `OTM` is above 9; a language has a value [`LanguageLoop::write_from`]
    ///   refuses; or a data unit is geometric, synthesized sound or a bitmap,
    ///   which Table 9-13 does not give to caption management.
    /// * [`Presence`](Error::Presence): `OTM` is present with another `TMD`
    ///   than `'10'`, or absent with it; or a language has a `DC` against its
    ///   `DMF`.
    /// * [`NumLanguages`](Error::NumLanguages): more than the 8 languages
    ///   §9.3 allows.
    /// * [`DataUnitLoopLength`](Error::DataUnitLoopLength): the data units
    ///   are longer than 0xFFFFFF bytes.
    /// * The errors of [`LanguageLoop::iter`] and [`DataUnitLoop::iter`], if
    ///   a loop was made of bytes that cannot be read.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        time::check_header(self.tmd, self.otm, "OTM", has_otm)?;
        let count = self.languages.iter().try_fold(0_usize, |count, language| {
            language.map(|_| count.saturating_add(1))
        })?;
        let num_languages = u8::try_from(count)
            .ok()
            .filter(|&count| count <= MAX_LANGUAGES)
            .ok_or(Error::NumLanguages { count })?;
        let data_unit_loop_length = data_unit::check_loop(self.data_units, not_for_management)?;
        write_or_truncate(out, |out| {
            time::write_header(out, self.tmd, self.otm);
            out.push(num_languages);
            for language in self.languages.iter() {
                language?.write(out)?;
            }
            out.extend_from_slice(&data_unit_loop_length);
            data_unit::write_units(self.data_units, out)
        })
    }
}

impl<'input> TryFrom<&DataGroup<'input>> for CaptionManagementData<'input> {
    type Error = Error;

    /// Reads the caption management data a data group carries
    ///
    /// # Errors
    ///
    /// * [`DataGroupId`](Error::DataGroupId): the `data_group_id` is not
    ///   0x00 or 0x20, those of caption management (Table 9-2).
    /// * [`Field`](Error::Field): the data ends inside a field.
    /// * [`DataUnitLoopLength`](Error::DataUnitLoopLength):
    ///   `data_unit_loop_length` does not match the bytes after it.
    fn try_from(group: &DataGroup<'input>) -> Result<Self, Self::Error> {
        let found = group.data_group_id;
        if !matches!(found.get(), 0x00 | 0x20) {
            return Err(Error::DataGroupId { found });
        }
        let bytes = group.data_group_data_byte;
        let mut reader = FieldReader::new(bytes);
        let (tmd, otm) = time::read_header(&mut reader, "OTM", has_otm)?;
        let num_languages = reader.read_u8::<8>().map_err(|error| Error::Field {
            field: "num_languages",
            error,
        })?;
        let rest = reader
            .read_bytes(reader.remaining_bytes())
            .unwrap_or_default();
        let mut after_languages = rest;
        for _ in 0..num_languages {
            let (_, next) = language::frame(after_languages)?;
            after_languages = next;
        }
        let languages = rest
            .get(..rest.len().saturating_sub(after_languages.len()))
            .unwrap_or_default();

        Ok(Self {
            tmd,
            otm,
            languages: LanguageLoop::from(languages),
            data_units: data_unit::read_loop(after_languages)?,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec;
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{CaptionManagementData, LanguageLoop};
    #[cfg(feature = "alloc")]
    use super::{Format, Language, LanguageTag, RollupMode, Tcs};
    #[cfg(feature = "alloc")]
    use crate::caption::Dmf;
    use crate::caption::data_group::{DataGroup, DataGroupId};
    use crate::caption::data_unit::DataUnitLoop;
    use crate::caption::{BcdTime, Error, Tmd};

    /// The caption management data of the Task 26 spike, which ffprobe's
    /// libaribcaption read: TMD `'00'`, one language (`language_tag` 0, DMF
    /// `'1010'`, `jpn`, Format `'1000'`, TCS `'00'`, `rollup_mode` `'00'`)
    /// and no data unit
    const SPIKE: [u8; 10] = [0x3f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00, 0x00, 0x00];

    /// Built by hand from Table 9-3: TMD `'10'` and Reserved `'111111'`
    /// (0xBF), OTM 01:23:45.678 and Reserved `'1111'` (0x01 0x23 0x45 0x67
    /// 0x8F), `num_languages` 0, `data_unit_loop_length` 0
    const WITH_OTM: [u8; 10] = [0xbf, 0x01, 0x23, 0x45, 0x67, 0x8f, 0x00, 0x00, 0x00, 0x00];

    /// Built by hand from Table 9-3: TMD `'00'`, `num_languages` 1,
    /// `language_tag` 0 / reserved `'1'` / DMF `'1100'` (0x1C), DC 0x00,
    /// `jpn`, Format `'1000'` / TCS `'00'` / `rollup_mode` `'00'` (0x80),
    /// `data_unit_loop_length` 0
    const WITH_DC: [u8; 11] = [
        0x3f, 0x01, 0x1c, 0x00, 0x6a, 0x70, 0x6e, 0x80, 0x00, 0x00, 0x00,
    ];

    fn group(id: u8, bytes: &[u8]) -> DataGroup<'_> {
        DataGroup::new(DataGroupId::try_from(id).unwrap(), bytes)
    }

    #[cfg(feature = "alloc")]
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
    fn management_data_built_from_new_and_every_with_method_equals_one_with_every_field_assigned() {
        let mut assigned = CaptionManagementData::new(
            Tmd::OFFSET_TIME,
            LanguageLoop::from(&SPIKE[2..7]),
            DataUnitLoop::from(&[][..]),
        );
        assigned.otm = Some(BcdTime::new(1, 23, 45, 678).unwrap());

        let built = CaptionManagementData::new(
            Tmd::OFFSET_TIME,
            LanguageLoop::from(&SPIKE[2..7]),
            DataUnitLoop::from(&[][..]),
        )
        .with_otm(BcdTime::new(1, 23, 45, 678).unwrap());

        assert_eq!(built, assigned);
    }

    #[test]
    fn caption_management_data_is_read_with_its_languages() {
        let mut with_otm = CaptionManagementData::new(
            Tmd::OFFSET_TIME,
            LanguageLoop::from(&[][..]),
            DataUnitLoop::from(&[][..]),
        );
        with_otm.otm = Some(BcdTime::new(1, 23, 45, 678).unwrap());

        assert_eq!(
            [&SPIKE[..], &WITH_OTM, &WITH_DC]
                .map(|bytes| CaptionManagementData::try_from(&group(0x20, bytes))),
            [
                Ok(CaptionManagementData::new(
                    Tmd::FREE,
                    LanguageLoop::from(&SPIKE[2..7]),
                    DataUnitLoop::from(&[][..])
                )),
                Ok(with_otm),
                Ok(CaptionManagementData::new(
                    Tmd::FREE,
                    LanguageLoop::from(&WITH_DC[2..8]),
                    DataUnitLoop::from(&[][..])
                )),
            ]
        );
    }

    #[test]
    fn data_of_another_data_group_or_with_bytes_after_its_data_units_is_refused() {
        let mut longer = [0; SPIKE.len() + 1];
        longer
            .get_mut(..SPIKE.len())
            .unwrap()
            .copy_from_slice(&SPIKE);

        assert_eq!(
            [group(0x01, &SPIKE), group(0x00, &longer)]
                .map(|group| CaptionManagementData::try_from(&group)),
            [
                Err(Error::DataGroupId {
                    found: DataGroupId::try_from(0x01).unwrap()
                }),
                Err(Error::DataUnitLoopLength {
                    data_unit_loop_length: 0,
                    available_bytes: 1
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn caption_management_data_is_written_back_as_read() {
        let written = [&SPIKE[..], &WITH_OTM, &WITH_DC].map(|bytes| {
            let mut written = Vec::new();
            CaptionManagementData::try_from(&group(0x00, bytes))
                .unwrap()
                .write(&mut written)
                .unwrap();
            written
        });

        assert_eq!(
            written,
            [SPIKE.to_vec(), WITH_OTM.to_vec(), WITH_DC.to_vec()]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn caption_management_data_the_tables_do_not_allow_is_refused_and_leaves_the_output_as_it_was()
    {
        let empty = |tmd| {
            CaptionManagementData::new(
                tmd,
                LanguageLoop::from(&[][..]),
                DataUnitLoop::from(&[][..]),
            )
        };
        let mut not_bcd = empty(Tmd::OFFSET_TIME);
        not_bcd.otm = CaptionManagementData::try_from(&group(
            0x00,
            &[0xbf, 0x0a, 0x23, 0x45, 0x67, 0x8f, 0x00, 0x00, 0x00, 0x00],
        ))
        .unwrap()
        .otm;
        let mut otm_without_offset = empty(Tmd::FREE);
        otm_without_offset.otm = Some(BcdTime::new(0, 0, 0, 0).unwrap());
        let mut nine = Vec::new();
        LanguageLoop::write_from([japanese(0b1010); 9], &mut nine).unwrap();

        assert_eq!(
            [
                empty(Tmd::try_from(0b11).unwrap()),
                empty(Tmd::OFFSET_TIME),
                otm_without_offset,
                not_bcd,
                CaptionManagementData::new(
                    Tmd::FREE,
                    LanguageLoop::from(nine.as_slice()),
                    DataUnitLoop::from(&[][..])
                ),
            ]
            .map(|data| {
                let mut written = Vec::from([0x47]);
                let result = data.write(&mut written);
                (result, written)
            }),
            [
                Err(Error::Forbidden {
                    field: "TMD",
                    value: 0b11
                }),
                Err(Error::Presence { field: "OTM" }),
                Err(Error::Presence { field: "OTM" }),
                Err(Error::Forbidden {
                    field: "OTM",
                    value: 0x0_a234_5678
                }),
                Err(Error::NumLanguages { count: 9 }),
            ]
            .map(|error| (error, vec![0x47]))
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn eight_languages_are_written() {
        let mut eight = Vec::new();
        LanguageLoop::write_from([japanese(0b1010); 8], &mut eight).unwrap();
        let mut written = Vec::new();

        assert_eq!(
            CaptionManagementData::new(
                Tmd::FREE,
                LanguageLoop::from(eight.as_slice()),
                DataUnitLoop::from(&[][..])
            )
            .write(&mut written),
            Ok(())
        );
        assert_eq!(written.get(1), Some(&8));
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn languages_and_data_units_are_written_again_from_their_fields() {
        let data = |languages: &'static [u8], data_units: &'static [u8]| {
            CaptionManagementData::new(
                Tmd::FREE,
                LanguageLoop::from(languages),
                DataUnitLoop::from(data_units),
            )
        };
        let mut written = Vec::new();

        // The reserved bit read as '0' is written '1'
        assert_eq!(
            data(
                &[0x0a, 0x6a, 0x70, 0x6e, 0x80],
                &[0x1f, 0x20, 0x00, 0x00, 0x00]
            )
            .write(&mut written),
            Ok(())
        );
        assert_eq!(
            written,
            [
                0x3f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00, 0x00, 0x05, 0x1f, 0x20, 0x00, 0x00,
                0x00
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn languages_or_data_units_that_cannot_be_written_are_refused_and_leave_the_output_as_it_was() {
        let data = |languages: &'static [u8], data_units: &'static [u8]| {
            CaptionManagementData::new(
                Tmd::FREE,
                LanguageLoop::from(languages),
                DataUnitLoop::from(data_units),
            )
        };
        let spike = &[0x1a, 0x6a, 0x70, 0x6e, 0x80][..];

        assert_eq!(
            [
                data(
                    &[0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x1a, 0x6a, 0x70, 0x6e, 0x50],
                    &[]
                ),
                data(&[0x1a, 0x6a], &[]),
                data(spike, &[0x1e, 0x20, 0x00, 0x00, 0x00]),
                data(
                    spike,
                    &[0x1f, 0x20, 0x00, 0x00, 0x00, 0x1f, 0x28, 0x00, 0x00, 0x00]
                ),
                data(spike, &[0x1f, 0x2c, 0x00, 0x00, 0x00]),
                data(spike, &[0x1f, 0x35, 0x00, 0x00, 0x00]),
            ]
            .map(|data| {
                let mut written = Vec::from([0x47]);
                let result = data.write(&mut written);
                (result, written)
            }),
            [
                Err(Error::Forbidden {
                    field: "Format",
                    value: 0b0101
                }),
                Err(Error::Field {
                    field: "ISO_639_language_code",
                    error: mpeg2_systems::field::Error::Truncated {
                        required_bits: 24,
                        remaining_bits: 8
                    }
                }),
                Err(Error::UnitSeparator { found: 0x1e }),
                Err(Error::Forbidden {
                    field: "data_unit_parameter",
                    value: 0x28
                }),
                Err(Error::Forbidden {
                    field: "data_unit_parameter",
                    value: 0x2c
                }),
                Err(Error::Forbidden {
                    field: "data_unit_parameter",
                    value: 0x35
                }),
            ]
            .map(|error| (error, vec![0x47]))
        );
        let mut written = Vec::new();
        assert_eq!(
            data(spike, &[0x1f, 0x99, 0x00, 0x00, 0x00]).write(&mut written),
            Ok(())
        );
    }
}
