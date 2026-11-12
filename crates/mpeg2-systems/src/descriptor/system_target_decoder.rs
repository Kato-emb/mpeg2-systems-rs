//! STD descriptor, which tells how data moves from the multiplex buffer to
//! the elementary stream buffer of a video stream in the T-STD

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::typed::fixed_body;
#[cfg(feature = "alloc")]
use super::typed::{field_error, write_descriptor};
use super::{DescriptorTag, Error, RawDescriptor};

/// Bytes of the body
const BODY_LENGTH: usize = 1;

/// STD descriptor, which tells how data moves from the multiplex buffer to
/// the elementary stream buffer of a video stream in the T-STD
///
/// `STD_descriptor`, ISO/IEC 13818-1 §2.6.32, Table 2-70, semantics §2.6.33
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::system_target_decoder::StdDescriptor;
///
/// let bytes = [0x11, 0x01, 0xff];
/// let descriptor = StdDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert!(descriptor.leak_valid_flag);
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
#[doc(alias("STD_descriptor", "system_target_decoder_descriptor"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StdDescriptor {
    /// Whether the transfer from `MBn` to `EBn` uses the leak method rather than
    /// the `vbv_delay` method
    pub leak_valid_flag: bool,
}

impl StdDescriptor {
    /// Creates the descriptor of the transfer method `leak_valid_flag` names
    #[must_use]
    pub const fn new(leak_valid_flag: bool) -> Self {
        Self { leak_valid_flag }
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
    /// # Errors
    ///
    /// None: the flag fits its field. The `Result` is that of every
    /// descriptor's `write`.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write_descriptor(out, DescriptorTag::STD, BODY_LENGTH, |writer| {
            writer
                .write_bits::<7>(0b111_1111_u8)
                .and_then(|()| writer.write_bool(self.leak_valid_flag))
                .map_err(field_error("leak_valid_flag"))
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for StdDescriptor {
    type Error = Error;

    /// Takes the field of an STD descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x11.
    /// * [`BodyLength`](Error::BodyLength): the body is not 1 byte.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let [byte] = *fixed_body::<BODY_LENGTH>(raw, DescriptorTag::STD)?;

        Ok(Self::new(byte & 1 != 0))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::StdDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    #[cfg(feature = "alloc")]
    #[test]
    fn reserved_bits_read_as_0_are_written_as_1() {
        let descriptor =
            StdDescriptor::try_from(&RawDescriptor::read(&[0x11, 0x01, 0x01]).unwrap());
        let mut written = Vec::new();
        StdDescriptor::new(true).write(&mut written).unwrap();

        assert_eq!(
            (descriptor, written.as_slice()),
            (Ok(StdDescriptor::new(true)), [0x11, 0x01, 0xff].as_slice())
        );
    }

    #[test]
    fn a_body_of_other_than_1_byte_is_refused() {
        assert_eq!(
            StdDescriptor::try_from(&RawDescriptor::new(DescriptorTag::STD, &[0xff, 0xff])),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::STD,
                length: 2
            })
        );
    }
}
