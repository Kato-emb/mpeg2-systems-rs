//! The framing every descriptor shares: a descriptor whose body is kept as
//! bytes, and a loop of them

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{DescriptorTag, Error};
use crate::field::FieldReader;
#[cfg(feature = "alloc")]
use crate::field::write_or_truncate;

/// Bytes of `descriptor_tag` and `descriptor_length`
pub(super) const HEADER_LENGTH: usize = 2;

/// Returns `descriptor_tag` and `descriptor_length` of a descriptor of
/// `descriptor_tag` whose body is `body_length` bytes
///
/// # Errors
///
/// * [`ForbiddenTag`](Error::ForbiddenTag): `descriptor_tag` is 0x01.
/// * [`DescriptorLength`](Error::DescriptorLength): `body_length` is more
///   than 255.
#[cfg(feature = "alloc")]
pub(super) fn header(
    descriptor_tag: DescriptorTag,
    body_length: usize,
) -> Result<[u8; HEADER_LENGTH], Error> {
    if descriptor_tag == DescriptorTag::FORBIDDEN {
        return Err(Error::ForbiddenTag);
    }
    let descriptor_length = u8::try_from(body_length).map_err(|_| Error::DescriptorLength {
        length: body_length,
    })?;
    Ok([descriptor_tag.get(), descriptor_length])
}

/// Descriptor whose body is kept as bytes, whatever its tag
///
/// `descriptor()`, ISO/IEC 13818-1 §2.6, semantics §2.6.1
///
/// `descriptor_length` follows from the body. A typed descriptor is
/// converted from it with `TryFrom` (see the [module](crate::descriptor)).
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::{DescriptorTag, Error, RawDescriptor};
///
/// // A registration descriptor of format_identifier "HDMV"
/// let bytes = [0x05, 0x04, 0x48, 0x44, 0x4d, 0x56];
/// let descriptor = RawDescriptor::read(&bytes)?;
/// assert_eq!(descriptor.descriptor_tag, DescriptorTag::REGISTRATION);
/// assert_eq!(descriptor.body, b"HDMV");
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Error>(())
/// ```
#[doc(alias = "descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RawDescriptor<'input> {
    /// The syntax of the body
    pub descriptor_tag: DescriptorTag,
    /// The bytes after `descriptor_length`
    pub body: &'input [u8],
}

impl<'input> RawDescriptor<'input> {
    /// Creates a descriptor of `descriptor_tag` carrying `body`
    #[must_use]
    pub const fn new(descriptor_tag: DescriptorTag, body: &'input [u8]) -> Self {
        Self {
            descriptor_tag,
            body,
        }
    }

    /// Takes the descriptor at the front of `bytes`, and returns it with the
    /// bytes after it
    fn frame(bytes: &'input [u8]) -> Result<(Self, &'input [u8]), Error> {
        let mut reader = FieldReader::new(bytes);
        let (descriptor_tag, descriptor_length) = reader
            .read_u8::<8>()
            .and_then(|tag| Ok((DescriptorTag::from(tag), reader.read_u8::<8>()?)))
            .map_err(|error| Error::Field {
                field: "descriptor_length",
                error,
            })?;
        let available_bytes = reader.remaining_bytes();
        let body = reader
            .read_bytes(usize::from(descriptor_length))
            .map_err(|_| Error::Length {
                descriptor_length,
                available_bytes,
            })?;
        // Why not an error: the body ends on a byte boundary.
        let rest = reader
            .read_bytes(reader.remaining_bytes())
            .unwrap_or_default();

        Ok((Self::new(descriptor_tag, body), rest))
    }

    /// Reads a descriptor from `bytes`, which hold it whole and nothing more
    ///
    /// # Errors
    ///
    /// * [`Field`](Error::Field): `bytes` end before `descriptor_length`.
    /// * [`Length`](Error::Length): `descriptor_length` does not match the
    ///   bytes after it.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        let (descriptor, rest) = Self::frame(bytes)?;
        if !rest.is_empty() {
            return Err(Error::Length {
                descriptor_length: u8::try_from(descriptor.body.len()).unwrap_or(u8::MAX),
                available_bytes: bytes.len().saturating_sub(HEADER_LENGTH),
            });
        }

