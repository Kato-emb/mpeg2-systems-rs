//! When the PAT and the PMT, and the PCR, are due: the times a
//! [`Muxer`](super::Muxer) sends them before a PES packet

use crate::time::SystemTimeClock;

/// The times the PAT and the PMT and the PCR were last sent, before
/// [`MuxerConfig::mux_delay`](super::MuxerConfig::mux_delay) is taken off
// Why not a field of the time the input has reached: it is the latest
// `last_timestamp` of the streams, which every time stamp taken sets.
#[derive(Debug, Clone, Copy)]
pub(super) struct Schedule {
    pub(super) psi_sent_at: SystemTimeClock,
    pub(super) pcr_sent_at: SystemTimeClock,
    /// Whether more than one PCR has been sent
    // Why not compare pcr_sent_at with the time of the first PCR: that time
    // is kept nowhere else, so it would be a field for this alone.
    pub(super) several_pcrs: bool,
}

/// The times at which the PAT and the PMT, or the PCR, are sent before a
/// PES packet: `count` times `step` ticks of the STC apart from `first`
#[derive(Debug, Clone, Copy)]
pub(super) struct Times {
    first: SystemTimeClock,
    pub(super) count: u64,
    step: u64,
}

impl Times {
    /// Returns the one time `at`
    pub(super) const fn once(at: SystemTimeClock) -> Self {
        Self {
            first: at,
            count: 1,
            step: 0,
        }
    }

    /// Returns the times `interval` apart after `sent_at` up to `clock`, or
    /// `clock` alone when `interval` is 0
    pub(super) const fn due(
        sent_at: SystemTimeClock,
        clock: SystemTimeClock,
        interval: u64,
    ) -> Self {
        match clock.ticks_since(sent_at).checked_div(interval) {
            // Why not skip sending at an interval of 0: the API promises PSI
            // before every PES packet, including when the clock has not advanced.
            None => Self::once(clock),
            Some(count) => Self {
                first: sent_at.add_ticks(interval),
                count,
                step: interval,
            },
        }
    }

    /// Returns the last of the times, `None` when there are none
    pub(super) fn last(self) -> Option<SystemTimeClock> {
        let steps = self.count.checked_sub(1)?;
        Some(self.first.add_ticks(steps.saturating_mul(self.step)))
    }

    pub(super) fn iter(self) -> impl Iterator<Item = SystemTimeClock> {
        (0..self.count).map(move |index| self.first.add_ticks(index.saturating_mul(self.step)))
    }
}
