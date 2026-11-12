//! The system header and the program stream map a [`Muxer`](super::Muxer)
//! sends: when they are due, the version of the map, and their bytes

use alloc::vec::Vec;
use core::time::Duration;

use super::Error;
use crate::psi::VersionNumber;
use crate::time::SystemTimeClock;

/// The system header and the program stream map sent, and those to send
#[derive(Debug, Clone)]
pub(super) struct Headers {
    /// The time of the STC the SCR of the last pack that carried the system
    /// header and the program stream map as it stands samples, before
    /// [`MuxerConfig::mux_delay`](super::MuxerConfig::mux_delay) is taken
    /// off, `None` when no pack has
    // Why not a flag for a change of the map besides it: a change makes the
    // map due in the next pack, as no sending at all does.
    sent_at: Option<SystemTimeClock>,
    /// `program_stream_map_version` of the program stream map as it stands
    // Why not count the changes: those made before the next PES packet are
    // one change of the map a receiver sees.
    map_version: VersionNumber,
    /// The system header and the program stream map the PES packet being
    /// taken carries
    // Why not a Vec per PES packet: at a short `header_interval` every PES
    // packet carries them, and that path allocates nothing.
    bytes: Vec<u8>,
    /// The stream bound loop and the elementary stream loop of `bytes`, as
    /// they are written
    // Why not a Vec per PES packet: as for `bytes`.
    loops: Vec<u8>,
}

impl Headers {
    /// Returns the headers of none sent, the map of
    /// `program_stream_map_version` 0
    pub(super) const fn new() -> Self {
        Self {
            sent_at: None,
            map_version: VersionNumber::from_5_bits(0),
            bytes: Vec::new(),
            loops: Vec::new(),
        }
    }

    /// Returns `program_stream_map_version` of the program stream map as it
    /// stands
    pub(super) const fn map_version(&self) -> VersionNumber {
        self.map_version
    }

    /// Returns whether the headers are due in a pack whose SCR samples `at`:
    /// `header_interval` or more after the last pack that carried them, or
    /// where none has since the map changed
    pub(super) fn is_due(&self, at: Option<SystemTimeClock>, header_interval: Duration) -> bool {
        self.sent_at.zip(at).is_none_or(|(sent_at, at)| {
            at.ticks_since(sent_at) >= SystemTimeClock::ticks_in(header_interval)
        })
    }

    /// Takes the bytes `write` writes of the headers, given the
    /// `program_stream_map_version` and the bytes to write their loops in,
    /// and returns the bytes of the system header `write` returns
    pub(super) fn write(
        &mut self,
        write: impl FnOnce(VersionNumber, &mut Vec<u8>, &mut Vec<u8>) -> Result<usize, Error>,
    ) -> Result<usize, Error> {
        self.bytes.clear();
        write(self.map_version, &mut self.loops, &mut self.bytes)
    }

    /// Takes none of the headers for the PES packet being taken
    pub(super) fn clear(&mut self) {
        self.bytes.clear();
    }

    /// Returns the bytes of the headers the PES packet being taken carries
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Records that the pack whose SCR samples `at` carried the headers
    pub(super) const fn sent(&mut self, at: Option<SystemTimeClock>) {
        self.sent_at = at;
    }

    /// Makes the program stream map as it now stands due in the next pack,
    /// its version incremented when one has been sent since the last change
    pub(super) fn map_changed(&mut self) {
        if self.sent_at.take().is_some() {
            self.map_version = self.map_version.next();
        }
    }
}
