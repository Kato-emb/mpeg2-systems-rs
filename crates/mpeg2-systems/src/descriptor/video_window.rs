//! Video window descriptor, which places the window of a video stream on the
//! target background grid

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::write_descriptor;
use super::{DescriptorTag, Error, RawDescriptor, field_error, fixed_body};
use crate::field::FieldReader;

/// Bytes of the body
const BODY_LENGTH: usize = 4;

/// Video window descriptor, which places the window of a video stream on the
/// target background grid
///
/// `video_window_descriptor`, ISO/IEC 13818-1 §2.6.14, Table 2-60, semantics
/// §2.6.15
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::video_window::VideoWindowDescriptor;
///
/// // A window at (16, 8) of priority 15
/// let bytes = [0x08, 0x04, 0x00, 0x40, 0x00, 0x8f];
/// let descriptor = VideoWindowDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor, VideoWindowDescriptor::new(16, 8, 15));
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "video_window_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoWindowDescriptor {
    /// Column of the grid the top left pixel of the window is on, 14 bits
    pub horizontal_offset: u16,
    /// Row of the grid the top left pixel of the window is on, 14 bits
    pub vertical_offset: u16,
    /// How the window overlaps others, 0 the lowest to 15 the highest
    pub window_priority: u8,
}

impl VideoWindowDescriptor {
    /// Creates the descriptor of a window
    #[must_use]
    pub const fn new(horizontal_offset: u16, vertical_offset: u16, window_priority: u8) -> Self {
        Self {
            horizontal_offset,
            vertical_offset,
            window_priority,
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub const fn encoded_len(&self) -> usize {
        super::HEADER_LENGTH.saturating_add(BODY_LENGTH)
    }

    /// Appends the descriptor to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`Field`](Error::Field): an offset wider than 14 bits, or a
    /// `window_priority` wider than 4 bits.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write_descriptor(out, DescriptorTag::VIDEO_WINDOW, BODY_LENGTH, |writer| {
            writer
                .write_bits::<14>(self.horizontal_offset)
                .map_err(field_error("horizontal_offset"))?;
            writer
                .write_bits::<14>(self.vertical_offset)
                .map_err(field_error("vertical_offset"))?;
            writer
                .write_bits::<4>(self.window_priority)
                .map_err(field_error("window_priority"))
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for VideoWindowDescriptor {
    type Error = Error;

    /// Takes the fields of a video window descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x08.
    /// * [`BodyLength`](Error::BodyLength): the body is not 4 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::VIDEO_WINDOW)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            Ok(Self::new(
                reader.read_u16::<14>()?,
                reader.read_u16::<14>()?,
                reader.read_u8::<4>()?,
            ))
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::VideoWindowDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};
    #[cfg(feature = "alloc")]
    use crate::field;

    #[test]
    fn a_body_of_other_than_4_bytes_is_refused() {
        assert_eq!(
            VideoWindowDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::VIDEO_WINDOW,
                &[0x00, 0x40, 0x00, 0x8f, 0x00]
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::VIDEO_WINDOW,
                length: 5
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_value_wider_than_its_field_is_refused_before_a_byte_is_written() {
        let mut written = Vec::new();

        assert_eq!(
            VideoWindowDescriptor::new(16, 8, 16).write(&mut written),
            Err(Error::Field {
                field: "window_priority",
                error: field::Error::ValueTooWide {
                    width: 4,
                    value: 16
                }
            })
        );
        assert_eq!(written, []);
    }
}
