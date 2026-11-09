//! Caption statement data, the body of a caption: its text and the other
//! data units shown with it

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use mpeg2_systems::field::FieldReader;

use super::Error;
use super::data_group::DataGroup;
use super::data_unit::{self, DataUnitLoop};
use super::time::{self, BcdTime, Tmd};
#[cfg(feature = "alloc")]
use super::write_or_truncate;

/// Whether caption statement data of `tmd` has an `STM`
fn has_stm(tmd: Tmd) -> bool {
    tmd == Tmd::REAL_TIME || tmd == Tmd::OFFSET_TIME
}

/// Caption statement data, the body of a caption in one language: its time
/// and the data units of its text and other contents
///
/// `caption_data`, ARIB STD-B24 Volume 1 Part 3 §9.3.2, Table 9-10
///
/// `data_unit_loop_length` follows from the loop, so it is not a field.
/// `stm` is present exactly when `tmd` is [`Tmd::REAL_TIME`] or
/// [`Tmd::OFFSET_TIME`]; [`write`](Self::write) refuses it otherwise.
///
/// # Examples
///
/// ```
/// use mpeg2_systems_arib::caption::Tmd;
/// use mpeg2_systems_arib::caption::data_group::{DataGroup, DataGroupId};
/// use mpeg2_systems_arib::caption::data_unit::{DataUnit, DataUnitParameter};
/// use mpeg2_systems_arib::caption::statement::CaptionData;
///
/// // The statement body あ, in the first language
/// let bytes = [0x3f, 0x00, 0x00, 0x07, 0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x22];
/// let data = CaptionData::try_from(&DataGroup::new(DataGroupId::try_from(0x01)?, &bytes))?;
/// assert_eq!(data.tmd, Tmd::FREE);
/// assert_eq!(
///     data.data_units.iter().next().transpose()?,
///     Some(DataUnit::new(DataUnitParameter::STATEMENT_BODY, &[0x24, 0x22]))
/// );
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// data.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[doc(alias("caption_data", "caption statement data"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptionData<'input> {
    /// What the time of the caption is counted from
    pub tmd: Tmd,
    /// Time the caption is shown from, when `tmd` is [`Tmd::REAL_TIME`] or
    /// [`Tmd::OFFSET_TIME`]
    pub stm: Option<BcdTime>,
    /// The text of the caption and the other contents shown with it
    pub data_units: DataUnitLoop<'input>,
}

impl<'input> CaptionData<'input> {
    /// Creates the caption statement data of `data_units`, without an `STM`
    #[must_use]
    pub const fn new(tmd: Tmd, data_units: DataUnitLoop<'input>) -> Self {
        Self {
            tmd,
            stm: None,
            data_units,
        }
    }

    /// Appends the caption statement data to `out`, as the
    /// `data_group_data_byte` of a data group
    ///
    /// Each data unit is written again from its fields. A write that fails
    /// leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `TMD` is `'11'`, reserved, or a
    ///   digit of `STM` is above 9.
    /// * [`Presence`](Error::Presence): `STM` is present with `TMD` `'00'`,
    ///   or absent with `'01'` or `'10'`; or there is no data unit, of which
    ///   §9.3.2 asks for at least one.
    /// * [`DataUnitLoopLength`](Error::DataUnitLoopLength): the data units
    ///   are longer than 0xFFFFFF bytes.
    /// * The errors of [`DataUnitLoop::iter`], if the loop was made of bytes
    ///   that cannot be read.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        time::check_header(self.tmd, self.stm, "STM", has_stm)?;
        if self.data_units.is_empty() {
            return Err(Error::Presence { field: "data_unit" });
        }
        let data_unit_loop_length = data_unit::check_loop(self.data_units, |_| false)?;
        write_or_truncate(out, |out| {
            time::write_header(out, self.tmd, self.stm);
            out.extend_from_slice(&data_unit_loop_length);
            data_unit::write_units(self.data_units, out)
        })
    }
}

