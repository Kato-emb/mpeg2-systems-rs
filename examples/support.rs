//! What the dump examples share: how an event is laid out, a run of bytes
//! shown by its length and first bytes, and a descriptor loop shown by tag
//! and length

use std::fmt::Write;

use mpeg2_systems::descriptor::DescriptorLoop;
use mpeg2_systems::pes::PesPacket;

/// Bytes of a run shown before the rest is left out
const PREVIEW_LENGTH: usize = 16;

/// Prints event `index`: its heading, its lines indented under it, and a
/// blank line before it unless it is the first
pub(crate) fn print_event(index: usize, heading: &str, lines: &[String]) {
    #[expect(clippy::print_stdout, reason = "the events are the output")]
    {
        if index > 0 {
            println!();
        }
        println!("[{index}] {heading}");
        for line in lines {
            println!("      {line}");
        }
    }
}

/// A run of bytes as its name, its length and its first bytes in
/// hexadecimal: `payload 1,803 B: 00 00 01 b3 ..`
pub(crate) fn byte_run(name: &str, bytes: &[u8]) -> String {
    let shown: Vec<String> = bytes
        .iter()
        .take(PREVIEW_LENGTH)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let more = if bytes.len() > PREVIEW_LENGTH {
        " .."
    } else {
        ""
    };
    format!(
        "{name} {} B: {}{more}",
        grouped(bytes.len()),
        shown.join(" ")
    )
}

/// The descriptors of `descriptors`, each by its `descriptor_tag` and
/// `descriptor_length`, the contents not read
pub(crate) fn descriptor_lines(descriptors: DescriptorLoop<'_>) -> Vec<String> {
    descriptors
        .iter()
        .map(|descriptor| match descriptor {
            Ok(descriptor) => format!(
                "descriptor_tag=0x{:02X}  descriptor_length={}",
                descriptor.descriptor_tag.get(),
                descriptor.body.len()
            ),
            Err(error) => format!("a descriptor that does not read: {error}"),
        })
        .collect()
}

/// The PTS and the DTS `packet` carries, each as `  PTS=…` in 90 kHz units
pub(crate) fn pes_times(packet: &PesPacket<'_>) -> String {
    let header = packet.optional_header.as_ref();
    let mut times = String::new();
    if let Some(pts) = header.and_then(|header| header.pts) {
        let _ = write!(times, "  PTS={}", pts.get());
    }
    if let Some(dts) = header.and_then(|header| header.dts) {
        let _ = write!(times, "  DTS={}", dts.get());
    }
    times
}

/// `value` with its digits grouped by three: `8,192`
fn grouped(value: usize) -> String {
    let digits = value.to_string();
    let groups: Vec<String> = digits
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|group| String::from_utf8_lossy(group).into_owned())
        .collect();
    groups.join(",")
}
