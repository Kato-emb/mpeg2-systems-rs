//! Data group, the unit caption data is cut into

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use mpeg2_systems::field::{self, FieldReader, TwoBitFieldOutOfRange};

use super::{Error, SixBitFieldOutOfRange};

/// Bytes from `data_group_id` to `data_group_size`
const HEADER_LENGTH: usize = 5;

/// Bytes of `CRC_16`
const CRC_16_LENGTH: usize = 2;

/// Data group, the unit caption management data and caption statement data
/// are carried in
///
/// `data_group`, ARIB STD-B24 Volume 1 Part 3 §9.2, Table 9-1
///
/// `data_group_size` follows from the data, and `CRC_16` is checked when the
/// group is read and computed when it is written, so neither is a field.
/// The crate does not read the data by its `data_group_id`, nor join the
/// groups a caption data is cut into by `data_group_link_number`.
///
/// # Examples
///
/// ```
/// use mpeg2_systems_arib::caption::data_group::{DataGroup, DataGroupId};
///
/// // A data group of caption management data, group A, the second of two
/// // linked data groups
/// let bytes = [
///     0x00, 0x01, 0x01, 0x00, 0x0a, 0x3f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00, 0x00, 0x00,
///     0x8e, 0x7a,
/// ];
/// let group = DataGroup::read(&bytes)?;
/// assert_eq!(
///     group,
///     DataGroup::new(DataGroupId::try_from(0)?, &bytes[5..15])
///         .with_data_group_link_number(1)
///         .with_last_data_group_link_number(1)
/// );
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// group.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[doc(alias = "data_group")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataGroup<'input> {
    /// Kind of the caption data, and the group, A or B, it belongs to
    pub data_group_id: DataGroupId,
    /// Version of the data group, one up each time its content changes
    pub data_group_version: DataGroupVersion,
    /// Number of this data group among those a caption data is cut into
    pub data_group_link_number: u8,
    /// Number of the last data group of the caption data
    pub last_data_group_link_number: u8,
    /// The caption data, or the part of it this data group carries
    pub data_group_data_byte: &'input [u8],
}

impl<'input> DataGroup<'input> {
    /// Creates a data group of `data_group_id` carrying
    /// `data_group_data_byte` whole, of version 0
    #[must_use]
    pub const fn new(data_group_id: DataGroupId, data_group_data_byte: &'input [u8]) -> Self {
        Self {
            data_group_id,
            data_group_version: DataGroupVersion(0),
            data_group_link_number: 0,
            last_data_group_link_number: 0,
            data_group_data_byte,
        }
    }

    /// Returns the value with the given `data_group_version`
    #[must_use]
    pub const fn with_data_group_version(mut self, data_group_version: DataGroupVersion) -> Self {
        self.data_group_version = data_group_version;
        self
    }

    /// Returns the value with the given `data_group_link_number`
    #[must_use]
    pub const fn with_data_group_link_number(mut self, data_group_link_number: u8) -> Self {
        self.data_group_link_number = data_group_link_number;
        self
    }

    /// Returns the value with the given `last_data_group_link_number`
    #[must_use]
    pub const fn with_last_data_group_link_number(
        mut self,
        last_data_group_link_number: u8,
    ) -> Self {
        self.last_data_group_link_number = last_data_group_link_number;
        self
    }

    /// Reads a data group from `bytes`, which hold it whole and nothing more
    ///
    /// # Errors
    ///
    /// * [`Field`](Error::Field): `bytes` end inside the fields up to
    ///   `data_group_size`, or inside `CRC_16`.
    /// * [`DataGroupSize`](Error::DataGroupSize): `data_group_size` does not
    ///   match the bytes before `CRC_16`.
    /// * [`Crc16`](Error::Crc16): `CRC_16` does not match the data group.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        let mut reader = FieldReader::new(bytes);
        let field = |field| move |error| Error::Field { field, error };
        let data_group_id = reader.read_u8::<6>().map_err(field("data_group_id"))?;
        let data_group_version = reader.read_u8::<2>().map_err(field("data_group_version"))?;
        let data_group_link_number = reader
            .read_u8::<8>()
            .map_err(field("data_group_link_number"))?;
        let last_data_group_link_number = reader
            .read_u8::<8>()
            .map_err(field("last_data_group_link_number"))?;
        let data_group_size = reader.read_u16::<16>().map_err(field("data_group_size"))?;
        let remaining_bits = reader.remaining_bits();
        let data_and_crc = reader
            .read_bytes(reader.remaining_bytes())
            .unwrap_or_default();
        let (data_group_data_byte, crc_field) = data_and_crc
            .split_last_chunk::<CRC_16_LENGTH>()
            .ok_or(Error::Field {
                field: "CRC_16",
                error: field::Error::Truncated {
                    required_bits: 16,
                    remaining_bits,
                },
            })?;
        if usize::from(data_group_size) != data_group_data_byte.len() {
            return Err(Error::DataGroupSize {
                data_group_size,
                available_bytes: data_group_data_byte.len(),
            });
        }
        let crc_16_read = u16::from_be_bytes(*crc_field);
        let computed = crc_16(
            bytes
                .get(..bytes.len().saturating_sub(CRC_16_LENGTH))
                .unwrap_or_default(),
        );
        if crc_16_read != computed {
            return Err(Error::Crc16 {
                crc_16: crc_16_read,
                computed,
            });
        }

