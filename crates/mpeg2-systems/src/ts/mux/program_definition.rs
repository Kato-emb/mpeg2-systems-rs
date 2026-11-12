//! The program registered with a [`Muxer`](super::Muxer), its streams, and
//! the PAT and the PMT made of them

use alloc::vec::Vec;
use core::mem;
use core::num::NonZeroU16;

use super::Error;
use crate::psi::VersionNumber;
use crate::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
use crate::psi::pmt::{self, ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
use crate::psi::section::Section;
use crate::stream_type::StreamType;
use crate::time::SystemTimeClock;
use crate::ts::Pid;

/// The program registered, and the streams registered with it
///
/// Program definition, ISO/IEC 13818-1 §2.4.4.9
#[derive(Debug, Clone)]
pub(super) struct ProgramDefinition {
    program_number: NonZeroU16,
    program_map_pid: Pid,
    /// The `pcr_pid` registered, `None` for the muxer to choose it
    // Why not the PCR PID chosen: it is chosen from the streams, which are
    // registered after the program.
    pcr_pid: Option<Pid>,
    program_info: Vec<u8>,
    streams: Vec<Stream>,
}

/// Refuses the PMT of `program_info` and `elementary_streams` where it would
/// be longer than its syntax allows, from the lengths alone, before anything
/// of them is copied
fn check_pmt_length<'stream>(
    program_info: &[u8],
    elementary_streams: impl IntoIterator<Item = ElementaryStream<'stream>>,
) -> Result<(), Error> {
    pmt::check_lengths(program_info.into(), elementary_streams)
        .map_err(|error| Error::TsProgramMapSection { error })
}

/// A stream registered with the program
#[derive(Debug, Clone)]
pub(super) struct Stream {
    pid: Pid,
    stream_type: StreamType,
    es_info: Vec<u8>,
    /// The time of the STC the DTS (the PTS without one) of the last PES
    /// packet given on the stream samples, `None` before the first
    // Why not one time of the muxer: a stream may run behind another.
    last_timestamp: Option<SystemTimeClock>,
}

impl Stream {
    pub(super) const fn last_timestamp(&self) -> Option<SystemTimeClock> {
        self.last_timestamp
    }
}

impl ProgramDefinition {
    /// Returns the program `program_number`, its PMT on `program_map_pid`,
    /// with no `program_info`, no `pcr_pid` and no stream
    ///
    /// # Errors
    ///
    /// * [`ProgramNumber`](Error::ProgramNumber): `program_number` is 0.
    /// * [`Pid`](Error::Pid): `program_map_pid` is outside 0x0010 to 0x1FFE.
    pub(super) fn new(program_number: u16, program_map_pid: Pid) -> Result<Self, Error> {
        let Some(program_number) = NonZeroU16::new(program_number) else {
            return Err(Error::ProgramNumber { program_number });
        };
        if !program_map_pid.is_assignable() {
            return Err(Error::Pid {
                pid: program_map_pid,
            });
        }
        Ok(Self {
            program_number,
            program_map_pid,
            pcr_pid: None,
            program_info: Vec::new(),
            streams: Vec::new(),
        })
    }

    pub(super) const fn program_number(&self) -> u16 {
        self.program_number.get()
    }

    pub(super) const fn program_map_pid(&self) -> Pid {
        self.program_map_pid
    }

    /// Sets the `pcr_pid` registered
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `pcr_pid` is outside 0x0010 to 0x1FFE.
    pub(super) fn set_pcr_pid(&mut self, pcr_pid: Pid) -> Result<(), Error> {
        if !pcr_pid.is_assignable() {
            return Err(Error::Pid { pid: pcr_pid });
        }
        self.pcr_pid = Some(pcr_pid);
        Ok(())
    }

    /// Sets `program_info`, a copy of it, where the PAT of
    /// `transport_stream_id` and the PMT take it; a call refused leaves the
    /// program as it was
    ///
    /// # Errors
    ///
    /// * [`TsProgramMapSection`](Error::TsProgramMapSection): the PMT with
    ///   `program_info` is refused, its length before `program_info` is
    ///   copied.
    pub(super) fn set_program_info(
        &mut self,
        program_info: &[u8],
        transport_stream_id: u16,
    ) -> Result<(), Error> {
        check_pmt_length(program_info, self.elementary_streams())?;
        let previous = mem::replace(&mut self.program_info, program_info.to_vec());
        self.check(transport_stream_id).inspect_err(|_| {
            self.program_info = previous;
        })
    }

