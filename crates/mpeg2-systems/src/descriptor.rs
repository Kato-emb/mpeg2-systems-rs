//! Descriptors, which extend the definition of a program or a program
//! element, and the loops that carry them
//!
//! A table carries its descriptors as a [`DescriptorLoop`], which is read
//! descriptor by descriptor as it is walked. Each descriptor comes out as a
//! [`RawDescriptor`]: its tag and its body, whatever the tag. A typed
//! descriptor is converted from a [`RawDescriptor`] with `TryFrom`, and a
//! crate that defines descriptors of its own, for the user private tags,
//! implements the same `TryFrom` for its types. A tag nobody converts stays a
//! [`RawDescriptor`] and is written back as read.
//!
//! A typed descriptor reads the values its specification reserves or forbids
//! and keeps them, but its `write` refuses them, so a descriptor read is not
//! always written back as it came.
//!
//! The descriptors of tags 0x02 to 0x0A have a module each: [`video_stream`],
//! [`audio_stream`], [`hierarchy`], [`registration`],
//! [`data_stream_alignment`], [`target_background_grid`], [`video_window`],
//! [`ca`] and [`iso_639_language`]; those of tags 0x0B to 0x12 too:
//! [`system_clock`], [`multiplex_buffer_utilization`], [`copyright`],
//! [`maximum_bitrate`], [`private_data_indicator`], [`smoothing_buffer`],
//! [`system_target_decoder`] (the STD descriptor) and [`ibp`]; and the codec
//! descriptors [`avc_video`], [`mpeg2_aac_audio`] and [`hevc_video`].
//!
//! # Examples
//!
//! ```
//! use mpeg2_systems::descriptor::{DescriptorLoop, RawDescriptor};
//!
//! /// A descriptor a crate defines for the user private tag 0x52
//! struct StreamIdentifier {
//!     component_tag: u8,
//! }
//!
//! impl TryFrom<&RawDescriptor<'_>> for StreamIdentifier {
//!     type Error = &'static str;
//!
//!     fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
//!         match (raw.descriptor_tag.get(), raw.body) {
//!             (0x52, [component_tag]) => Ok(Self {
//!                 component_tag: *component_tag,
//!             }),
//!             _ => Err("not a stream identifier descriptor"),
//!         }
//!     }
//! }
//!
//! // A registration descriptor, then a stream identifier descriptor
//! let bytes = [0x05, 0x04, 0x48, 0x44, 0x4d, 0x56, 0x52, 0x01, 0x30];
//! let mut descriptors = DescriptorLoop::from(bytes.as_slice()).iter();
//!
//! let registration = descriptors.next().unwrap()?;
//! assert_eq!(registration.body, b"HDMV");
//! let stream_identifier = StreamIdentifier::try_from(&descriptors.next().unwrap()?);
//! assert_eq!(stream_identifier.map(|descriptor| descriptor.component_tag), Ok(0x30));
//! assert!(descriptors.next().is_none());
//! # Ok::<(), mpeg2_systems::descriptor::Error>(())
//! ```

use core::fmt;

use crate::field;

pub mod audio_stream;
pub mod avc_video;
pub mod ca;
pub mod copyright;
pub mod data_stream_alignment;
pub mod hevc_video;
pub mod hierarchy;
pub mod ibp;
pub mod iso_639_language;
pub mod maximum_bitrate;
pub mod mpeg2_aac_audio;
pub mod multiplex_buffer_utilization;
pub mod private_data_indicator;
mod raw;
pub mod registration;
pub mod smoothing_buffer;
pub mod system_clock;
pub mod system_target_decoder;
pub mod tag;
pub mod target_background_grid;
mod typed;
pub mod video_stream;
pub mod video_window;

pub use raw::{DescriptorIter, DescriptorLoop, RawDescriptor};
pub use tag::DescriptorTag;

/// Failure to read or write a descriptor or a descriptor loop
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The bytes end before `descriptor_length`
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field ran past the end
        error: field::Error,
    },
    /// The bytes after `descriptor_length` are not as many as it declares
    Length {
        /// The `descriptor_length` read
        descriptor_length: u8,
        /// The bytes given after `descriptor_length`
        available_bytes: usize,
    },
    /// The body to write is longer than the 255 bytes `descriptor_length`
    /// counts
    DescriptorLength {
        /// The bytes of body given
        length: usize,
    },
    /// The `descriptor_tag` to write is 0x01, which Table 2-45 forbids
    ForbiddenTag,
    /// The descriptor converted is of another tag than the type takes
    DescriptorTag {
        /// The tag the type takes
        expected: DescriptorTag,
        /// The tag of the descriptor
        found: DescriptorTag,
    },
    /// The body of the descriptor converted is not as long as the syntax of
    /// its tag
    BodyLength {
        /// The tag of the descriptor
        descriptor_tag: DescriptorTag,
        /// The bytes of body found
        length: usize,
    },
    /// A field to write has a value its semantics forbid or reserve
    Forbidden {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The value given
        value: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read or written")
            }
            Self::Length {
                descriptor_length,
                available_bytes,
            } => write!(
                formatter,
                "descriptor_length {descriptor_length} does not match the {available_bytes} \
                 bytes given"
            ),
            Self::DescriptorLength { length } => write!(
                formatter,
                "descriptor body of {length} bytes exceeds the 255 descriptor_length counts"
            ),
            Self::ForbiddenTag => write!(formatter, "descriptor_tag 0x01 is forbidden"),
            Self::DescriptorTag { expected, found } => write!(
                formatter,
                "descriptor_tag {:#04x} is not {:#04x}, the tag of the descriptor type",
                found.get(),
                expected.get()
            ),
            Self::BodyLength {
                descriptor_tag,
                length,
            } => write!(
                formatter,
                "a body of {length} bytes does not fit the syntax of descriptor_tag {:#04x}",
                descriptor_tag.get()
            ),
            Self::Forbidden { field, value } => write!(formatter, "{field} {value} is forbidden"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::Length { .. }
            | Self::DescriptorLength { .. }
            | Self::ForbiddenTag
            | Self::DescriptorTag { .. }
            | Self::BodyLength { .. }
            | Self::Forbidden { .. } => None,
        }
    }
}
