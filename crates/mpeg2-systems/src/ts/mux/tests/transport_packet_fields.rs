use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::ts::demux::{Demuxer, DemuxerConfig, Event};

/// What a Transport Packet carries of what a [`TransportPacketFields`] sets, and
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

/// A Transport Packet of `pid` that carries none of the fields set
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
/// in turn, each `(pid, dts, fields)`: the result of each, and the output
fn multiplexed(
    inputs: &[(u16, u64, TransportPacketFields<'_>)],
) -> (Vec<Result<(), Error>>, Vec<u8>) {
    let mut muxer = muxer_of(MuxerConfig::new());
    let payload = vec![0xaa; 400];
    let results = inputs
        .iter()
        .map(|(pid_value, dts, fields)| {
            let stream_id = if *pid_value == VIDEO_PID { 0xe0 } else { 0xc0 };
            muxer.handle_pes_packet_with_transport_packet_fields(
                pid(*pid_value),
                &pes_packet(stream_id, &payload, Some(*dts)),
                fields,
            )
        })
        .collect();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    (results, out)
}

/// The output of [`multiplexed`], each PES packet taken
fn output_of(inputs: &[(u16, u64, TransportPacketFields<'_>)]) -> Vec<u8> {
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

/// The `sync_byte` to `adaptation_field_control` of a Transport Packet of
/// `pid`, its `continuity_counter` 0 (§2.4.3.2)
fn header(
    pid_value: u16,
    payload_unit_start_indicator: bool,
    transport_priority: bool,
    adaptation_field_control: u8,
) -> Vec<u8> {
    let [high, low] = pid_value.to_be_bytes();
    vec![
        0x47,
        u8::from(payload_unit_start_indicator) << 6 | u8::from(transport_priority) << 5 | high,
        low,
        adaptation_field_control << 4,
    ]
}

/// `program_clock_reference_base` and `_extension` of a PCR of `base`, its
/// extension 0: 33 bits of base, 6 reserved bits of `'1'`, 9 bits of
/// extension (§2.4.3.4)
fn pcr_field(base: u64) -> Vec<u8> {
    let field = base << 15 | 0x3f << 9;
    field.to_be_bytes().get(2..).unwrap().to_vec()
}

/// The header and the adaptation field of the Transport Packet that starts
/// the last PES packet on `pid` in `out`, its `continuity_counter` cleared
fn first_packet_head(out: &[u8], pid_value: u16) -> Vec<u8> {
    let chunk = out
        .chunks(188)
        .rfind(|chunk| {
            let packet = TransportPacket::read(chunk).unwrap();
            packet.pid.get() == pid_value && packet.payload_unit_start_indicator
        })
        .unwrap();
    let adaptation_field_control = chunk.get(3).unwrap() >> 4;
    let adaptation_field_length = if adaptation_field_control & 0b10 == 0 {
        0
    } else {
        1 + usize::from(*chunk.get(4).unwrap())
    };
    let mut head = chunk.get(..4 + adaptation_field_length).unwrap().to_vec();
    if let Some(byte) = head.get_mut(3) {
        *byte &= 0xf0;
    }
    head
}

#[test]
fn the_fields_set_go_in_the_adaptation_field_of_the_first_transport_packet_only() {
    let private_data = [0x01, 0x02, 0x03];
    let fields = TransportPacketFields::new()
        .with_random_access_indicator(true)
        .with_elementary_stream_priority_indicator(true)
        .with_transport_private_data(&private_data);
    let audio = of_pid(&output_of(&[(AUDIO_PID, 90_000, fields)]), AUDIO_PID);
    assert_eq!(
        audio,
        [
            Carried {
                random_access_indicator: true,
                elementary_stream_priority_indicator: true,
                transport_private_data: Some(private_data.to_vec()),
                ..plain(AUDIO_PID, true)
            },
            plain(AUDIO_PID, false),
            plain(AUDIO_PID, false),
        ]
    );
}
#[test]
fn transport_priority_is_set_on_every_transport_packet_of_the_pes_packet_only() {
    let fields = TransportPacketFields::new().with_transport_priority(true);
    let out = output_of(&[
        (VIDEO_PID, 90_000, TransportPacketFields::new()),
        (AUDIO_PID, 93_600, fields),
    ]);
    let others_with_priority: Vec<Carried> = carried(&out)
        .into_iter()
        .filter(|packet| packet.pid != AUDIO_PID && packet.transport_priority)
        .collect();
    let with_priority = |payload_unit_start_indicator| Carried {
        transport_priority: true,
        ..plain(AUDIO_PID, payload_unit_start_indicator)
    };
    assert_eq!(
        (of_pid(&out, AUDIO_PID), others_with_priority),
        (
            vec![
                with_priority(true),
                with_priority(false),
                with_priority(false)
            ],
            Vec::new()
        )
    );
}

#[test]
fn transport_priority_is_not_set_on_a_pcr_split_from_the_pes_packet_on_its_pid() {
    let private_data: Vec<u8> = (0..180).collect();
    let fields = TransportPacketFields::new()
        .with_transport_priority(true)
        .with_transport_private_data(&private_data);
    let out = output_of(&[
        (VIDEO_PID, 90_000, TransportPacketFields::new()),
        (VIDEO_PID, 93_600, fields),
    ]);
    let video = of_pid(&out, VIDEO_PID);
    let with_priority = |payload_unit_start_indicator| Carried {
        transport_priority: true,
        ..plain(VIDEO_PID, payload_unit_start_indicator)
    };
    // adaptation_field_length 182: the byte of flags with
    // transport_private_data_flag, transport_private_data_length 180, the
    // data (§2.4.3.4)
    let mut head = header(VIDEO_PID, true, true, 0b11);
    head.extend([182, 0x02, 180]);
    head.extend(&private_data);
    assert_eq!(
        (
            video.get(3..).map(<[Carried]>::to_vec),
            first_packet_head(&out, VIDEO_PID)
        ),
        (
            Some(vec![
                Carried {
                    program_clock_reference: Some(pcr((93_600 - DELAY) * 300)),
                    ..plain(VIDEO_PID, false)
                },
                Carried {
                    transport_private_data: Some(private_data.clone()),
                    ..with_priority(true)
                },
                with_priority(false),
                with_priority(false),
                with_priority(false),
            ]),
            head
        )
    );
}
#[test]
fn the_fields_set_go_beside_a_pcr_in_the_same_packet() {
    let private_data = [0x0a; 8];
    let fields = TransportPacketFields::new()
        .with_random_access_indicator(true)
        .with_transport_private_data(&private_data);
    let video = of_pid(&output_of(&[(VIDEO_PID, 90_000, fields)]), VIDEO_PID);
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
    let random_access = TransportPacketFields::new().with_random_access_indicator(true);
    let out = output_of(&[
        (VIDEO_PID, 90_000, TransportPacketFields::new()),
        (VIDEO_PID, 90_900, random_access),
        (VIDEO_PID, 93_600, TransportPacketFields::new()),
        (VIDEO_PID, 94_500, TransportPacketFields::new()),
    ]);
    let at = |dts: u64| (VIDEO_PID, pcr((dts - DELAY) * 300));
    assert_eq!(pcrs_of(&out), [at(90_000), at(90_900), at(94_500)]);
}

#[test]
fn random_access_off_the_pcr_pid_adds_no_pcr() {
    let random_access = TransportPacketFields::new().with_random_access_indicator(true);
    let out = output_of(&[
        (VIDEO_PID, 90_000, TransportPacketFields::new()),
        (AUDIO_PID, 90_900, random_access),
    ]);
    assert_eq!(pcrs_of(&out), [(VIDEO_PID, pcr((90_000 - DELAY) * 300))]);
}

#[test]
fn fields_that_set_nothing_add_no_adaptation_field_where_no_pcr_is_due() {
    let out = output_of(&[
        (VIDEO_PID, 90_000, TransportPacketFields::new()),
        (AUDIO_PID, 90_900, TransportPacketFields::new()),
    ]);
    // adaptation_field_control '01': a payload alone (§2.4.3.2)
    assert_eq!(
        first_packet_head(&out, AUDIO_PID),
        header(AUDIO_PID, true, false, 0b01)
    );
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
        (VIDEO_PID, 90_000, TransportPacketFields::new()),
        (
            pid_value,
            dts,
            TransportPacketFields::new()
                .with_random_access_indicator(random_access_indicator)
                .with_transport_private_data(private_data),
        ),
    ]);
    (*results.last().unwrap(), out)
}

#[test]
fn transport_private_data_past_the_adaptation_field_is_refused_and_leaves_the_muxer_as_it_was() {
    let next = (AUDIO_PID, 93_600, TransportPacketFields::new());
    let (results, after_refusal) = multiplexed(&[
        (VIDEO_PID, 90_000, TransportPacketFields::new()),
        (
            AUDIO_PID,
            90_900,
            TransportPacketFields::new().with_transport_private_data(&[0x0b; 181]),
        ),
        next,
    ]);
    let (_, without_refusal) =
        multiplexed(&[(VIDEO_PID, 90_000, TransportPacketFields::new()), next]);
    let (taken, _) = with_private_data(AUDIO_PID, 90_900, false, &[0x0b; 180]);
    let (taken_off_the_pcr_pid, _) = with_private_data(AUDIO_PID, 90_900, true, &[0x0b; 180]);
    assert_eq!(
        (results, after_refusal, taken, taken_off_the_pcr_pid),
        (
            vec![
                Ok(()),
                Err(Error::TransportPrivateDataLength {
                    length: 181,
                    limit: 180
                }),
                Ok(())
            ],
            without_refusal,
            Ok(()),
            Ok(())
        )
    );
}
#[test]
fn a_pcr_due_with_transport_private_data_too_long_to_share_its_packet_goes_alone_before_it() {
    let private_data = [0x0c; 180];
    let (result, out) = with_private_data(VIDEO_PID, 93_600, false, &private_data);
    let mut head = header(VIDEO_PID, true, false, 0b11);
    head.extend([182, 0x02, 180]);
    head.extend(private_data);
    assert_eq!(
        (
            result,
            of_pid(&out, VIDEO_PID).get(3..5).map(<[Carried]>::to_vec),
            first_packet_head(&out, VIDEO_PID)
        ),
        (
            Ok(()),
            Some(vec![
                Carried {
                    program_clock_reference: Some(pcr((93_600 - DELAY) * 300)),
                    ..plain(VIDEO_PID, false)
                },
                Carried {
                    transport_private_data: Some(private_data.to_vec()),
                    ..plain(VIDEO_PID, true)
                },
            ]),
            head
        )
    );
}
#[test]
fn random_access_on_the_pcr_pid_takes_as_much_transport_private_data_as_fits_beside_the_pcr() {
    let (taken, out) = with_private_data(VIDEO_PID, 90_900, true, &[0x0d; 174]);
    let (refused, _) = with_private_data(VIDEO_PID, 90_900, true, &[0x0d; 175]);
    // adaptation_field_length 182: random_access_indicator, PCR_flag and
    // transport_private_data_flag, the PCR, transport_private_data_length
    // 174, the data (§2.4.3.4)
    let mut head = header(VIDEO_PID, true, false, 0b11);
    head.extend([182, 0x52]);
    head.extend(pcr_field(90_900 - DELAY));
    head.push(174);
    head.extend([0x0d; 174]);
    assert_eq!(
        (taken, refused, first_packet_head(&out, VIDEO_PID)),
        (
            Ok(()),
            Err(Error::TransportPrivateDataLength {
                length: 175,
                limit: 174
            }),
            head
        )
    );
}
#[test]
fn a_pes_packet_after_an_adaptation_field_that_leaves_one_byte_of_payload_reads_back_whole() {
    let payload: Vec<u8> = (0..=255).collect();
    let mut muxer = muxer_of(MuxerConfig::new());
    let packet = pes_packet(0xc0, &payload, Some(90_000));
    muxer
        .handle_pes_packet_with_transport_packet_fields(
            pid(AUDIO_PID),
            &packet,
            &TransportPacketFields::new().with_transport_private_data(&[0x0e; 180]),
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
    let mut head = header(AUDIO_PID, true, false, 0b11);
    head.extend([182, 0x02, 180]);
    head.extend([0x0e; 180]);
    assert_eq!(
        (first_packet_head(&out, AUDIO_PID), payloads),
        (head, vec![payload])
    );
}

#[test]
fn a_pcr_split_from_its_pes_packet_counts_with_the_pes_packet_against_the_buffer_limit() {
    let config = MuxerConfig::new()
        .with_psi_interval(Duration::ZERO)
        .with_pcr_interval(Duration::from_millis(10))
        .with_buffer_limit(600);
    let mut muxer = muxer_of(config);
    let payload = [0xaa; 10];
    muxer
        .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &payload, Some(90_000)))
        .unwrap();
    muxer.poll_output(&mut Vec::new());
    // Why 91 800: two PCRs are then due, the second at the PES packet, where
    // 180 bytes of private data leave it no room
    let refused = muxer.handle_pes_packet_with_transport_packet_fields(
        pid(VIDEO_PID),
        &pes_packet(0xe0, &payload, Some(91_800)),
        &TransportPacketFields::new().with_transport_private_data(&[0x0f; 180]),
    );
    assert_eq!(refused, Err(Error::BufferFull { length: 188 * 6 }));
}

/// The PCRs of an output, each the PID it goes on and its value
type Pcrs = Vec<(u16, ClockReference)>;

/// What the muxer of the default settings makes of `inputs` and the end of
/// input, each `(pid, stream_id, dts, fields)`: the result of each, and the
/// PCRs of the output
fn finished(
    inputs: &[(u16, u8, Option<u64>, TransportPacketFields<'_>)],
) -> (Vec<Result<(), Error>>, Pcrs) {
    let mut muxer = muxer_of(MuxerConfig::new());
    let payload = [0xaa; 100];
    let results = inputs
        .iter()
        .map(|(pid_value, stream_id, dts, fields)| {
            muxer.handle_pes_packet_with_transport_packet_fields(
                pid(*pid_value),
                &pes_packet(*stream_id, &payload, *dts),
                fields,
            )
        })
        .collect();
    muxer.finish();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    (results, pcrs_of(&out))
}

/// The PCR of the time `dts` on the PCR PID, the default `mux_delay` taken off
fn video_pcr(dts: u64) -> (u16, ClockReference) {
    (VIDEO_PID, pcr((dts - DELAY) * 300))
}

#[test]
fn random_access_on_the_pcr_pid_at_the_time_of_the_pcr_before_is_refused() {
    let random_access = TransportPacketFields::new().with_random_access_indicator(true);
    let audio = (AUDIO_PID, 0xc0, Some(90_000), TransportPacketFields::new());
    let (results, pcrs) = finished(&[audio, (VIDEO_PID, 0xe0, Some(90_000), random_access)]);
    let (_, without_the_refused) = finished(&[audio]);
    assert_eq!(
        (results, pcrs.clone(), without_the_refused),
        (
            vec![
                Ok(()),
                Err(Error::RandomAccessIndicator {
                    pid: pid(VIDEO_PID),
                    program_clock_reference: pcr((90_000 - DELAY) * 300),
                })
            ],
            vec![video_pcr(90_000), video_pcr(93_600)],
            pcrs
        )
    );
}

#[test]
fn random_access_on_the_pcr_pid_without_a_time_stamp_after_the_pcr_is_refused() {
    let random_access = TransportPacketFields::new().with_random_access_indicator(true);
    let video = (VIDEO_PID, 0xe0, Some(90_000), TransportPacketFields::new());
    let (results, pcrs) = finished(&[video, (VIDEO_PID, 0xe0, None, random_access)]);
    assert_eq!(
        (results, pcrs),
        (
            vec![
                Ok(()),
                Err(Error::RandomAccessIndicator {
                    pid: pid(VIDEO_PID),
                    program_clock_reference: pcr((90_000 - DELAY) * 300),
                })
            ],
            vec![video_pcr(90_000), video_pcr(93_600)]
        )
    );
}

#[test]
fn random_access_on_the_pcr_pid_given_first_among_pes_packets_of_one_time_is_taken() {
    let random_access = TransportPacketFields::new().with_random_access_indicator(true);
    let (results, pcrs) = finished(&[
        (VIDEO_PID, 0xe0, Some(90_000), random_access),
        (AUDIO_PID, 0xc0, Some(90_000), TransportPacketFields::new()),
    ]);
    assert_eq!(
        (results, pcrs),
        (
            vec![Ok(()), Ok(())],
            vec![video_pcr(90_000), video_pcr(93_600)]
        )
    );
}

#[test]
fn random_access_on_the_pcr_pid_without_a_time_stamp_after_the_time_advanced_is_taken() {
    let random_access = TransportPacketFields::new().with_random_access_indicator(true);
    let (results, pcrs) = finished(&[
        (VIDEO_PID, 0xe0, Some(90_000), TransportPacketFields::new()),
        (AUDIO_PID, 0xc0, Some(90_900), TransportPacketFields::new()),
        (VIDEO_PID, 0xe0, None, random_access),
    ]);
    assert_eq!(
        (results, pcrs),
        (
            vec![Ok(()), Ok(()), Ok(())],
            vec![video_pcr(90_000), video_pcr(90_900)]
        )
    );
}
