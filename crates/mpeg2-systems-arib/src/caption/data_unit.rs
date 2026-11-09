//! Data unit, the unit caption data carries its statement body, DRCS and
//! other contents in

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use mpeg2_systems::field::FieldReader;

use super::Error;
#[cfg(feature = "alloc")]
use super::write_or_truncate;

/// Bytes `data_unit_size` and `data_unit_loop_length` count at most, the 24
/// bits they are
#[cfg(feature = "alloc")]
const MAX_LENGTH: usize = 0xff_ffff;

/// `unit_separator` of every data unit
const UNIT_SEPARATOR: u8 = 0x1f;

/// Bytes from `unit_separator` to `data_unit_size`
const HEADER_LENGTH: usize = 5;

/// Data unit, a piece of caption data of one kind
///
/// `data_unit`, ARIB STD-B24 Volume 1 Part 3 §9.4, Table 9-11
///
/// `unit_separator` is 0x1F in every data unit, and `data_unit_size` follows
/// from the data, so neither is a field. The crate does not read the data:
/// 8-unit code characters, DRCS patterns and bitmaps are the caller's.
///
/// # Examples
///
/// ```
/// use mpeg2_systems_arib::caption::data_unit::{DataUnit, DataUnitParameter};
///
/// // The statement body あ, in the 8-unit code
/// let unit = DataUnit::new(DataUnitParameter::STATEMENT_BODY, &[0x24, 0x22]);
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// unit.write(&mut written)?;
/// assert_eq!(written, [0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x22]);
/// # }
/// # Ok::<(), mpeg2_systems_arib::caption::Error>(())
/// ```
#[doc(alias = "data_unit")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataUnit<'input> {
    /// Kind of the data
    pub data_unit_parameter: DataUnitParameter,
    /// The data
    pub data_unit_data_byte: &'input [u8],
}

impl<'input> DataUnit<'input> {
    /// Creates a data unit of `data_unit_parameter` carrying
    /// `data_unit_data_byte`
    #[must_use]
    pub const fn new(
        data_unit_parameter: DataUnitParameter,
        data_unit_data_byte: &'input [u8],
    ) -> Self {
        Self {
            data_unit_parameter,
            data_unit_data_byte,
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        HEADER_LENGTH.saturating_add(self.data_unit_data_byte.len())
    }

    /// Appends the data unit to `out`, with its `unit_separator` and
    /// `data_unit_size`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`DataUnitDataLength`](Error::DataUnitDataLength): the data is longer
    /// than the 0xFFFFFF bytes `data_unit_size` counts.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let length = self.data_unit_data_byte.len();
        let size = u24(length).ok_or(Error::DataUnitDataLength { length })?;
        out.push(UNIT_SEPARATOR);
        out.push(self.data_unit_parameter.get());
        out.extend_from_slice(&size);
        out.extend_from_slice(self.data_unit_data_byte);

        Ok(())
    }
}

/// Kind of the data of a data unit
///
/// `data_unit_parameter`, ARIB STD-B24 Volume 1 Part 3 §9.4, Table 9-12
#[doc(alias = "data_unit_parameter")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataUnitParameter(u8);

impl DataUnitParameter {
    /// 0x20: the characters of a caption statement, or the display area of
    /// caption management
    pub const STATEMENT_BODY: Self = Self(0x20);
    /// 0x28: geometric graphics
    pub const GEOMETRIC: Self = Self(0x28);
    /// 0x2C: synthesized sound
    pub const SYNTHESIZED_SOUND: Self = Self(0x2c);
    /// 0x30: 1-byte DRCS patterns
    pub const ONE_BYTE_DRCS: Self = Self(0x30);
    /// 0x31: 2-byte DRCS patterns
    pub const TWO_BYTE_DRCS: Self = Self(0x31);
    /// 0x34: a color map
    pub const COLOR_MAP: Self = Self(0x34);
    /// 0x35: a bitmap
    pub const BIT_MAP: Self = Self(0x35);

