//! Program specific information: the sections that tables are carried in

pub mod section;

use core::fmt;

/// Kind of table a section belongs to
///
/// `table_id`, ISO/IEC 13818-1 §2.4.4.5, Table 2-31
///
/// Every value has a meaning in Table 2-31, so any `u8` converts. The values
/// the table assigns one by one are associated constants; `0x40` to `0xFE`
/// are user private, which [`is_user_private`](Self::is_user_private) tests,
/// and `0xFF` is forbidden.
#[doc(alias = "table_id")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TableId(u8);

impl TableId {
    /// `program_association_section` (PAT)
    pub const PROGRAM_ASSOCIATION: Self = Self(0x00);
    /// `conditional_access_section` (CAT)
    pub const CONDITIONAL_ACCESS: Self = Self(0x01);
    /// `TS_program_map_section` (PMT)
    pub const TS_PROGRAM_MAP: Self = Self(0x02);
    /// `TS_description_section`
    pub const TS_DESCRIPTION: Self = Self(0x03);
    /// `ISO_IEC_14496_scene_description_section`
    pub const ISO_IEC_14496_SCENE_DESCRIPTION: Self = Self(0x04);
    /// `ISO_IEC_14496_object_descriptor_section`
    pub const ISO_IEC_14496_OBJECT_DESCRIPTOR: Self = Self(0x05);
    /// `Metadata_section`
    pub const METADATA: Self = Self(0x06);
    /// IPMP Control Information Section, defined in ISO/IEC 13818-11
    pub const IPMP_CONTROL_INFORMATION: Self = Self(0x07);
    /// `ISO_IEC_14496_section`
    pub const ISO_IEC_14496: Self = Self(0x08);
    /// ISO/IEC 23001-11 (Green access unit) section
    pub const ISO_IEC_23001_11_GREEN_ACCESS_UNIT: Self = Self(0x09);
    /// ISO/IEC 23001-10 (Quality access unit) section
    pub const ISO_IEC_23001_10_QUALITY_ACCESS_UNIT: Self = Self(0x0a);
    /// ISO/IEC 23001-13 (Media Orchestration access unit) section
    pub const ISO_IEC_23001_13_MEDIA_ORCHESTRATION_ACCESS_UNIT: Self = Self(0x0b);
    /// Forbidden: a byte of this value where a section would start is
    /// stuffing that runs to the end of the Transport Packet
    pub const FORBIDDEN: Self = Self(0xff);

    /// Returns the 8-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns whether the table is user private, `0x40` to `0xFE`
    #[must_use]
    pub const fn is_user_private(self) -> bool {
        matches!(self.0, 0x40..=0xfe)
    }

    /// Returns whether the table is one of the PSI tables ISO/IEC 13818-1
    /// defines with the `'1'`, `'0'` bits before `section_length`: PAT, CAT,
    /// PMT and the transport stream description table
    #[must_use]
    pub const fn is_psi_table(self) -> bool {
        self.0 <= Self::TS_DESCRIPTION.0
    }

    /// Returns the largest `section_length` a section of the table may have
    ///
    /// 1021 for the PSI tables of [`is_psi_table`](Self::is_psi_table), whose
    /// sections are at most 1024 bytes, and 4093 for every other table, whose
    /// sections follow the `private_section` syntax and are at most 4096
    /// bytes (§2.4.4.6, §2.4.4.10, §2.4.4.12). A section never needs a larger
    /// buffer than this, so gathering one takes no limit from the caller.
    #[must_use]
    pub const fn max_section_length(self) -> u16 {
        if self.is_psi_table() { 1021 } else { 4093 }
    }
}

impl From<u8> for TableId {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

/// Version of a table, which changes whenever its content does
///
/// `version_number`, ISO/IEC 13818-1 §2.4.4.6, §2.4.4.12
///
/// The version is incremented by 1 modulo 32 at each change. The crate reads
/// and writes it; telling a new version from a repeated one is the
/// demultiplexer's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VersionNumber(u8);

impl VersionNumber {
    /// Number of distinct versions, 32
    pub const MODULUS: u8 = 32;

    /// Returns the 5-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns the version that follows, 31 wrapping to 0
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1).rem_euclid(Self::MODULUS))
    }
}

impl TryFrom<u8> for VersionNumber {
    type Error = VersionNumberOutOfRange;

    /// Takes a 5-bit value
    ///
    /// # Errors
    ///
    /// [`VersionNumberOutOfRange`]: `value` is 32 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < Self::MODULUS {
            Ok(Self(value))
        } else {
            Err(VersionNumberOutOfRange { value })
        }
    }
}

/// Failure to make a [`VersionNumber`] of a value 5 bits cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VersionNumberOutOfRange {
    value: u8,
}

impl VersionNumberOutOfRange {
    /// Returns the value given, 32 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }
}

impl fmt::Display for VersionNumberOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "version_number {} does not fit in 5 bits",
            self.value
        )
    }
}

impl core::error::Error for VersionNumberOutOfRange {}

#[cfg(test)]
mod tests {
    use super::{TableId, VersionNumber, VersionNumberOutOfRange};

    #[test]
    fn a_version_number_takes_any_5_bit_value_and_wraps_from_31_to_0() {
        let version = |value| VersionNumber::try_from(value).unwrap();

        assert_eq!(
            (
                VersionNumber::try_from(32),
                version(4).next(),
                version(31).next()
            ),
            (
                Err(VersionNumberOutOfRange { value: 32 }),
                version(5),
                version(0)
            )
        );
    }

    #[test]
    fn psi_tables_are_bounded_tighter_than_private_ones() {
        assert_eq!(
            [0x00, 0x03, 0x04, 0x3f, 0x40, 0xfe]
                .map(|value| TableId::from(value).max_section_length()),
            [1021, 1021, 4093, 4093, 4093, 4093]
        );
    }
}
