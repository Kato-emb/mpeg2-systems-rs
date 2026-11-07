//! Target background grid descriptor, which describes the grid of pixels
//! video windows are placed on

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{DescriptorTag, Error, RawDescriptor, fixed_body};
#[cfg(feature = "alloc")]
use super::{field_error, write_descriptor};
use crate::field::{FieldReader, FourBitFieldOutOfRange};

/// Bytes of the body
const BODY_LENGTH: usize = 4;

/// Sample or display aspect ratio of a picture
///
/// `aspect_ratio_information`, defined in Rec. ITU-T H.262 | ISO/IEC
/// 13818-2 §6.3.3, Table 6-3
///
/// 0 is forbidden and 5 to 15 are reserved.
#[doc(alias = "aspect_ratio_information")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AspectRatioInformation(u8);

impl AspectRatioInformation {
    /// Square samples, a sample aspect ratio of 1.0
    pub const SQUARE_SAMPLES: Self = Self(0b0001);
    /// A display aspect ratio of 3:4
    pub const DISPLAY_3_4: Self = Self(0b0010);
    /// A display aspect ratio of 9:16
    pub const DISPLAY_9_16: Self = Self(0b0011);
    /// A display aspect ratio of 2.21:1
    pub const DISPLAY_2_21_1: Self = Self(0b0100);

    /// Returns the 4-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for AspectRatioInformation {
    type Error = FourBitFieldOutOfRange;

    /// Takes a 4-bit value
    ///
    /// # Errors
    ///
    /// [`FourBitFieldOutOfRange`]: `value` is 16 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < 16 {
            Ok(Self(value))
        } else {
            Err(FourBitFieldOutOfRange { value })
        }
    }
}

/// Target background grid descriptor, which describes the grid of pixels
/// video windows are placed on
///
/// `target_background_grid_descriptor`, ISO/IEC 13818-1 §2.6.12, Table
/// 2-59, semantics §2.6.13
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::target_background_grid::{
///     AspectRatioInformation, TargetBackgroundGridDescriptor,
/// };
///
/// // A grid of 1920 by 1080 pixels of display aspect ratio 9:16
/// let bytes = [0x07, 0x04, 0x1e, 0x00, 0x43, 0x83];
/// let descriptor = TargetBackgroundGridDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(
///     descriptor,
///     TargetBackgroundGridDescriptor::new(1920, 1080, AspectRatioInformation::DISPLAY_9_16)
/// );
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "target_background_grid_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetBackgroundGridDescriptor {
    /// Width of the grid in pixels, 14 bits
    pub horizontal_size: u16,
    /// Height of the grid in pixels, 14 bits
    pub vertical_size: u16,
    /// Sample or display aspect ratio of the grid
    pub aspect_ratio_information: AspectRatioInformation,
}

impl TargetBackgroundGridDescriptor {
    /// Creates the descriptor of a grid
    #[must_use]
    pub const fn new(
        horizontal_size: u16,
        vertical_size: u16,
        aspect_ratio_information: AspectRatioInformation,
    ) -> Self {
        Self {
            horizontal_size,
            vertical_size,
            aspect_ratio_information,
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
    /// * [`Forbidden`](Error::Forbidden): `aspect_ratio_information` is 0,
    ///   forbidden, or 5 to 15, reserved.
    /// * [`Field`](Error::Field): a size wider than 14 bits.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let aspect_ratio_information = self.aspect_ratio_information.get();
        if !matches!(aspect_ratio_information, 1..=4) {
            return Err(Error::Forbidden {
                field: "aspect_ratio_information",
                value: u32::from(aspect_ratio_information),
            });
        }

        write_descriptor(
            out,
            DescriptorTag::TARGET_BACKGROUND_GRID,
            BODY_LENGTH,
            |writer| {
                writer
                    .write_bits::<14>(self.horizontal_size)
                    .map_err(field_error("horizontal_size"))?;
                writer
                    .write_bits::<14>(self.vertical_size)
                    .map_err(field_error("vertical_size"))?;
                writer
                    .write_bits::<4>(aspect_ratio_information)
                    .map_err(field_error("aspect_ratio_information"))
            },
        )
    }
}

impl TryFrom<&RawDescriptor<'_>> for TargetBackgroundGridDescriptor {
    type Error = Error;

    /// Takes the fields of a target background grid descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x07.
    /// * [`BodyLength`](Error::BodyLength): the body is not 4 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::TARGET_BACKGROUND_GRID)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            Ok(Self::new(
                reader.read_u16::<14>()?,
                reader.read_u16::<14>()?,
                AspectRatioInformation(reader.read_u8::<4>()?),
            ))
        })()
        .map_err(|error| Error::Field {
            field: "descriptor body",
            error,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    #[cfg(feature = "alloc")]
    use super::AspectRatioInformation;
    use super::TargetBackgroundGridDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};
    #[cfg(feature = "alloc")]
    use crate::field;

    #[test]
    fn a_body_of_other_than_4_bytes_is_refused() {
        assert_eq!(
            TargetBackgroundGridDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::TARGET_BACKGROUND_GRID,
                &[0x1e, 0x00, 0x43]
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::TARGET_BACKGROUND_GRID,
                length: 3
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn descriptors_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let grid = |horizontal_size, aspect_ratio_information| {
            TargetBackgroundGridDescriptor::new(
                horizontal_size,
                1080,
                AspectRatioInformation::try_from(aspect_ratio_information).unwrap(),
            )
        };
        let mut written = Vec::new();

        assert_eq!(
            [grid(1920, 0), grid(1920, 5), grid(0x4000, 3)]
                .map(|descriptor| descriptor.write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "aspect_ratio_information",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "aspect_ratio_information",
                    value: 5
                }),
                Err(Error::Field {
                    field: "horizontal_size",
                    error: field::Error::ValueTooWide {
                        width: 14,
                        value: 0x4000
                    }
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
