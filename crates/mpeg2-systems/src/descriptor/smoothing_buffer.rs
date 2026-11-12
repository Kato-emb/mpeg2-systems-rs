//! Smoothing buffer descriptor, which sizes the smoothing buffer of a program
//! or a program element and its leak rate

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::typed::write_descriptor;
use super::typed::{field_error, fixed_body};
use super::{DescriptorTag, Error, RawDescriptor};
use crate::field::FieldReader;

/// Bytes of the body
const BODY_LENGTH: usize = 6;

/// Smoothing buffer descriptor, which sizes the smoothing buffer of a program
/// or a program element and its leak rate
///
/// `smoothing_buffer_descriptor`, ISO/IEC 13818-1 §2.6.30, Table 2-69,
/// semantics §2.6.31
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::smoothing_buffer::SmoothingBufferDescriptor;
///
/// // A leak of 37500 × 400 bits a second out of a buffer of 1504 bytes
/// let bytes = [0x10, 0x06, 0xc0, 0x92, 0x7c, 0xc0, 0x05, 0xe0];
/// let descriptor = SmoothingBufferDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor, SmoothingBufferDescriptor::new(37500, 1504));
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
#[doc(alias = "smoothing_buffer_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmoothingBufferDescriptor {
    /// Leak rate out of the buffer, in units of 400 bits a second, 22 bits
    pub sb_leak_rate: u32,
    /// Size of the buffer in bytes, 22 bits
    pub sb_size: u32,
}

impl SmoothingBufferDescriptor {
    /// Creates the descriptor of a buffer
    #[must_use]
    pub const fn new(sb_leak_rate: u32, sb_size: u32) -> Self {
        Self {
            sb_leak_rate,
            sb_size,
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub const fn encoded_len(&self) -> usize {
        super::raw::HEADER_LENGTH.saturating_add(BODY_LENGTH)
    }

    /// Appends the descriptor to `out`, its `reserved` bits all `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `sb_leak_rate` or `sb_size` is 0,
    ///   which is not the positive integer §2.6.31 requires.
    /// * [`Field`](Error::Field): either wider than 22 bits.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        for (field, value) in [
            ("sb_leak_rate", self.sb_leak_rate),
            ("sb_size", self.sb_size),
        ] {
            if value == 0 {
                return Err(Error::Forbidden { field, value });
            }
        }

        write_descriptor(
            out,
            DescriptorTag::SMOOTHING_BUFFER,
            BODY_LENGTH,
            |writer| {
                writer
                    .write_bits::<2>(0b11_u8)
                    .and_then(|()| writer.write_bits::<22>(self.sb_leak_rate))
                    .map_err(field_error("sb_leak_rate"))?;
                writer
                    .write_bits::<2>(0b11_u8)
                    .and_then(|()| writer.write_bits::<22>(self.sb_size))
                    .map_err(field_error("sb_size"))
            },
        )
    }
}

impl TryFrom<&RawDescriptor<'_>> for SmoothingBufferDescriptor {
    type Error = Error;

    /// Takes the fields of a smoothing buffer descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x10.
    /// * [`BodyLength`](Error::BodyLength): the body is not 6 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::SMOOTHING_BUFFER)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            reader.read_bits::<2>()?;
            let sb_leak_rate = reader.read_u32::<22>()?;
            reader.read_bits::<2>()?;
            Ok(Self::new(sb_leak_rate, reader.read_u32::<22>()?))
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::SmoothingBufferDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    #[cfg(feature = "alloc")]
    #[test]
    fn reserved_bits_read_as_0_are_written_as_1() {
        let descriptor = SmoothingBufferDescriptor::try_from(
            &RawDescriptor::read(&[0x10, 0x06, 0x00, 0x92, 0x7c, 0x00, 0x05, 0xe0]).unwrap(),
        );
        let mut written = Vec::new();
        SmoothingBufferDescriptor::new(37500, 1504)
            .write(&mut written)
            .unwrap();

        assert_eq!(
            (descriptor, written.as_slice()),
            (
                Ok(SmoothingBufferDescriptor::new(37500, 1504)),
                [0x10, 0x06, 0xc0, 0x92, 0x7c, 0xc0, 0x05, 0xe0].as_slice()
            )
        );
    }

    #[test]
    fn a_body_of_other_than_6_bytes_is_refused() {
        assert_eq!(
            SmoothingBufferDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::SMOOTHING_BUFFER,
                &[0xc0, 0x92, 0x7c, 0xc0, 0x05]
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::SMOOTHING_BUFFER,
                length: 5
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_leak_rate_or_size_of_0_is_refused_before_a_byte_is_written() {
        let mut written = Vec::new();

        assert_eq!(
            [
                SmoothingBufferDescriptor::new(0, 1504),
                SmoothingBufferDescriptor::new(37500, 0)
            ]
            .map(|descriptor| descriptor.write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "sb_leak_rate",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "sb_size",
                    value: 0
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