impl<'input> TryFrom<&DataGroup<'input>> for CaptionData<'input> {
    type Error = Error;

    /// Reads the caption statement data a data group carries
    ///
    /// # Errors
    ///
    /// * [`DataGroupId`](Error::DataGroupId): the `data_group_id` is not
    ///   0x01 to 0x08 or 0x21 to 0x28, those of the caption statements of
    ///   the eight languages (Table 9-2).
    /// * [`Field`](Error::Field): the data ends inside a field.
    /// * [`DataUnitLoopLength`](Error::DataUnitLoopLength):
    ///   `data_unit_loop_length` does not match the bytes after it.
    fn try_from(group: &DataGroup<'input>) -> Result<Self, Self::Error> {
        let found = group.data_group_id;
        if !matches!(found.get(), 0x01..=0x08 | 0x21..=0x28) {
            return Err(Error::DataGroupId { found });
        }
        let mut reader = FieldReader::new(group.data_group_data_byte);
        let (tmd, stm) = time::read_header(&mut reader, "STM", has_stm)?;
        let rest = reader
            .read_bytes(reader.remaining_bytes())
            .unwrap_or_default();

        Ok(Self {
            tmd,
            stm,
            data_units: data_unit::read_loop(rest)?,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec;
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::CaptionData;
    use crate::caption::data_group::{DataGroup, DataGroupId};
    use crate::caption::data_unit::DataUnitLoop;
    use crate::caption::{BcdTime, Error, Tmd};

    /// The caption statement data あ of the Task 26 spike, which ffprobe's
    /// libaribcaption decoded into a subtitle: TMD `'00'` and one data unit
    /// of statement body 0x24 0x22
    const SPIKE: [u8; 11] = [
        0x3f, 0x00, 0x00, 0x07, 0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x22,
    ];

    /// Built by hand from Table 9-10: TMD `'01'` and Reserved `'111111'`
    /// (0x7F), STM 00:00:01.500 and Reserved `'1111'` (0x00 0x00 0x01 0x50
    /// 0x0F), `data_unit_loop_length` 7, and from Table 9-11 one data unit:
    /// `unit_separator` 0x1F, statement body 0x20, `data_unit_size` 2, 0x24 0x22
    const WITH_STM: [u8; 16] = [
        0x7f, 0x00, 0x00, 0x01, 0x50, 0x0f, 0x00, 0x00, 0x07, 0x1f, 0x20, 0x00, 0x00, 0x02, 0x24,
        0x22,
    ];

    fn group(id: u8, bytes: &[u8]) -> DataGroup<'_> {
        DataGroup::new(DataGroupId::try_from(id).unwrap(), bytes)
    }

    #[test]
    fn caption_statement_data_is_read_with_its_data_units() {
        let mut with_stm = CaptionData::new(Tmd::REAL_TIME, DataUnitLoop::from(&WITH_STM[9..]));
        with_stm.stm = Some(BcdTime::new(0, 0, 1, 500).unwrap());

        assert_eq!(
            [group(0x01, &SPIKE), group(0x28, &WITH_STM)]
                .map(|group| CaptionData::try_from(&group)),
            [
                Ok(CaptionData::new(Tmd::FREE, DataUnitLoop::from(&SPIKE[4..]))),
                Ok(with_stm),
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
            [
                group(0x00, &SPIKE),
                group(0x09, &SPIKE),
                group(0x29, &SPIKE),
                group(0x01, &longer)
            ]
            .map(|group| CaptionData::try_from(&group)),
            [
                Err(Error::DataGroupId {
                    found: DataGroupId::try_from(0x00).unwrap()
                }),
                Err(Error::DataGroupId {
                    found: DataGroupId::try_from(0x09).unwrap()
                }),
                Err(Error::DataGroupId {
                    found: DataGroupId::try_from(0x29).unwrap()
                }),
                Err(Error::DataUnitLoopLength {
                    data_unit_loop_length: 7,
                    available_bytes: 8
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn caption_statement_data_is_written_back_as_read() {
        let written = [&SPIKE[..], &WITH_STM].map(|bytes| {
            let mut written = Vec::new();
            CaptionData::try_from(&group(0x01, bytes))
                .unwrap()
                .write(&mut written)
                .unwrap();
            written
        });

        assert_eq!(written, [SPIKE.to_vec(), WITH_STM.to_vec()]);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn caption_statement_data_the_tables_do_not_allow_is_refused_and_leaves_the_output_as_it_was() {
        let empty = |tmd| CaptionData::new(tmd, DataUnitLoop::from(&[][..]));
        let mut stm_without_time = empty(Tmd::FREE);
        stm_without_time.stm = Some(BcdTime::new(0, 0, 0, 0).unwrap());
        let mut long = vec![0; 0x100_0004];
        long.get_mut(..5)
            .unwrap()
            .copy_from_slice(&[0x1f, 0x20, 0xff, 0xff, 0xff]);

        assert_eq!(
            [
                empty(Tmd::try_from(0b11).unwrap()),
                empty(Tmd::REAL_TIME),
                stm_without_time,
                CaptionData::new(Tmd::FREE, DataUnitLoop::from(long.as_slice())),
                empty(Tmd::FREE),
                CaptionData::new(
                    Tmd::FREE,
                    DataUnitLoop::from(&[0x1f, 0x20, 0x00, 0x00, 0x05, 0x24][..])
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
                Err(Error::Presence { field: "STM" }),
                Err(Error::Presence { field: "STM" }),
                Err(Error::DataUnitLoopLength {
                    data_unit_loop_length: 0x100_0004,
                    available_bytes: 0xff_ffff
                }),
                Err(Error::Presence { field: "data_unit" }),
                Err(Error::DataUnitSize {
                    data_unit_size: 5,
                    available_bytes: 1
                }),
            ]
            .map(|error| (error, vec![0x47]))
        );
    }
}
