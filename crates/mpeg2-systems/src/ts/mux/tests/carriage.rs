use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::ts::demux::{Demuxer, DemuxerConfig, Event};

/// What a Transport Packet carries of what a [`PesPacketCarriage`] sets, and
/// of what the muxer sets beside it
#[derive(Debug, Clone, PartialEq, Eq)]
struct Carried {
    pid: u16,
    payload_unit_start_indicator: bool,
    transport_priority: bool,
    random_access_indicator: bool,
    elementary_stream_priority_indicator: bool,
    program_clock_reference: Option<ClockReference>,
    transport_private_data: Option<Vec<u8>>,
}

fn carried(bytes: &[u8]) -> Vec<Carried> {
    bytes
        .chunks(188)
        .map(|chunk| {
            let packet = TransportPacket::read(chunk).unwrap();
            let field = packet.adaptation_field.as_ref();
            Carried {
                pid: packet.pid.get(),
                payload_unit_start_indicator: packet.payload_unit_start_indicator,
                transport_priority: packet.transport_priority,
                random_access_indicator: field.is_some_and(|field| field.random_access_indicator),
                elementary_stream_priority_indicator: field
                    .is_some_and(|field| field.elementary_stream_priority_indicator),
                program_clock_reference: field.and_then(|field| field.program_clock_reference),
                transport_private_data: field
                    .and_then(|field| field.transport_private_data)
                    .map(<[u8]>::to_vec),
            }
        })
        .collect()
}

/// A Transport Packet of `pid` that carries none of what a carriage sets
fn plain(pid: u16, payload_unit_start_indicator: bool) -> Carried {
    Carried {
        pid,
        payload_unit_start_indicator,
        transport_priority: false,
        random_access_indicator: false,
        elementary_stream_priority_indicator: false,
        program_clock_reference: None,
        transport_private_data: None,
    }
}

