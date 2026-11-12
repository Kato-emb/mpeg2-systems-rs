//! When the PAT and the PMT, and the PCR, are due: the times a
//! [`Muxer`](super::Muxer) sends them before a PES packet

use core::time::Duration;

use super::Error;
use crate::time::SystemTimeClock;

/// The longest time from one PCR to the next, ISO/IEC 13818-1 §2.7.2
const LONGEST_PCR_INTERVAL: Duration = Duration::from_millis(100);

/// Refuses a `pcr_interval` shorter than a tick of the 27 MHz system clock,
/// which would send two PCRs of one value, or longer than §2.7.2 allows
pub(super) fn check_pcr_interval(pcr_interval: Duration) -> Result<(), Error> {
    if SystemTimeClock::ticks_in(pcr_interval) == 0 || pcr_interval > LONGEST_PCR_INTERVAL {
        return Err(Error::PcrInterval { pcr_interval });
    }
    Ok(())
}

/// The times the PAT and the PMT and the PCR were last sent, before
/// [`MuxerConfig::mux_delay`](super::MuxerConfig::mux_delay) is taken off
// Why not a field of the time the input has reached: it is the latest
// `last_timestamp` of the streams, which every time stamp taken sets.
#[derive(Debug, Clone, Copy)]
pub(super) struct Schedule {
    psi_sent_at: SystemTimeClock,
    pcr_sent_at: SystemTimeClock,
    /// Whether more than one PCR has been sent
    // Why not compare pcr_sent_at with the time of the first PCR: that time
    // is kept nowhere else, so it would be a field for this alone.
    several_pcrs: bool,
}

impl Schedule {
    /// Returns the schedule with a PCR sent at `at` beside those sent before
    /// it, from which the next PCR is due
    const fn with_pcr_sent_at(self, at: SystemTimeClock) -> Self {
        Self {
            pcr_sent_at: at,
            several_pcrs: true,
            ..self
        }
    }

    /// Returns the time of the PCR that ends a stream which carries one PCR
    /// only: `pcr_interval` ticks after it, or `reached` if that is later;
    /// `None` once more than one PCR has been sent
    pub(super) fn closing_pcr_at(
        self,
        pcr_interval: u64,
        reached: SystemTimeClock,
    ) -> Option<SystemTimeClock> {
        (!self.several_pcrs).then(|| self.pcr_sent_at.add_ticks(pcr_interval).max(reached))
    }

    /// Takes the PCR [`closing_pcr_at`](Self::closing_pcr_at) gave as sent
    pub(super) const fn closing_pcr_sent(&mut self) {
        self.several_pcrs = true;
    }
}

/// What a [`Muxer`](super::Muxer) sends before one PES packet, at `clock`,
/// and the schedule after it
#[derive(Debug, Clone, Copy)]
pub(super) struct Plan {
    clock: SystemTimeClock,
    /// The schedule before the PES packet, `None` before the first
    previous: Option<Schedule>,
    psi: Times,
    pcrs: Times,
    /// Whether a PCR is due at `clock` on the PES packet's PID
    pcr_due: bool,
    /// Whether a PCR at `clock` goes in the PES packet's first Transport
    /// Packet
    pcr_in_packet: bool,
}

/// One thing a [`Plan`] sends before the PES packet
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sending {
    /// The PAT and the PMT
    Psi,
    /// A PCR in a Transport Packet of its own, at this time
    Pcr(SystemTimeClock),
}

impl Plan {
    /// Returns what is due before a PES packet at `clock`, after `previous`,
    /// on the PCR PID when `on_pcr_pid`: the PAT and the PMT each
    /// `psi_interval` ticks, and the PCR each `pcr_interval` ticks, or once
    /// each before the first PES packet
    pub(super) fn new(
        previous: Option<Schedule>,
        clock: SystemTimeClock,
        psi_interval: u64,
        pcr_interval: u64,
        on_pcr_pid: bool,
    ) -> Self {
        let (psi, pcrs) = match previous {
            None => (Times::once(clock), Times::once(clock)),
            Some(schedule) => (
                Times::due(schedule.psi_sent_at, clock, psi_interval),
                Times::due(schedule.pcr_sent_at, clock, pcr_interval),
            ),
        };
        Self {
            clock,
            previous,
            psi,
            pcrs,
            pcr_due: on_pcr_pid && pcrs.last() == Some(clock),
            pcr_in_packet: false,
        }
    }