        Ok(descriptor)
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        HEADER_LENGTH.saturating_add(self.body.len())
    }

    /// Appends the descriptor to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`ForbiddenTag`](Error::ForbiddenTag): `descriptor_tag` is 0x01.
    /// * [`DescriptorLength`](Error::DescriptorLength): `body` is longer than
    ///   255 bytes.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        out.extend_from_slice(&header(self.descriptor_tag, self.body.len())?);
        out.extend_from_slice(self.body);

        Ok(())
    }
}

/// A loop of descriptors, borrowed
///
/// The bytes are framed descriptor by descriptor as [`iter`](Self::iter)
/// walks them, so a view of any bytes can be made; a descriptor the bytes end
/// inside is an error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DescriptorLoop<'input>(&'input [u8]);

impl<'input> DescriptorLoop<'input> {
    /// The loop of no descriptor
    pub(crate) const EMPTY: Self = Self(&[]);

    /// Returns the bytes of the loop
    #[must_use]
    pub const fn as_bytes(self) -> &'input [u8] {
        self.0
    }

    /// Returns whether the loop has no bytes
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// Returns an iterator over the descriptors, each framed as it is reached
    #[must_use]
    pub const fn iter(self) -> DescriptorIter<'input> {
        DescriptorIter(self.0)
    }

    /// Appends the bytes of a loop of `descriptors` to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// The errors of [`RawDescriptor::write`].
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::descriptor::{DescriptorLoop, DescriptorTag, RawDescriptor};
    ///
    /// let mut bytes = Vec::new();
    /// DescriptorLoop::write_from(
    ///     [RawDescriptor::new(DescriptorTag::REGISTRATION, b"HDMV")],
    ///     &mut bytes,
    /// )?;
    /// assert_eq!(bytes, [0x05, 0x04, 0x48, 0x44, 0x4d, 0x56]);
    /// # }
    /// # Ok::<(), mpeg2_systems::descriptor::Error>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from<'descriptor>(
        descriptors: impl IntoIterator<Item = RawDescriptor<'descriptor>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        Self::write_entries(descriptors.into_iter().map(Ok), out)
    }

    /// Appends the bytes of `entries` to `out`, leaving `out` as it was if an
    /// entry is an error or is refused
    #[cfg(feature = "alloc")]
    pub(crate) fn write_entries<'descriptor>(
        entries: impl IntoIterator<Item = Result<RawDescriptor<'descriptor>, Error>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        write_or_truncate(out, |out| {
            for descriptor in entries {
                descriptor?.write(out)?;
            }

            Ok(())
        })
    }
}

impl<'input> From<&'input [u8]> for DescriptorLoop<'input> {
    fn from(bytes: &'input [u8]) -> Self {
        Self(bytes)
    }
}

impl<'input> IntoIterator for DescriptorLoop<'input> {
    type Item = Result<RawDescriptor<'input>, Error>;
    type IntoIter = DescriptorIter<'input>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the descriptors of a [`DescriptorLoop`]
///
/// A descriptor the bytes end inside is an error, and the last item: nothing
/// after a header that cannot be framed can be located.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DescriptorIter<'input>(&'input [u8]);

impl<'input> Iterator for DescriptorIter<'input> {
    type Item = Result<RawDescriptor<'input>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }

        // Why end the walk on an error: the bytes after a descriptor that
        // cannot be framed have no known position, so resynchronizing would
        // read bytes whose meaning is unknown.
        let (item, rest) = match RawDescriptor::frame(self.0) {
            Ok((descriptor, rest)) => (Ok(descriptor), rest),
            Err(error) => (Err(error), &[][..]),
        };
        self.0 = rest;

        Some(item)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;
    use core::array;

    use super::{DescriptorLoop, RawDescriptor};
    use crate::descriptor::{DescriptorTag, Error};
    use crate::field;

    /// A `registration_descriptor` of "HDMV", an `ISO_639_language_descriptor`
    /// of "jpn" and a user private descriptor of tag 0x52, laid out by hand
    const THREE_DESCRIPTORS: [u8; 15] = [
        0x05, 0x04, 0x48, 0x44, 0x4d, 0x56, 0x0a, 0x04, 0x6a, 0x70, 0x6e, 0x00, 0x52, 0x01, 0x30,
    ];

