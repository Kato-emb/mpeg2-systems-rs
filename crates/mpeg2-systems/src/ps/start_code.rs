//! The start codes of a Program Stream: the 32 bits each of its elements
//! starts with, and the code that ends it, ISO/IEC 13818-1 §2.1.137

/// `packet_start_code_prefix`, the 24 bits every start code starts with
#[cfg(feature = "alloc")]
pub(super) const START_CODE_PREFIX: [u8; 3] = [0x00, 0x00, 0x01];

/// The byte after `packet_start_code_prefix` in each start code
#[cfg(feature = "alloc")]
pub(super) const PACK_STREAM_ID: u8 = PACK_START_CODE[3];
#[cfg(feature = "alloc")]
pub(super) const SYSTEM_HEADER_STREAM_ID: u8 = SYSTEM_HEADER_START_CODE[3];
#[cfg(feature = "alloc")]
pub(super) const PROGRAM_STREAM_MAP_STREAM_ID: u8 = PROGRAM_STREAM_MAP_START_CODE[3];
#[cfg(feature = "alloc")]
pub(super) const PROGRAM_END_STREAM_ID: u8 = MPEG_PROGRAM_END_CODE[3];

/// `pack_start_code`, the 32 bits a pack header starts with, ISO/IEC 13818-1
/// §2.5.3.4
pub const PACK_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xba];

/// `system_header_start_code`, the 32 bits a system header starts with,
/// ISO/IEC 13818-1 §2.5.3.6
pub const SYSTEM_HEADER_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xbb];

/// `packet_start_code_prefix` and `map_stream_id`, the 32 bits a program
/// stream map starts with, ISO/IEC 13818-1 §2.5.4.2
pub const PROGRAM_STREAM_MAP_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xbc];

/// `MPEG_program_end_code`, the 32 bits that end a Program Stream, ISO/IEC
/// 13818-1 §2.5.3.2
pub const MPEG_PROGRAM_END_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xb9];

/// Returns whether `bytes` are the `MPEG_program_end_code` and nothing else
///
/// # Examples
///
/// ```
/// use mpeg2_systems::ps::{self, MPEG_PROGRAM_END_CODE};
///
/// assert!(ps::is_mpeg_program_end_code(&[0x00, 0x00, 0x01, 0xb9]));
/// assert!(!ps::is_mpeg_program_end_code(&[0x00, 0x00, 0x01, 0xba]));
/// assert!(!ps::is_mpeg_program_end_code(&MPEG_PROGRAM_END_CODE[..3]));
/// ```
#[must_use]
pub fn is_mpeg_program_end_code(bytes: &[u8]) -> bool {
    bytes == MPEG_PROGRAM_END_CODE
}
