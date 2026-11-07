//! Multiplex buffer utilization descriptor, which bounds the occupancy of
//! the STD multiplex buffer for remultiplexers

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::write_descriptor;
use super::{DescriptorTag, Error, RawDescriptor, field_error, fixed_body};
use crate::field::FieldReader;

/// Bytes of the body
const BODY_LENGTH: usize = 4;

/// The bounds of `LTW_offset` a [`MultiplexBufferUtilizationDescriptor`]
/// carries when `bound_valid_flag` is `'1'`
///
/// Both are in units of (27 MHz ÷ 300) clock periods, as `LTW_offset` is.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LtwOffsetBounds {
    /// Lowest value any `LTW_offset` of the stream would have, 15 bits
    pub ltw_offset_lower_bound: u16,
    /// Largest value any `LTW_offset` of the stream would have, 15 bits
    pub ltw_offset_upper_bound: u16,
}

impl LtwOffsetBounds {
    /// Creates the bounds
    #[must_use]
    pub const fn new(ltw_offset_lower_bound: u16, ltw_offset_upper_bound: u16) -> Self {
        Self {
            ltw_offset_lower_bound,
            ltw_offset_upper_bound,
        }
    }
}

/// Multiplex buffer utilization descriptor, which bounds the occupancy of
/// the STD multiplex buffer for remultiplexers
///
/// `multiplex_buffer_utilization_descriptor`, ISO/IEC 13818-1 §2.6.22, Table
/// 2-65, semantics §2.6.23
///
/// `bound_valid_flag` follows from whether
/// [`ltw_offset_bounds`](Self::ltw_offset_bounds) is present. Without it the
/// bounds are undefined: they are not kept when read, and are written 0.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::multiplex_buffer_utilization::{
///     LtwOffsetBounds, MultiplexBufferUtilizationDescriptor,
/// };
///
/// // LTW_offset between 100 and 2000
/// let bytes = [0x0c, 0x04, 0x80, 0x64, 0x87, 0xd0];
/// let descriptor = MultiplexBufferUtilizationDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor.ltw_offset_bounds, Some(LtwOffsetBounds::new(100, 2000)));
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias(
    "multiplex_buffer_utilization_descriptor",
    "Multiplex_buffer_utilization_descriptor"
))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MultiplexBufferUtilizationDescriptor {
    /// The bounds of `LTW_offset`, present when `bound_valid_flag` is `'1'`
    pub ltw_offset_bounds: Option<LtwOffsetBounds>,
}

impl MultiplexBufferUtilizationDescriptor {
    /// Creates the descriptor without bounds
    #[must_use]
    #[expect(
        clippy::new_without_default,
        reason = "the specification defines no default descriptor"
    )]
    pub const fn new() -> Self {
        Self {
            ltw_offset_bounds: None,
        }
    }

    /// Returns whether the bounds are valid
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element reads its derived fields from a reference"
    )]
    pub const fn bound_valid_flag(&self) -> bool {
        self.ltw_offset_bounds.is_some()
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

    /// Appends the descriptor to `out`, its `reserved` bit `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `ltw_offset_lower_bound` is above
    ///   `ltw_offset_upper_bound`, so no `LTW_offset` lies between them.
    /// * [`Field`](Error::Field): a bound wider than 15 bits.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let bounds = self.ltw_offset_bounds.unwrap_or(LtwOffsetBounds::new(0, 0));
        if bounds.ltw_offset_lower_bound > bounds.ltw_offset_upper_bound {
            return Err(Error::Forbidden {
                field: "LTW_offset_lower_bound",
                value: u32::from(bounds.ltw_offset_lower_bound),
            });
        }

        write_descriptor(
            out,
            DescriptorTag::MULTIPLEX_BUFFER_UTILIZATION,
            BODY_LENGTH,
            |writer| {
                writer
                    .write_bool(self.bound_valid_flag())
                    .and_then(|()| writer.write_bits::<15>(bounds.ltw_offset_lower_bound))
                    .map_err(field_error("LTW_offset_lower_bound"))?;
                writer
                    .write_bool(true)
                    .and_then(|()| writer.write_bits::<15>(bounds.ltw_offset_upper_bound))
                    .map_err(field_error("LTW_offset_upper_bound"))
            },
        )
    }
}

impl TryFrom<&RawDescriptor<'_>> for MultiplexBufferUtilizationDescriptor {
    type Error = Error;

    /// Takes the fields of a multiplex buffer utilization descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x0C.
    /// * [`BodyLength`](Error::BodyLength): the body is not 4 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::MULTIPLEX_BUFFER_UTILIZATION)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            let bound_valid_flag = reader.read_bool()?;
            let ltw_offset_lower_bound = reader.read_u16::<15>()?;
            reader.read_bool()?;
            let bounds = LtwOffsetBounds::new(ltw_offset_lower_bound, reader.read_u16::<15>()?);
            Ok(Self {
                ltw_offset_bounds: bound_valid_flag.then_some(bounds),
            })
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{LtwOffsetBounds, MultiplexBufferUtilizationDescriptor};
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    #[test]
    fn the_bounds_are_read_only_when_bound_valid_flag_is_set() {
        let mut bounded = MultiplexBufferUtilizationDescriptor::new();
        bounded.ltw_offset_bounds = Some(LtwOffsetBounds::new(100, 2000));

        assert_eq!(
            [
                [0x0c, 0x04, 0x80, 0x64, 0x87, 0xd0],
                [0x0c, 0x04, 0x00, 0x64, 0x87, 0xd0],
            ]
            .map(|bytes| {
                MultiplexBufferUtilizationDescriptor::try_from(
                    &RawDescriptor::read(&bytes).unwrap(),
                )
            }),
            [Ok(bounded), Ok(MultiplexBufferUtilizationDescriptor::new())]
        );
    }

    #[test]
    fn a_body_of_other_than_4_bytes_is_refused() {
        assert_eq!(
            MultiplexBufferUtilizationDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::MULTIPLEX_BUFFER_UTILIZATION,
                &[0x80, 0x64, 0x87]
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::MULTIPLEX_BUFFER_UTILIZATION,
                length: 3
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_descriptor_without_bounds_is_written_with_them_0_and_crossed_bounds_are_refused() {
        let mut crossed = MultiplexBufferUtilizationDescriptor::new();
        crossed.ltw_offset_bounds = Some(LtwOffsetBounds::new(2000, 100));
        let mut written = Vec::new();

        assert_eq!(
            [
                crossed.write(&mut written),
                MultiplexBufferUtilizationDescriptor::new().write(&mut written)
            ],
            [
                Err(Error::Forbidden {
                    field: "LTW_offset_lower_bound",
                    value: 2000
                }),
                Ok(())
            ]
        );
        assert_eq!(written, [0x0c, 0x04, 0x00, 0x00, 0x80, 0x00]);
    }
}