    /// Registers `stream`, a copy of its `es_info`, where the PAT of
    /// `transport_stream_id` and the PMT take it; a call refused leaves the
    /// program as it was
    ///
    /// # Errors
    ///
    /// * [`Pid`](Error::Pid): `elementary_pid` is outside 0x0010 to 0x1FFE.
    /// * [`PidInUse`](Error::PidInUse): `elementary_pid` is that of a stream
    ///   registered already, or of the PMT.
    /// * [`TsProgramMapSection`](Error::TsProgramMapSection): the PMT with the
    ///   stream is refused, its length before `es_info` is copied.
    pub(super) fn add_stream(
        &mut self,
        stream: ElementaryStream<'_>,
        transport_stream_id: u16,
    ) -> Result<(), Error> {
        let pid = stream.elementary_pid;
        if !pid.is_assignable() {
            return Err(Error::Pid { pid });
        }
        if pid == self.program_map_pid || self.stream(pid).is_some() {
            return Err(Error::PidInUse { pid });
        }
        check_pmt_length(
            &self.program_info,
            self.elementary_streams().chain(core::iter::once(stream)),
        )?;
        self.streams.push(Stream {
            pid,
            stream_type: stream.stream_type,
            es_info: stream.es_info.as_bytes().to_vec(),
            last_timestamp: None,
        });
        self.check(transport_stream_id).inspect_err(|_| {
            self.streams.pop();
        })
    }

    /// Returns the stream registered on `pid`
    pub(super) fn stream(&self, pid: Pid) -> Option<&Stream> {
        self.streams.iter().find(|stream| stream.pid == pid)
    }

    /// Sets the `last_timestamp` of the stream registered on `pid`, if any
    pub(super) fn set_last_timestamp(&mut self, pid: Pid, time: SystemTimeClock) {
        if let Some(stream) = self.streams.iter_mut().find(|stream| stream.pid == pid) {
            stream.last_timestamp = Some(time);
        }
    }

    /// Returns the latest `last_timestamp` of the streams, the time of the
    /// STC the input has reached, `None` before the first time stamp
    pub(super) fn latest_timestamp(&self) -> Option<SystemTimeClock> {
        self.streams
            .iter()
            .filter_map(|stream| stream.last_timestamp)
            .max()
    }

    /// Returns the PID the PCR goes on: the `pcr_pid` registered, or else
    /// the first video stream, or else the first stream
    pub(super) fn pcr_pid(&self) -> Option<Pid> {
        self.pcr_pid.or_else(|| {
            self.streams
                .iter()
                .find(|stream| stream.stream_type.is_video())
                .or_else(|| self.streams.first())
                .map(|stream| stream.pid)
        })
    }

    /// Appends the program loop of the PAT and the elementary stream loop
    /// of the PMT to `scratch`, and returns the length of the program loop
    fn write_loops(&self, scratch: &mut Vec<u8>) -> Result<usize, Error> {
        let pat_loop_length = self.write_loops_of_pat(scratch)?;
        ElementaryStreamLoop::write_from(self.elementary_streams(), scratch)
            .map_err(|error| Error::TsProgramMapSection { error })?;
        Ok(pat_loop_length)
    }

    /// Appends the program loop of the PAT to `out`, and returns its length
    fn write_loops_of_pat(&self, out: &mut Vec<u8>) -> Result<usize, Error> {
        let start = out.len();
        ProgramLoop::write_from(
            [Program::ProgramMap {
                program_number: self.program_number,
                program_map_pid: self.program_map_pid,
            }],
            out,
        )
        .map_err(|error| Error::ProgramAssociationSection { error })?;
        Ok(out.len().saturating_sub(start))
    }

