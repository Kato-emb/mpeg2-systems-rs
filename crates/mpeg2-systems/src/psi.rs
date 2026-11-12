//! Program specific information: the sections that tables are carried in
//!
//! A [`Section`](section::Section) is read from bytes, its `CRC_32` checked.
//! A table the crate defines, [`pat`], [`pmt`] or [`cat`], is converted from
//! a section with `TryFrom`, and a crate that defines tables of its own, for
//! a body that gives the user private `table_id` values a meaning, converts a
//! section the same way.

pub mod cat;
pub mod pat;
pub mod pmt;
pub mod section;
mod table_id;
mod version_number;

pub use table_id::TableId;
pub use version_number::{VersionNumber, VersionNumberOutOfRange};

/// Bytes of a section with the fields `section_syntax_indicator` `'1'`
/// announces, besides its body: up to `section_length`, up to
/// `last_section_number` and `CRC_32`
const LONG_SECTION_OVERHEAD: usize = section::PREFIX_LENGTH
    .saturating_add(section::SYNTAX_LENGTH)
    .saturating_add(section::CRC_32_LENGTH);

/// Refuses a section of `encoded_len` bytes whose `section_length` would be
/// larger than `table_id` allows, before its loops are walked or a buffer is
/// sized after them
#[cfg(feature = "alloc")]
const fn check_section_length(table_id: TableId, encoded_len: usize) -> Result<(), section::Error> {
    let section_length = encoded_len.saturating_sub(section::PREFIX_LENGTH);
    let limit = table_id.max_section_length();
    if section_length > limit as usize {
        return Err(section::Error::SectionLength {
            section_length,
            limit,
        });
    }

    Ok(())
}
