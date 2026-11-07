//! The steps every typed descriptor shares: taking the body of a raw
//! descriptor of its tag, and writing a descriptor around a body

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{DescriptorTag, Error, RawDescriptor};
use crate::field;
#[cfg(feature = "alloc")]
use crate::field::FieldWriter;

/// Returns the body of `raw`, refusing a descriptor of another tag than
/// `expected`
pub(crate) const fn body_of<'input>(
    raw: &RawDescriptor<'input>,
    expected: DescriptorTag,
) -> Result<&'input [u8], Error> {
    if raw.descriptor_tag.get() != expected.get() {
        return Err(Error::DescriptorTag {
            expected,
            found: raw.descriptor_tag,
        });
    }

    Ok(raw.body)
}

/// Returns the body of `raw`, refusing a descriptor of another tag than
/// `expected` or a body of other than `LENGTH` bytes
pub(crate) fn fixed_body<'input, const LENGTH: usize>(
    raw: &RawDescriptor<'input>,
    expected: DescriptorTag,
) -> Result<&'input [u8; LENGTH], Error> {
    let body = body_of(raw, expected)?;
    body.try_into().map_err(|_| Error::BodyLength {
        descriptor_tag: expected,
        length: body.len(),
    })
}

/// Returns the body of `raw` split after its first `LENGTH` bytes, refusing a
/// descriptor of another tag than `expected` or a body shorter than `LENGTH`
pub(crate) fn prefix_body<'input, const LENGTH: usize>(
    raw: &RawDescriptor<'input>,
    expected: DescriptorTag,
) -> Result<(&'input [u8; LENGTH], &'input [u8]), Error> {
    let body = body_of(raw, expected)?;
    body.split_first_chunk().ok_or(Error::BodyLength {
        descriptor_tag: expected,
        length: body.len(),
    })
}

/// Returns a mapping of a failure on `field` to an [`Error::Field`]
pub(crate) const fn field_error(field: &'static str) -> impl Fn(field::Error) -> Error {
    move |error| Error::Field { field, error }
}

/// Appends a descriptor of `descriptor_tag` to `out`, its body of
/// `body_length` bytes laid out by `write_body`, leaving `out` as it was if
/// the body is too long or a field is refused
#[cfg(feature = "alloc")]
pub(crate) fn write_descriptor(
    out: &mut Vec<u8>,
    descriptor_tag: DescriptorTag,
    body_length: usize,
    write_body: impl FnOnce(&mut FieldWriter<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    let descriptor_length = u8::try_from(body_length).map_err(|_| Error::DescriptorLength {
        length: body_length,
    })?;
    let start = out.len();
    out.push(descriptor_tag.get());
    out.push(descriptor_length);
    let mut writer = FieldWriter::new(out);
    let written = write_body(&mut writer)
        .and_then(|()| writer.finish().map_err(field_error("descriptor body")));
    if written.is_err() {
        out.truncate(start);
    }

    written
}