    fn three_descriptors() -> [RawDescriptor<'static>; 3] {
        [
            RawDescriptor::new(DescriptorTag::REGISTRATION, b"HDMV"),
            RawDescriptor::new(DescriptorTag::ISO_639_LANGUAGE, b"jpn\0"),
            RawDescriptor::new(DescriptorTag::from(0x52), &[0x30]),
        ]
    }

    #[test]
    fn a_loop_yields_each_descriptor_with_its_tag_and_body() {
        let mut descriptors = DescriptorLoop::from(THREE_DESCRIPTORS.as_slice()).iter();
        let [registration, language, private] = three_descriptors();

        assert_eq!(
            array::from_fn(|_| descriptors.next()),
            [
                Some(Ok(registration)),
                Some(Ok(language)),
                Some(Ok(private)),
                None
            ]
        );
    }

    #[test]
    fn a_descriptor_running_past_the_loop_ends_it_after_the_descriptors_before() {
        let bytes = [
            0x05, 0x04, 0x48, 0x44, 0x4d, 0x56, 0x52, 0x01, 0x30, 0x0a, 0x04, 0x6a, 0x70,
        ];
        let mut descriptors = DescriptorLoop::from(bytes.as_slice()).iter();
        let [registration, _, private] = three_descriptors();

        assert_eq!(
            array::from_fn(|_| descriptors.next()),
            [
                Some(Ok(registration)),
                Some(Ok(private)),
                Some(Err(Error::Length {
                    descriptor_length: 4,
                    available_bytes: 2
                })),
                None
            ]
        );
    }

    #[test]
    fn a_loop_ending_inside_a_header_yields_an_error_then_ends() {
        let mut descriptors = DescriptorLoop::from([0x52].as_slice()).iter();

        assert_eq!(
            array::from_fn(|_| descriptors.next()),
            [
                Some(Err(Error::Field {
                    field: "descriptor_length",
                    error: field::Error::Truncated {
                        required_bits: 8,
                        remaining_bits: 0
                    }
                })),
                None
            ]
        );
    }

    #[test]
    fn an_empty_loop_yields_nothing() {
        let empty = DescriptorLoop::from([].as_slice());

        assert_eq!((empty.is_empty(), empty.iter().next()), (true, None));
    }

    #[test]
    fn a_descriptor_is_read_only_from_bytes_that_hold_it_exactly() {
        assert_eq!(
            [
                RawDescriptor::read(&[0x52, 0x01, 0x30]),
                RawDescriptor::read(&[0x52, 0x01, 0x30, 0xff]),
                RawDescriptor::read(&[0x52, 0x02, 0x30]),
            ],
            [
                Ok(RawDescriptor::new(DescriptorTag::from(0x52), &[0x30])),
                Err(Error::Length {
                    descriptor_length: 1,
                    available_bytes: 2
                }),
                Err(Error::Length {
                    descriptor_length: 2,
                    available_bytes: 1
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_loop_built_from_its_descriptors_gives_the_bytes_laid_out_by_hand() {
        let mut written = Vec::new();
        DescriptorLoop::write_from(three_descriptors(), &mut written).unwrap();

        assert_eq!(written, THREE_DESCRIPTORS);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn descriptors_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let long_body = [0xff; 256];
        let [registration, ..] = three_descriptors();
        let mut written = Vec::new();

        assert_eq!(
            [
                RawDescriptor::new(DescriptorTag::from(0x52), &long_body).write(&mut written),
                RawDescriptor::new(DescriptorTag::FORBIDDEN, &[]).write(&mut written),
                DescriptorLoop::write_from(
                    [
                        registration,
                        RawDescriptor::new(DescriptorTag::FORBIDDEN, &[])
                    ],
                    &mut written
                ),
            ],
            [
                Err(Error::DescriptorLength { length: 256 }),
                Err(Error::ForbiddenTag),
                Err(Error::ForbiddenTag),
            ]
        );
        assert_eq!(written, []);
    }
}
