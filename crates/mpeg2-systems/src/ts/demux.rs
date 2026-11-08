//! Demuxing a Transport Stream: input of any length in, events out

use core::fmt;

mod framer;

pub use framer::sync::PacketSize;
use framer::{Output, PacketFramer};

/// Settings of a [`Demuxer`]
///
/// Build one from the default and set the fields that differ:
///
/// ```
/// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, PacketSize};
///
/// let mut config = DemuxerConfig::default();
/// config.packet_size = Some(PacketSize::Timestamped192);
/// let demuxer = Demuxer::new(config);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DemuxerConfig {
    /// The packet size the stream is known to have, or `None` to detect it
    /// from the input (the default)
    pub packet_size: Option<PacketSize>,
}

/// What a [`Demuxer`] observed in its input
///
/// An event borrows the bytes it carries from the demuxer, until the next call
/// on the demuxer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Event<'a> {
    /// The packet grid was found, and Transport Packets are cut off it from
    /// here on
    SyncAcquired {
        /// The packet size the grid has
        packet_size: PacketSize,
    },
    /// Input that was not cut into a Transport Packet
    ///
    /// Reported for the bytes before the grid is found, those the search
    /// walks past after [`Error::SyncLost`], and those [`Demuxer::finish`]
    /// leaves that fill no packet. A long run arrives as several events, so
    /// the bytes the demuxer holds stay bounded. The bytes
    /// of these events and of the packets cut add up to every byte handed to
    /// [`Demuxer::handle_input`].
    Unsynchronized {
        /// The bytes, in input order
        bytes: &'a [u8],
    },
    /// The input broke a rule of the Transport Stream; demuxing goes on
    Error(Error),
}

/// Failure of the Transport Stream a [`Demuxer`] reads, reported as an
/// [`Event::Error`]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// One packet on the grid did not start with `sync_byte` 0x47
    ///
    /// The packet's bytes are passed over and the grid is kept.
    SyncByteMissing {
        /// The byte found where `sync_byte` was expected
        found: u8,
    },
    /// Two packets in a row did not start with `sync_byte` 0x47, and the grid
    /// was dropped
    ///
    /// The search for a grid starts again at the second of them; what it
    /// walks past comes as [`Event::Unsynchronized`].
    SyncLost,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SyncByteMissing { found } => {
                write!(formatter, "sync_byte {found:#04x} is not 0x47")
            }
            Self::SyncLost => write!(
                formatter,
                "sync_byte is missing from two transport packets in a row"
            ),
        }
    }
}

impl core::error::Error for Error {}

/// Transport Stream demuxer, which takes input of any length and reports what
/// it finds as events
///
/// The demuxer finds the packet grid (188, 192 or 204 bytes), cuts Transport
/// Packets off it, and reports each byte it could not cut. Handing input over
/// never fails: a broken stream is reported as [`Event::Error`] and demuxing
/// goes on. The demuxer does no I/O; reading the stream is up to the caller.
///
/// ```
/// use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event, PacketSize};
///
/// let stream = include_bytes!("../../tests/fixtures/188.ts");
/// let mut demuxer = Demuxer::new(DemuxerConfig::default());
/// let mut events = Vec::new();
///
/// // Hand over the input in any pieces, taking the events after each
/// for chunk in stream.chunks(1000) {
///     demuxer.handle_input(chunk);
///     while let Some(event) = demuxer.poll_event() {
///         events.push(format!("{event:?}"));
///     }
/// }
///
/// // End the input and take the events it leaves
/// demuxer.finish();
/// while let Some(event) = demuxer.poll_event() {
///     events.push(format!("{event:?}"));
/// }
///
/// let acquired = Event::SyncAcquired { packet_size: PacketSize::Standard188 };
/// assert_eq!(events, [format!("{acquired:?}")]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Demuxer {
    framer: PacketFramer,
}

impl Demuxer {
    /// Returns a demuxer that has seen no input
    #[must_use]
    pub const fn new(config: DemuxerConfig) -> Self {
        Self {
            framer: PacketFramer::new(config.packet_size),
        }
    }

    /// Hands over the next bytes of the stream, cut anywhere
    ///
    /// The bytes are only kept; [`Demuxer::poll_event`] reads them.
    ///
    /// # Panics
    /// Panics if called after [`Demuxer::finish`] before
    /// [`Demuxer::poll_event`] has returned `None`.
    pub fn handle_input(&mut self, bytes: &[u8]) {
        self.framer.extend(bytes);
    }

    /// Reads the input as far as the next event and returns it, or `None`
    /// when the input handed over so far holds no more events
    pub fn poll_event(&mut self) -> Option<Event<'_>> {
        loop {
            let event = match self.framer.step() {
                Output::NeedMoreData => return None,
                Output::Packet { .. } => continue,
                Output::SyncAcquired(packet_size) => Event::SyncAcquired { packet_size },
                Output::SyncByteMissing { found } => Event::Error(Error::SyncByteMissing { found }),
                Output::SyncLost => Event::Error(Error::SyncLost),
                Output::Unsynchronized(range) => Event::Unsynchronized {
                    bytes: self.framer.bytes(range),
                },
            };
            return Some(event);
        }
    }

    /// Ends the input at the bytes handed over so far
    ///
    /// The events [`Demuxer::poll_event`] returns next include a last packet
    /// that lacks only the carrier bytes after it, and every other byte left
    /// as [`Event::Unsynchronized`], so nothing is joined across the end. Poll
    /// until `None` before handing over more input. The packet size found
    /// stays, so the same demuxer can then take a stream that goes on.
    pub fn finish(&mut self) {
        self.framer.finish();
    }
}