/// What the muxer of the default settings makes of the PES packets given
/// in turn, each `(pid, dts, carriage)`: the result of each, and the output
fn multiplexed(inputs: &[(u16, u64, PesPacketCarriage<'_>)]) -> (Vec<Result<(), Error>>, Vec<u8>) {
    let mut muxer = muxer_of(MuxerConfig::new());
    let payload = vec![0xaa; 400];
    let results = inputs
        .iter()
        .map(|(pid_value, dts, carriage)| {
            let stream_id = if *pid_value == VIDEO_PID { 0xe0 } else { 0xc0 };
            muxer.handle_pes_packet_with_carriage(
                pid(*pid_value),
                &pes_packet(stream_id, &payload, Some(*dts)),
                carriage,
            )
        })
        .collect();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    (results, out)
}

/// The output of [`multiplexed`], each PES packet taken
fn output_of(inputs: &[(u16, u64, PesPacketCarriage<'_>)]) -> Vec<u8> {
    let (results, out) = multiplexed(inputs);
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    out
}

/// The Transport Packets of `pid` in `out`
fn of_pid(out: &[u8], pid_value: u16) -> Vec<Carried> {
    carried(out)
        .into_iter()
        .filter(|packet| packet.pid == pid_value)
        .collect()
}

#[test]
fn the_fields_set_go_in_the_adaptation_field_of_the_first_transport_packet_only() {
    let private_data = [0x01, 0x02, 0x03];
    let carriage = PesPacketCarriage::new()
        .with_random_access_indicator(true)
        .with_elementary_stream_priority_indicator(true)
        .with_transport_private_data(&private_data);
    let audio = of_pid(&output_of(&[(AUDIO_PID, 90_000, carriage)]), AUDIO_PID);
    let mut expected = vec![Carried {
        random_access_indicator: true,
        elementary_stream_priority_indicator: true,
        transport_private_data: Some(private_data.to_vec()),
        ..plain(AUDIO_PID, true)
    }];
    expected.resize(audio.len(), plain(AUDIO_PID, false));
    assert_eq!((audio, expected.len() > 1), (expected, true));
}

#[test]
fn transport_priority_is_set_on_every_transport_packet_of_the_pes_packet_only() {
    let carriage = PesPacketCarriage::new().with_transport_priority(true);
    let out = output_of(&[
        (VIDEO_PID, 90_000, PesPacketCarriage::new()),
        (AUDIO_PID, 93_600, carriage),
    ]);
    let audio = of_pid(&out, AUDIO_PID);
    let mut expected = vec![Carried {
        transport_priority: true,
        ..plain(AUDIO_PID, true)
    }];
    expected.resize(
        audio.len(),
        Carried {
            transport_priority: true,
            ..plain(AUDIO_PID, false)
        },
    );
    let others_with_priority: Vec<Carried> = carried(&out)
        .into_iter()
        .filter(|packet| packet.pid != AUDIO_PID && packet.transport_priority)
        .collect();
    assert_eq!(
        (audio, others_with_priority, expected.len() > 1),
        (expected, Vec::new(), true)
    );
}

#[test]
fn the_fields_set_go_beside_a_pcr_in_the_same_packet() {
    let private_data = [0x0a; 8];
    let carriage = PesPacketCarriage::new()
        .with_random_access_indicator(true)
        .with_transport_private_data(&private_data);
    let video = of_pid(&output_of(&[(VIDEO_PID, 90_000, carriage)]), VIDEO_PID);
    assert_eq!(
        video.first().cloned(),
        Some(Carried {
            random_access_indicator: true,
            program_clock_reference: Some(pcr((90_000 - DELAY) * 300)),
            transport_private_data: Some(private_data.to_vec()),
            ..plain(VIDEO_PID, true)
        })
    );
}

fn pcrs_of(out: &[u8]) -> Vec<(u16, ClockReference)> {
    carried(out)
        .iter()
        .filter_map(|packet| {
            packet
                .program_clock_reference
                .map(|value| (packet.pid, value))
        })
        .collect()
}

#[test]
fn random_access_on_the_pcr_pid_puts_a_pcr_in_the_packet_and_starts_the_pcr_interval_there() {
    let random_access = PesPacketCarriage::new().with_random_access_indicator(true);
    let out = output_of(&[
        (VIDEO_PID, 90_000, PesPacketCarriage::new()),
        (VIDEO_PID, 90_900, random_access),
        (VIDEO_PID, 93_600, PesPacketCarriage::new()),
        (VIDEO_PID, 94_500, PesPacketCarriage::new()),
    ]);
    let at = |dts: u64| (VIDEO_PID, pcr((dts - DELAY) * 300));
    assert_eq!(pcrs_of(&out), [at(90_000), at(90_900), at(94_500)]);
}

#[test]
fn random_access_off_the_pcr_pid_adds_no_pcr() {
    let random_access = PesPacketCarriage::new().with_random_access_indicator(true);
    let out = output_of(&[
        (VIDEO_PID, 90_000, PesPacketCarriage::new()),
        (AUDIO_PID, 90_900, random_access),
    ]);
    assert_eq!(pcrs_of(&out), [(VIDEO_PID, pcr((90_000 - DELAY) * 300))]);
}

#[test]
fn a_carriage_that_sets_nothing_carries_the_pes_packet_as_handle_pes_packet_does() {
    let payload = vec![0xaa; 400];
    let inputs = [
        (VIDEO_PID, 90_000),
        (AUDIO_PID, 90_900),
        (VIDEO_PID, 99_000),
    ];
    let mut plain_muxer = muxer_of(MuxerConfig::new());
    let mut carried_muxer = muxer_of(MuxerConfig::new());
    for (pid_value, dts) in inputs {
        let packet = pes_packet(0xe0, &payload, Some(dts));
        plain_muxer
            .handle_pes_packet(pid(pid_value), &packet)
            .unwrap();
        carried_muxer
            .handle_pes_packet_with_carriage(pid(pid_value), &packet, &PesPacketCarriage::new())
            .unwrap();
    }
    let (mut plain_out, mut carried_out) = (Vec::new(), Vec::new());
    plain_muxer.poll_output(&mut plain_out);
    carried_muxer.poll_output(&mut carried_out);
    assert_eq!(carried_out, plain_out);
}

/// The result of giving a PES packet on `pid` at `dts`, after a first one on
/// the video PID at 90 000, carrying `private_data`, random access as given,
/// and the output then
fn with_private_data(
    pid_value: u16,
    dts: u64,
    random_access_indicator: bool,
    private_data: &[u8],
) -> (Result<(), Error>, Vec<u8>) {
    let (results, out) = multiplexed(&[
        (VIDEO_PID, 90_000, PesPacketCarriage::new()),
        (
            pid_value,
            dts,
            PesPacketCarriage::new()
                .with_random_access_indicator(random_access_indicator)
                .with_transport_private_data(private_data),
        ),
    ]);
    (*results.last().unwrap(), out)
}

#[test]
fn transport_private_data_past_the_adaptation_field_is_refused_and_leaves_the_muxer_as_it_was() {
    let (refused, after_refusal) = with_private_data(AUDIO_PID, 90_900, false, &[0x0b; 181]);
    let mut first_only = Vec::new();
    let mut muxer = muxer_of(MuxerConfig::new());
    muxer
        .handle_pes_packet(
            pid(VIDEO_PID),
            &pes_packet(0xe0, &[0xaa; 400], Some(90_000)),
        )
        .unwrap();
    muxer.poll_output(&mut first_only);
    let (taken, _) = with_private_data(AUDIO_PID, 90_900, false, &[0x0b; 180]);
    let (taken_off_the_pcr_pid, _) = with_private_data(AUDIO_PID, 90_900, true, &[0x0b; 180]);
    assert_eq!(
        (refused, after_refusal, taken, taken_off_the_pcr_pid),
        (
            Err(Error::TransportPrivateDataLength {
                length: 181,
                limit: 180
            }),
            first_only,
            Ok(()),
            Ok(())
        )
    );
}

#[test]
fn a_pcr_due_with_transport_private_data_too_long_to_share_its_packet_goes_alone_before_it() {
    let private_data = [0x0c; 180];
    let (result, out) = with_private_data(VIDEO_PID, 93_600, false, &private_data);
    let video = of_pid(&out, VIDEO_PID);
    let first_pes_packet = video
        .iter()
        .position(|packet| packet.payload_unit_start_indicator)
        .unwrap();
    let second_pes_packet = video
        .iter()
        .rposition(|packet| packet.payload_unit_start_indicator)
        .unwrap();
    assert_eq!(
        (
            result,
            video.get(second_pes_packet - 1).cloned(),
            video.get(second_pes_packet).cloned(),
            first_pes_packet < second_pes_packet
        ),
        (
            Ok(()),
            Some(Carried {
                program_clock_reference: Some(pcr((93_600 - DELAY) * 300)),
                ..plain(VIDEO_PID, false)
            }),
            Some(Carried {
                transport_private_data: Some(private_data.to_vec()),
                ..plain(VIDEO_PID, true)
            }),
            true
        )
    );
}

#[test]
fn random_access_on_the_pcr_pid_takes_as_much_transport_private_data_as_fits_beside_the_pcr() {
    let (taken, out) = with_private_data(VIDEO_PID, 90_900, true, &[0x0d; 174]);
    let (refused, _) = with_private_data(VIDEO_PID, 90_900, true, &[0x0d; 175]);
    let last_start = carried(&out)
        .into_iter()
        .rfind(|packet| packet.pid == VIDEO_PID && packet.payload_unit_start_indicator);
    assert_eq!(
        (taken, refused, last_start),
        (
            Ok(()),
            Err(Error::TransportPrivateDataLength {
                length: 175,
                limit: 174
            }),
            Some(Carried {
                random_access_indicator: true,
                program_clock_reference: Some(pcr((90_900 - DELAY) * 300)),
                transport_private_data: Some(vec![0x0d; 174]),
                ..plain(VIDEO_PID, true)
            })
        )
    );
}

#[test]
fn a_pes_packet_after_an_adaptation_field_that_leaves_one_byte_of_payload_reads_back_whole() {
    let payload: Vec<u8> = (0..=255).collect();
    let mut muxer = muxer_of(MuxerConfig::new());
    let packet = pes_packet(0xc0, &payload, Some(90_000));
    muxer
        .handle_pes_packet_with_carriage(
            pid(AUDIO_PID),
            &packet,
            &PesPacketCarriage::new().with_transport_private_data(&[0x0e; 180]),
        )
        .unwrap();
    muxer.finish();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    demuxer.subscribe_pes(pid(AUDIO_PID));
    demuxer.handle_input(&out);
    demuxer.finish();
    let mut payloads = Vec::new();
    while let Some(event) = demuxer.poll_event() {
        if let Event::PesPacket { packet, .. } = event {
            payloads.push(packet.payload.to_vec());
        }
    }
    assert_eq!(
        (of_pid(&out, AUDIO_PID).first().cloned(), payloads),
        (
            Some(Carried {
                transport_private_data: Some(vec![0x0e; 180]),
                ..plain(AUDIO_PID, true)
            }),
            vec![payload]
        )
    );
}
