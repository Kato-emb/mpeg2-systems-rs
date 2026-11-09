//! ARIB STD-B10 descriptors, converted from a
//! [`RawDescriptor`](mpeg2_systems::descriptor::RawDescriptor) with `TryFrom`
//!
//! A typed descriptor reads the values its specification reserves and keeps
//! them where it has a field for them, but its `write` refuses them, as the
//! descriptors of `mpeg2-systems` do.
//!
//! - [`stream_identifier`]: the stream identifier descriptor (tag 0x52)
//! - [`data_component`]: the data component descriptor (tag 0xFD) and the
//!   caption information it carries

pub mod data_component;
pub mod stream_identifier;

/// Bytes of `descriptor_tag` and `descriptor_length`
const HEADER_LENGTH: usize = 2;
