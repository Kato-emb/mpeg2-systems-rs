//! CRC-32 that guards PSI sections and program stream maps
//!
//! A section and a program stream map end in a `CRC_32` field. [`crc_32`] is
//! the computation of Annex A: run over a whole section, `CRC_32` included, it
//! returns 0 when no bit has changed; run over the bytes before `CRC_32`, it
//! returns the value a writer puts there.

/// Generator polynomial x^32 + x^26 + x^23 + x^22 + x^16 + x^12 + x^11 + x^10 + x^8 + x^7 + x^5 + x^4 + x^2 + x + 1, x^32 left implicit
const GENERATOR_POLYNOMIAL: u32 = 0x04C1_1DB7;

/// Returns the CRC-32 that guards a section or a program stream map
///
/// `CRC_32`, ISO/IEC 13818-1 Annex A
///
/// Each byte enters most significant bit first into a register that starts
/// with every bit set; the result is neither reflected nor inverted. Over a
/// whole section, `CRC_32` included, the result is 0 when the section is
/// intact. The crate does not compute it across separate chunks of input: a
/// section is gathered whole before it is checked.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::crc::crc_32;
///
/// // A program association section, its CRC_32 in the last four bytes
/// let section = [
///     0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xf0, 0x00,
///     0x2a, 0xb1, 0x04, 0xb2,
/// ];
///
/// // The section with its CRC_32 brings the register to 0
/// assert_eq!(crc_32(&section), 0);
///
/// // The bytes before CRC_32 give the value it holds
/// let (body, field) = section.split_last_chunk::<4>().unwrap();
/// assert_eq!(crc_32(body), u32::from_be_bytes(*field));
/// ```
#[must_use]
pub fn crc_32(bytes: &[u8]) -> u32 {
    let mut register = u32::MAX;
    for byte in bytes {
        register ^= u32::from_be_bytes([*byte, 0, 0, 0]);
        for _ in 0..8 {
            let carry = register.leading_zeros() == 0;
            register = register.wrapping_shl(1);
            if carry {
                register ^= GENERATOR_POLYNOMIAL;
            }
        }
    }

    register
}

#[cfg(test)]
mod tests {
    use super::crc_32;

    /// The program association section of `.old/tests/fixtures/188.ts`, from
    /// the first packet on PID 0x0000, `CRC_32` included
    const PROGRAM_ASSOCIATION_SECTION: [u8; 16] = [
        0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xf0, 0x00, 0x2a, 0xb1, 0x04,
        0xb2,
    ];

    #[test]
    fn a_section_read_whole_brings_the_register_to_zero_and_its_body_gives_its_crc_32() {
        let (body, field) = PROGRAM_ASSOCIATION_SECTION.split_last_chunk::<4>().unwrap();

        assert_eq!(
            (crc_32(&PROGRAM_ASSOCIATION_SECTION), crc_32(body)),
            (0, u32::from_be_bytes(*field))
        );
    }

    #[test]
    fn the_catalogued_check_value_of_crc_32_mpeg_2_is_reproduced() {
        assert_eq!(crc_32(b"123456789"), 0x0376_e6e7);
    }

    #[test]
    fn a_section_with_any_one_byte_changed_does_not_bring_the_register_to_zero() {
        let positions_left_unnoticed = (0..PROGRAM_ASSOCIATION_SECTION.len())
            .filter(|position| {
                let mut corrupted = PROGRAM_ASSOCIATION_SECTION;
                if let Some(byte) = corrupted.get_mut(*position) {
                    *byte ^= 0x01;
                }
                crc_32(&corrupted) == 0
            })
            .count();

        assert_eq!(positions_left_unnoticed, 0);
    }
}
