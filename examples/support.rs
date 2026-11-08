//! What the dump examples share: the heading and the indented lines of an
//! event, tables of the entries an event lists, and runs of bytes shown by
//! their length and first bytes

use std::fmt::Write;

use mpeg2_systems::descriptor::DescriptorLoop;
use mpeg2_systems::pes::PesPacket;

/// Bytes of a run shown before the rest is left out
const PREVIEW_LENGTH: usize = 16;

/// Columns the lines under a heading are indented by
const INDENT: &str = "              ";

/// Where the values of a table column sit
#[derive(Debug, Clone, Copy)]
pub(crate) enum Align {
    /// Against the left edge, as names and hexadecimal values
    Left,
    /// Against the right edge, as decimal numbers
    Right,
}

/// Prints event `index`: a blank line unless it is the first, the heading of
/// its `kind` padded to `kind_width` and its `fields`, and its `lines`
/// indented under it
pub(crate) fn print_event(
    index: usize,
    kind: &str,
    kind_width: usize,
    fields: &str,
    lines: &[String],
) {
    #[expect(clippy::print_stdout, reason = "the events are the output")]
    {
        if index > 0 {
            println!();
        }
        println!(
            "{}",
            format!("[{index:>5}] {kind:<kind_width$}  {fields}").trim_end()
        );
        for line in lines {
            println!("{INDENT}{line}");
        }
    }
}

/// A table of `rows` under a line of the column names, each column as wide
/// as its name or its widest value
pub(crate) fn table(columns: &[(&str, Align)], rows: &[Vec<String>]) -> Vec<String> {
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(column, (name, _))| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(String::len)
                .fold(name.len(), usize::max)
        })
        .collect();
    let line = |cells: Vec<(&str, Align)>| {
        let mut line = String::new();
        for ((cell, align), width) in cells.into_iter().zip(&widths) {
            let _ = match align {
                Align::Left => write!(line, "{cell:<width$}  "),
                Align::Right => write!(line, "{cell:>width$}  "),
            };
        }
        line.trim_end().to_owned()
    };
    let mut lines = vec![line(
        columns
            .iter()
            .map(|&(name, _)| (name, Align::Left))
            .collect(),
    )];
    lines.extend(rows.iter().map(|row| {
        line(
            row.iter()
                .zip(columns)
                .map(|(cell, &(_, align))| (cell.as_str(), align))
                .collect(),
        )
    }));
    lines
}

/// A run of bytes as its name, its length and its first bytes:
/// `payload     1,803 B: 00 00 01 b3 ..`
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
        "{name:<7} {:>11} B: {}{more}",
        grouped(bytes.len()),
        shown.join(" ")
    )
}

/// The descriptors of `descriptors` as a table of their `descriptor_tag` and
/// `descriptor_length`, the contents not read, or nothing for an empty loop
pub(crate) fn descriptor_table(descriptors: DescriptorLoop<'_>) -> Vec<String> {
    if descriptors.as_bytes().is_empty() {
        return Vec::new();
    }
    let mut unread = Vec::new();
    let rows: Vec<Vec<String>> = descriptors
        .iter()
        .filter_map(|descriptor| {
            descriptor
                .map_err(|error| unread.push(format!("a descriptor that does not read: {error}")))
                .ok()
        })
        .map(|descriptor| {
            vec![
                format!("{:#04x}", descriptor.descriptor_tag.get()),
                descriptor.body.len().to_string(),
            ]
        })
        .collect();
    let mut lines = table(
        &[
            ("descriptor_tag", Align::Left),
            ("descriptor_length", Align::Right),
        ],
        &rows,
    );
    lines.extend(unread);
    lines
}

/// The PTS and the DTS `packet` carries in 90 kHz units, each as wide as 33
/// bits can count, `-` for one it does not carry
pub(crate) fn pes_times(packet: &PesPacket<'_>) -> String {
    let header = packet.optional_header.as_ref();
    let time = |time: Option<u64>| time.map_or_else(|| "-".to_owned(), |time| time.to_string());
    format!(
        "PTS {:>10}  DTS {:>10}",
        time(header.and_then(|header| header.pts).map(|pts| pts.get())),
        time(header.and_then(|header| header.dts).map(|dts| dts.get()))
    )
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