        Ok(Self {
            data_group_id: DataGroupId(data_group_id),
            data_group_version: DataGroupVersion(data_group_version),
            data_group_link_number,
            last_data_group_link_number,
            data_group_data_byte,
        })
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        HEADER_LENGTH
            .saturating_add(self.data_group_data_byte.len())
            .saturating_add(CRC_16_LENGTH)
    }

    /// Appends the data group to `out`, with its `data_group_size` and
    /// `CRC_16`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`DataGroupDataLength`](Error::DataGroupDataLength): the data is
    /// longer than the 65535 bytes `data_group_size` counts.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let length = self.data_group_data_byte.len();
        let data_group_size =
            u16::try_from(length).map_err(|_| Error::DataGroupDataLength { length })?;
        let start = out.len();
        out.push(self.data_group_id.get() << 2 | self.data_group_version.get());
        out.push(self.data_group_link_number);
        out.push(self.last_data_group_link_number);
        out.extend_from_slice(&data_group_size.to_be_bytes());
        out.extend_from_slice(self.data_group_data_byte);
        let crc = crc_16(out.get(start..).unwrap_or_default());
        out.extend_from_slice(&crc.to_be_bytes());

        Ok(())
    }
}

/// Identification of a data group: which caption data it carries, and
/// whether it is of group A or B
///
/// `data_group_id`, ARIB STD-B24 Volume 1 Part 3 §9.2, Table 9-2
///
/// # Examples
///
/// ```
/// use mpeg2_systems_arib::caption::SixBitFieldOutOfRange;
/// use mpeg2_systems_arib::caption::data_group::DataGroupId;
///
/// assert_eq!(DataGroupId::try_from(0x21).map(DataGroupId::get), Ok(0x21));
/// // A data_group_id is 6 bits
/// assert_eq!(DataGroupId::try_from(64).map_err(SixBitFieldOutOfRange::value), Err(64));
/// ```
#[doc(alias("DGI", "data_group_id"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataGroupId(u8);

impl DataGroupId {
    /// Returns the 6 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for DataGroupId {
    type Error = SixBitFieldOutOfRange;

    /// Takes the 6 bits of `value`
    ///
    /// # Errors
    ///
    /// [`SixBitFieldOutOfRange`]: `value` is 64 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        SixBitFieldOutOfRange::check(value).map(Self)
    }
}

/// Version of a data group, one up modulo 4 each time the content of the
/// same `data_group_id` changes
///
/// `data_group_version`, ARIB STD-B24 Volume 1 Part 3 §9.2
///
/// # Examples
///
/// ```
/// use mpeg2_systems::field::TwoBitFieldOutOfRange;
/// use mpeg2_systems_arib::caption::data_group::DataGroupVersion;
///
/// assert_eq!(DataGroupVersion::try_from(3).map(DataGroupVersion::get), Ok(3));
/// // A data_group_version is 2 bits
/// assert_eq!(DataGroupVersion::try_from(4).map_err(TwoBitFieldOutOfRange::value), Err(4));
/// ```
#[doc(alias = "data_group_version")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataGroupVersion(u8);

impl DataGroupVersion {
    /// Returns the 2 bits
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for DataGroupVersion {
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

/// Returns the `CRC_16` of `bytes`, ARIB STD-B24 Volume 1 Part 3 §9.2
///
/// The division by G(X) = X^16 + X^12 + X^5 + 1 that §9.2 describes, from 0
/// and with nothing inverted: CRC-16/XMODEM of the CRC catalog.
fn crc_16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0, |crc, &byte| {
        (0..8).fold(crc ^ u16::from(byte) << 8, |crc, _| {
            if crc & 0x8000 == 0 {
                crc << 1
            } else {
                crc << 1 ^ 0x1021
            }
        })
    })
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use mpeg2_systems::field;