    /// Returns the 8 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<u8> for DataUnitParameter {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

/// A loop of data units, borrowed
///
/// The bytes are read data unit by data unit as [`iter`](Self::iter) walks
/// them, so a view of any bytes can be made; a data unit the bytes end
/// inside is an error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataUnitLoop<'input>(&'input [u8]);

impl<'input> DataUnitLoop<'input> {
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

    /// Returns an iterator over the data units, each read as it is reached
    #[must_use]
    pub const fn iter(self) -> DataUnitIter<'input> {
        DataUnitIter(self.0)
    }

    /// Appends the bytes of a loop of `units` to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// The errors of [`DataUnit::write`].
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems_arib::caption::data_unit::{DataUnit, DataUnitLoop, DataUnitParameter};
    ///
    /// let mut bytes = Vec::new();
    /// DataUnitLoop::write_from(
    ///     [DataUnit::new(DataUnitParameter::STATEMENT_BODY, &[0x24, 0x22])],
    ///     &mut bytes,
    /// )?;
    /// assert_eq!(bytes, [0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x22]);
    /// # }
    /// # Ok::<(), mpeg2_systems_arib::caption::Error>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from<'unit>(
        units: impl IntoIterator<Item = DataUnit<'unit>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        write_or_truncate(out, |out| {
            for unit in units {
                unit.write(out)?;
            }

            Ok(())
        })
    }
}

impl<'input> From<&'input [u8]> for DataUnitLoop<'input> {
    fn from(bytes: &'input [u8]) -> Self {
        Self(bytes)
    }
}

impl<'input> IntoIterator for DataUnitLoop<'input> {
    type Item = Result<DataUnit<'input>, Error>;
    type IntoIter = DataUnitIter<'input>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the data units of a [`DataUnitLoop`]
///
/// A data unit that cannot be read is an error, and the last item: nothing
/// after it can be located.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataUnitIter<'input>(&'input [u8]);

impl<'input> Iterator for DataUnitIter<'input> {
    type Item = Result<DataUnit<'input>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }

        // Why end the walk on an error: the bytes after a data unit that
        // cannot be read have no known position.
        let (item, rest) = match frame(self.0) {
            Ok((unit, rest)) => (Ok(unit), rest),
            Err(error) => (Err(error), &[][..]),
        };
        self.0 = rest;

        Some(item)
    }
}

/// Reads the data unit at the head of `bytes`, and returns it with the
/// bytes after it
fn frame(bytes: &[u8]) -> Result<(DataUnit<'_>, &[u8]), Error> {
    let mut reader = FieldReader::new(bytes);
    let field = |field| move |error| Error::Field { field, error };
    let found = reader.read_u8::<8>().map_err(field("unit_separator"))?;
    if found != UNIT_SEPARATOR {
        return Err(Error::UnitSeparator { found });
    }
    let data_unit_parameter = reader
        .read_u8::<8>()
        .map_err(field("data_unit_parameter"))?;
    let data_unit_size = reader.read_u32::<24>().map_err(field("data_unit_size"))?;
    let available_bytes = reader.remaining_bytes();
    let data_unit_data_byte = usize::try_from(data_unit_size)
        .ok()
        .and_then(|length| reader.read_bytes(length).ok())
        .ok_or(Error::DataUnitSize {
            data_unit_size,
            available_bytes,
        })?;
    let rest = reader
        .read_bytes(reader.remaining_bytes())
        .unwrap_or_default();

    Ok((
        DataUnit::new(DataUnitParameter(data_unit_parameter), data_unit_data_byte),
        rest,
    ))
}

/// Reads `data_unit_loop_length` and the loop after it, which `bytes` end
/// with
pub(super) fn read_loop(bytes: &[u8]) -> Result<DataUnitLoop<'_>, Error> {
    let mut reader = FieldReader::new(bytes);
    let data_unit_loop_length = reader.read_u32::<24>().map_err(|error| Error::Field {
        field: "data_unit_loop_length",
        error,
    })?;
    let units = reader
        .read_bytes(reader.remaining_bytes())
        .unwrap_or_default();
    let data_unit_loop_length = usize::try_from(data_unit_loop_length).unwrap_or(usize::MAX);
    if data_unit_loop_length != units.len() {
        return Err(Error::DataUnitLoopLength {
            data_unit_loop_length,
            available_bytes: units.len(),
        });
    }

    Ok(DataUnitLoop(units))
}

