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