    /// Writes the bodies of the PAT of `transport_stream_id` and the PMT,
    /// `version_number` 0, into `bodies`, and returns the two sections that
    /// carry them
    pub(super) fn psi_sections<'body>(
        &self,
        transport_stream_id: u16,
        bodies: &'body mut Vec<u8>,
    ) -> Result<(Section<'body>, Section<'body>), Error> {
        bodies.clear();
        let pat_body_length = self.write_loops_of_pat(bodies)?;
        TsProgramMapSection::write_body_with(
            self.pcr_pid(),
            self.program_info.as_slice().into(),
            bodies,
            |body| ElementaryStreamLoop::write_from(self.elementary_streams(), body),
        )
        .map_err(|error| Error::TsProgramMapSection { error })?;
        let (pat_body, pmt_body) = bodies.split_at(pat_body_length);
        let version_number = VersionNumber(0);
        Ok((
            ProgramAssociationSection::new(transport_stream_id, version_number).section(pat_body),
            TsProgramMapSection::new(self.program_number.get(), version_number).section(pmt_body),
        ))
    }

    /// Returns the entry of the elementary stream loop of each stream
    fn elementary_streams(&self) -> impl Iterator<Item = ElementaryStream<'_>> {
        self.streams.iter().map(|stream| {
            let mut entry = ElementaryStream::new(stream.stream_type, stream.pid);
            entry.es_info = stream.es_info.as_slice().into();
            entry
        })
    }

    /// Returns the PAT of `transport_stream_id` and the PMT, `version_number`
    /// 0, their loops `pat_loop` and `pmt_loop` as
    /// [`write_loops`](Self::write_loops) writes them
    fn sections<'loops>(
        &'loops self,
        transport_stream_id: u16,
        pat_loop: &'loops [u8],
        pmt_loop: &'loops [u8],
    ) -> (
        ProgramAssociationSection<'loops>,
        TsProgramMapSection<'loops>,
    ) {
        let version_number = VersionNumber(0);
        let mut pat = ProgramAssociationSection::new(transport_stream_id, version_number);
        pat.programs = ProgramLoop::from(pat_loop);
        let mut pmt = TsProgramMapSection::new(self.program_number.get(), version_number);
        pmt.pcr_pid = self.pcr_pid();
        pmt.program_info = self.program_info.as_slice().into();
        pmt.elementary_streams = ElementaryStreamLoop::from(pmt_loop);
        (pat, pmt)
    }

    /// Writes the PAT and the PMT, so that what their `write` refuses is
    /// refused on registration rather than on input
    fn check(&self, transport_stream_id: u16) -> Result<(), Error> {
        let mut loops = Vec::new();
        let pat_loop_length = self.write_loops(&mut loops)?;
        let (pat_loop, pmt_loop) = loops.split_at(pat_loop_length);
        let (pat, pmt) = self.sections(transport_stream_id, pat_loop, pmt_loop);
        let mut out = Vec::new();
        pat.write(&mut out)
            .map_err(|error| Error::ProgramAssociationSection { error })?;
        pmt.write(&mut out)
            .map_err(|error| Error::TsProgramMapSection { error })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(value: u16) -> Pid {
        Pid::try_from(value).unwrap()
    }

    #[test]
    fn without_a_pcr_pid_the_pcr_goes_on_the_first_stream_of_a_video_stream_type_or_else_the_first_stream()
     {
        let first_pcr_pid = |stream_types: &[StreamType]| {
            let mut program = ProgramDefinition::new(1, pid(0x1000)).unwrap();
            for (pid_value, stream_type) in (0x0100..).zip(stream_types) {
                program
                    .add_stream(ElementaryStream::new(*stream_type, pid(pid_value)), 1)
                    .unwrap();
            }
            program.pcr_pid().map(Pid::get)
        };
        assert_eq!(
            [
                first_pcr_pid(&[StreamType::ISO_IEC_13818_7_AUDIO, StreamType::HEVC_VIDEO]),
                first_pcr_pid(&[
                    StreamType::ISO_IEC_13818_7_AUDIO,
                    StreamType::ISO_IEC_13818_2_VIDEO
                ]),
                first_pcr_pid(&[StreamType::ISO_IEC_13818_7_AUDIO, StreamType::AVC_VIDEO]),
                first_pcr_pid(&[
                    StreamType::ISO_IEC_13818_7_AUDIO,
                    StreamType::SVC_VIDEO_SUB_BITSTREAM
                ]),
                first_pcr_pid(&[
                    StreamType::SVC_VIDEO_SUB_BITSTREAM,
                    StreamType::ISO_IEC_13818_7_AUDIO
                ]),
                first_pcr_pid(&[
                    StreamType::ISO_IEC_13818_7_AUDIO,
                    StreamType::LCEVC_VIDEO,
                    StreamType::ISO_IEC_13818_3_AUDIO
                ]),
            ],
            [
                Some(0x0101),
                Some(0x0101),
                Some(0x0101),
                Some(0x0100),
                Some(0x0100),
                Some(0x0100)
            ]
        );
    }
}