/// Returns the `data_unit_loop_length` of `units`, refusing a loop longer
/// than it counts
#[cfg(feature = "alloc")]
pub(super) fn loop_length(units: DataUnitLoop<'_>) -> Result<[u8; 3], Error> {
    let data_unit_loop_length = units.0.len();
    u24(data_unit_loop_length).ok_or(Error::DataUnitLoopLength {
        data_unit_loop_length,
        available_bytes: MAX_LENGTH,
    })
}

/// Returns `length` as the 3 bytes of a 24-bit length, if it fits them
#[cfg(feature = "alloc")]
fn u24(length: usize) -> Option<[u8; 3]> {
    let [_, bytes @ ..] = u32::try_from(length)
        .ok()
        .filter(|_| length <= MAX_LENGTH)?
        .to_be_bytes();

    Some(bytes)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec;
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{DataUnit, DataUnitLoop, DataUnitParameter};
    use crate::caption::Error;

    /// The data unit of the caption statement あ of the Task 26 spike: a
    /// statement body of the two bytes 0x24 0x22
    #[cfg(feature = "alloc")]
    const STATEMENT_BODY: [u8; 7] = [0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x22];

    #[test]
    fn a_loop_yields_each_data_unit_with_its_parameter_and_data() {
        let bytes = [
            0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x22, 0x1f, 0x30, 0x00, 0x00, 0x00,
        ];
        let mut units = DataUnitLoop::from(&bytes[..]).iter();

        assert_eq!(
            [units.next(), units.next(), units.next()],
            [
                Some(Ok(DataUnit::new(
                    DataUnitParameter::STATEMENT_BODY,
                    &[0x24, 0x22]
                ))),
                Some(Ok(DataUnit::new(DataUnitParameter::ONE_BYTE_DRCS, &[]))),
                None,
            ]
        );
    }

    #[test]
    fn a_data_unit_past_the_loop_or_without_its_separator_ends_the_loop() {
        assert_eq!(
            [
                &[0x1f, 0x20, 0x00, 0x00, 0x05, 0x24, 0x22][..],
                &[0x1e, 0x20, 0x00, 0x00, 0x00, 0x1f, 0x20, 0x00, 0x00, 0x00],
            ]
            .map(|bytes| {
                let mut units = DataUnitLoop::from(bytes).iter();
                [units.next(), units.next()]
            }),
            [
                [
                    Some(Err(Error::DataUnitSize {
                        data_unit_size: 5,
                        available_bytes: 2
                    })),
                    None
                ],
                [Some(Err(Error::UnitSeparator { found: 0x1e })), None],
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn data_units_are_written_back_as_read() {
        let mut written = Vec::new();

        assert_eq!(
            DataUnitLoop::write_from(
                DataUnitLoop::from(&STATEMENT_BODY[..])
                    .iter()
                    .map(Result::unwrap),
                &mut written
            ),
            Ok(())
        );
        assert_eq!(written, STATEMENT_BODY);
        assert_eq!(
            DataUnit::new(DataUnitParameter::STATEMENT_BODY, &[0x24, 0x22]).encoded_len(),
            STATEMENT_BODY.len()
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn data_past_0xffffff_bytes_is_refused_and_leaves_the_output_as_it_was() {
        let data = vec![0; 0x100_0000];
        let mut written = Vec::from([0x47]);

        assert_eq!(
            DataUnitLoop::write_from(
                [
                    DataUnit::new(DataUnitParameter::STATEMENT_BODY, &[0x24, 0x22]),
                    DataUnit::new(DataUnitParameter::BIT_MAP, &data),
                ],
                &mut written
            ),
            Err(Error::DataUnitDataLength { length: 0x100_0000 })
        );
        assert_eq!(written, [0x47]);
    }
}