    use super::{DataGroup, DataGroupId, DataGroupVersion, crc_16};
    use crate::caption::Error;

    /// The data group of caption management data (DGI 0, one language
    /// `jpn`), from the synchronized PES data of the Task 26 spike, which
    /// ffprobe's libaribcaption decoded
    const MANAGEMENT: [u8; 17] = [
        0x00, 0x00, 0x00, 0x00, 0x0a, 0x3f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00, 0x00, 0x00,
        0xb0, 0x78,
    ];

    /// The data group of the caption statement あ (DGI 1), from the same
    /// spike
    const STATEMENT: [u8; 18] = [
        0x04, 0x00, 0x00, 0x00, 0x0b, 0x3f, 0x00, 0x00, 0x07, 0x1f, 0x20, 0x00, 0x00, 0x02, 0x24,
        0x22, 0x05, 0x71,
    ];

    fn data_group(id: u8, bytes: &[u8]) -> DataGroup<'_> {
        DataGroup::new(
            DataGroupId::try_from(id).unwrap(),
            bytes.get(5..bytes.len().saturating_sub(2)).unwrap(),
        )
    }

    #[test]
    fn a_data_group_built_from_new_and_every_with_method_equals_one_with_every_field_assigned() {
        let mut assigned = DataGroup::new(DataGroupId::try_from(1).unwrap(), &[0xaa]);
        assigned.data_group_version = DataGroupVersion::try_from(2).unwrap();
        assigned.data_group_link_number = 1;
        assigned.last_data_group_link_number = 3;

        let built = DataGroup::new(DataGroupId::try_from(1).unwrap(), &[0xaa])
            .with_data_group_version(DataGroupVersion::try_from(2).unwrap())
            .with_data_group_link_number(1)
            .with_last_data_group_link_number(3);

        assert_eq!(built, assigned);
    }

    #[test]
    fn a_data_group_is_read_and_its_crc_16_checked() {
        assert_eq!(
            [&MANAGEMENT[..], &STATEMENT].map(DataGroup::read),
            [
                Ok(data_group(0, &MANAGEMENT)),
                Ok(data_group(1, &STATEMENT))
            ]
        );
    }

    #[test]
    fn a_data_group_that_disagrees_with_its_size_or_its_crc_16_is_refused() {
        let mut longer = MANAGEMENT;
        longer[4] = 0x0b;
        let mut flipped = MANAGEMENT;
        flipped[16] ^= 0x01;

        assert_eq!(
            [&longer[..], &flipped, &MANAGEMENT[..2]].map(DataGroup::read),
            [
                Err(Error::DataGroupSize {
                    data_group_size: 0x0b,
                    available_bytes: 10
                }),
                Err(Error::Crc16 {
                    crc_16: 0xb079,
                    computed: 0xb078
                }),
                Err(Error::Field {
                    field: "last_data_group_link_number",
                    error: field::Error::Truncated {
                        required_bits: 8,
                        remaining_bits: 0
                    }
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_data_group_is_written_back_as_read() {
        let mut management = Vec::new();
        let mut statement = Vec::new();

        assert_eq!(
            [
                data_group(0, &MANAGEMENT).write(&mut management),
                data_group(1, &STATEMENT).write(&mut statement)
            ],
            [Ok(()), Ok(())]
        );
        assert_eq!(
            [management, statement],
            [MANAGEMENT.to_vec(), STATEMENT.to_vec()]
        );
        assert_eq!(data_group(1, &STATEMENT).encoded_len(), STATEMENT.len());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn data_past_65535_bytes_is_refused_and_leaves_the_output_as_it_was() {
        let data = [0; 65536];
        let mut written = Vec::from([0x47]);

        assert_eq!(
            DataGroup::new(DataGroupId::try_from(0).unwrap(), &data).write(&mut written),
            Err(Error::DataGroupDataLength { length: 65536 })
        );
        assert_eq!(written, [0x47]);
    }

    #[test]
    fn crc_16_is_the_crc_16_xmodem_of_the_crc_catalog() {
        assert_eq!(crc_16(b"123456789"), 0x31c3);
    }
}
