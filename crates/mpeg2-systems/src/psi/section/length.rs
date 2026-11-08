//! Length of a section, read from its first bytes

use super::PREFIX_LENGTH;

/// Returns how long the section that `prefix` starts is, the 3 bytes from
/// `table_id` to `section_length` included
///
/// `section_length`, ISO/IEC 13818-1 §2.4.4.11
///
/// Only the first 3 bytes are looked at, or `None` returned when fewer are
/// given. Neither `table_id` nor the largest `section_length` its table
/// allows ([`TableId::max_section_length`]) is checked; [`Section::read`]
/// checks both. Unlike [`Section::section_length`], which gives the field's
/// value, the length returned counts the 3 bytes before it.
///
/// [`TableId::max_section_length`]: crate::psi::TableId::max_section_length
/// [`Section::read`]: super::Section::read
/// [`Section::section_length`]: super::Section::section_length
///
/// # Examples
///
/// ```
/// use mpeg2_systems::psi::section::section_length;
///
/// assert_eq!(section_length(&[0x00, 0xb0]), None);
/// // The start of a program association section
/// assert_eq!(section_length(&[0x00, 0xb0, 0x0d]), Some(16));
/// // The 4 bits before section_length are not part of it
/// assert_eq!(section_length(&[0x00, 0xf0, 0x0d]), Some(16));
/// ```
#[must_use]
pub fn section_length(prefix: &[u8]) -> Option<usize> {
    let [_, high, low] = prefix.first_chunk::<PREFIX_LENGTH>()?;

    Some(PREFIX_LENGTH.saturating_add(usize::from(u16::from_be_bytes([high & 0x0f, *low]))))
}

#[cfg(test)]
mod tests {
    use super::section_length;
    use crate::psi::section::Section;
    use crate::ts::Pid;
    use crate::ts::packet::TransportPacket;

    const STANDARD_188: &[u8] = include_bytes!("../../../tests/fixtures/188.ts");

    #[test]
    fn fewer_than_3_bytes_have_no_length() {
        assert_eq!(section_length(&[]), None);
        assert_eq!(section_length(&[0x00]), None);
        assert_eq!(section_length(&[0x00, 0xb0]), None);
    }

    #[test]
    fn the_length_counts_the_3_bytes_up_to_section_length() {
        assert_eq!(section_length(&[0x00, 0xb0, 0x0d]), Some(16));
        assert_eq!(section_length(&[0x80, 0x70, 0x00]), Some(3));
    }

    #[test]
    fn the_bits_before_section_length_do_not_change_it() {
        assert_eq!(section_length(&[0x02, 0xf0, 0x0d]), Some(16));
        assert_eq!(section_length(&[0x02, 0x00, 0x0d]), Some(16));
    }

    #[test]
    fn a_length_past_the_largest_any_table_allows_is_returned_unchecked() {
        assert_eq!(section_length(&[0x00, 0x0f, 0xff]), Some(4098));
        assert_eq!(section_length(&[0xff, 0xff, 0xff]), Some(4098));
    }

    #[test]
    fn the_length_cuts_the_program_association_section_of_a_stream() {
        let packet = STANDARD_188
            .chunks(188)
            .filter_map(|bytes| TransportPacket::read(bytes).ok())
            .find(|packet| packet.pid == Pid::PAT && packet.payload_unit_start_indicator)
            .unwrap();
        let (&pointer_field, rest) = packet.payload.unwrap().split_first().unwrap();
        let bytes = rest.get(usize::from(pointer_field)..).unwrap();

        let length = section_length(bytes).unwrap();

        assert_eq!(
            Section::read(bytes.get(..length).unwrap())
                .unwrap()
                .encoded_len(),
            length
        );
    }
}
