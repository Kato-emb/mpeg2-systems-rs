//! The kinds of table a section belongs to

/// Kind of table a section belongs to
///
/// `table_id`, ISO/IEC 13818-1 §2.4.4.5, Table 2-31
///
/// Every value has a meaning in Table 2-31, so any `u8` converts. The values
/// the table assigns one by one are associated constants; `0x40` to `0xFE`
/// are user private, which [`is_user_private`](Self::is_user_private) tests,
/// and `0xFF` is forbidden.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::psi::TableId;
///
/// // A PMT section, bounded as a PSI table
/// let pmt = TableId::from(0x02);
/// assert_eq!(pmt, TableId::TS_PROGRAM_MAP);
/// assert_eq!(pmt.max_section_length(), 1021);
///
/// // A user private table, bounded as a private section
/// let private = TableId::from(0x80);
/// assert!(private.is_user_private());
/// assert_eq!(private.max_section_length(), 4093);
/// ```
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

#[cfg(test)]
mod tests {
    use super::TableId;

    #[test]
    fn psi_tables_are_bounded_tighter_than_private_ones() {
        assert_eq!(
            [0x00, 0x03, 0x04, 0x3f, 0x40, 0xfe]
                .map(|value| TableId::from(value).max_section_length()),
            [1021, 1021, 4093, 4093, 4093, 4093]
        );
    }
}