    /// Returns whether a PCR is due at `clock` on the PES packet's PID
    pub(super) const fn pcr_due(&self) -> bool {
        self.pcr_due
    }

    /// Returns whether `clock` is after the time of the PCR sent before
    pub(super) fn after_last_pcr(&self) -> bool {
        self.previous
            .is_none_or(|schedule| self.clock > schedule.pcr_sent_at)
    }

    /// Returns the plan with a PCR at `clock` in the PES packet's first
    /// Transport Packet when `pcr_in_packet`; a PCR due at `clock` that does
    /// not go there goes on its own before it
    pub(super) const fn with_pcr_in_packet(mut self, pcr_in_packet: bool) -> Self {
        self.pcr_in_packet = pcr_in_packet;
        self
    }

    /// Returns how many times the PAT and the PMT are sent
    pub(super) const fn psi_count(&self) -> u64 {
        self.psi.count
    }

    /// Returns how many PCRs on their own fill a leap of the time stamps
    /// before the PES packet, the one due at `clock` left out
    // Why not count the PCR due at the PES packet, split from it, with the
    // PCRs that fill a leap: it is sent at the PES packet's time, and is part
    // of what the PES packet adds to the output.
    pub(super) fn filling_pcrs(&self) -> u64 {
        self.pcrs.count.saturating_sub(u64::from(self.pcr_due))
    }

    /// Returns whether the PCR due at `clock` goes on its own
    pub(super) const fn pcr_split(&self) -> bool {
        self.pcr_due && !self.pcr_in_packet
    }

    /// Returns what is sent before the PES packet, in the order it is sent
    pub(super) fn sendings(&self) -> impl Iterator<Item = Sending> {
        let pcrs_alone = self
            .pcrs
            .count
            .saturating_sub(u64::from(self.pcr_due && self.pcr_in_packet));
        let mut psi = self.psi.iter().peekable();
        let mut pcrs = self
            .pcrs
            .iter()
            .take(usize::try_from(pcrs_alone).unwrap_or(usize::MAX))
            .peekable();
        // Why not all the PSI and then all the PCRs: each goes at its time,
        // the PSI first where the two fall together, so that a PCR stays
        // the time its bytes arrive at.
        core::iter::from_fn(move || match (psi.peek(), pcrs.peek()) {
            (Some(psi_at), Some(pcr_at)) if psi_at <= pcr_at => psi.next().map(|_| Sending::Psi),
            (Some(_), None) => psi.next().map(|_| Sending::Psi),
            (_, Some(_)) => pcrs.next().map(Sending::Pcr),
            (None, None) => None,
        })
    }

    /// Returns the schedule once the PES packet is sent
    pub(super) fn next(self) -> Schedule {
        let next = match self.previous {
            None => Schedule {
                psi_sent_at: self.clock,
                pcr_sent_at: self.clock,
                several_pcrs: false,
            },
            Some(schedule) => Schedule {
                psi_sent_at: self.psi.last().unwrap_or(schedule.psi_sent_at),
                pcr_sent_at: self.pcrs.last().unwrap_or(schedule.pcr_sent_at),
                several_pcrs: schedule.several_pcrs || self.pcrs.count > 0,
            },
        };
        if self.pcr_in_packet && !self.pcr_due {
            next.with_pcr_sent_at(self.clock)
        } else {
            next
        }
    }
}

/// The times at which the PAT and the PMT, or the PCR, are sent before a
/// PES packet: `count` times `step` ticks of the STC apart from `first`
#[derive(Debug, Clone, Copy)]
struct Times {
    first: SystemTimeClock,
    count: u64,
    step: u64,
}

impl Times {
    /// Returns the one time `at`
    const fn once(at: SystemTimeClock) -> Self {
        Self {
            first: at,
            count: 1,
            step: 0,
        }
    }

    /// Returns the times `interval` apart after `sent_at` up to `clock`, or
    /// `clock` alone when `interval` is 0
    const fn due(sent_at: SystemTimeClock, clock: SystemTimeClock, interval: u64) -> Self {
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
    fn last(self) -> Option<SystemTimeClock> {
        let steps = self.count.checked_sub(1)?;
        Some(self.first.add_ticks(steps.saturating_mul(self.step)))
    }

    fn iter(self) -> impl Iterator<Item = SystemTimeClock> {
        (0..self.count).map(move |index| self.first.add_ticks(index.saturating_mul(self.step)))
    }
}
