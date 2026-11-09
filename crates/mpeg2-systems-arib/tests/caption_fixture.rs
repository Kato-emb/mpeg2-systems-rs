//! The ARIB caption fixture read down to its data units, against the values
//! `cargo xtask fixtures --arib` laid out from ARIB STD-B24 and TR-B14

#[cfg(test)]
#[cfg(feature = "alloc")]
#[path = "support/caption.rs"]
mod support;

#[cfg(test)]
#[cfg(feature = "alloc")]
mod tests {
    use mpeg2_systems::descriptor::{DescriptorLoop, Error as DescriptorError};
    use mpeg2_systems::pes::{PesPacket, StreamId};
    use mpeg2_systems::stream_type::StreamType;
    use mpeg2_systems_arib::caption::Dmf;
    use mpeg2_systems_arib::caption::Tmd;
    use mpeg2_systems_arib::caption::data_group::{DataGroup, DataGroupId, DataGroupVersion};
    use mpeg2_systems_arib::caption::data_unit::{DataUnit, DataUnitLoop, DataUnitParameter};
    use mpeg2_systems_arib::caption::management::{
        CaptionManagementData, Format, Language, LanguageLoop, LanguageTag, RollupMode, Tcs,
    };
    use mpeg2_systems_arib::caption::statement::CaptionData;
    use mpeg2_systems_arib::descriptor::data_component::{
        AdditionalAribCaptionInfo, DataComponentDescriptor, DataComponentId, Timing,
    };
    use mpeg2_systems_arib::descriptor::stream_identifier::StreamIdentifierDescriptor;
    use mpeg2_systems_arib::independent_pes::SynchronizedPesData;

    use super::support;

    /// The language of the caption management data, TR-B14 Fascicle 2
    /// Volume 3 Part 1 Table 4-4: `language_tag` 0, reserved `'1'`, DMF
    /// `'1010'`, `jpn`, Format `'1000'`, TCS `'00'`, `rollup_mode` `'00'`
    const LANGUAGE: [u8; 5] = [0x1a, 0x6a, 0x70, 0x6e, 0x80];

    /// The caption management data, ARIB STD-B24 Volume 1 Part 3 Table 9-3:
    /// TMD `'00'`, `num_languages` 1, [`LANGUAGE`], no data unit
    const MANAGEMENT: [u8; 10] = [0x3f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00, 0x00, 0x00];

    /// The data units of the statements, Table 9-11: one statement body of
    /// あ and of い of the Kanji set (STD-B24 Volume 1 Part 2 Table 7-4, row
    /// 4 cells 2 and 4)
    const UNITS: [[u8; 7]; 2] = [
        [0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x22],
        [0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x24],
    ];

    /// The caption statement data, Table 9-10: TMD `'00'`,
    /// `data_unit_loop_length` 7, and the data units of [`UNITS`]
    const STATEMENTS: [[u8; 11]; 2] = [
        [
            0x3f, 0x00, 0x00, 0x07, 0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x22,
        ],
        [
            0x3f, 0x00, 0x00, 0x07, 0x1f, 0x20, 0x00, 0x00, 0x02, 0x24, 0x24,
        ],
    ];

    fn group<'data>(id: u8, version: u8, data: &'data [u8]) -> DataGroup<'data> {
        let mut group = DataGroup::new(DataGroupId::try_from(id).unwrap(), data);
        group.data_group_version = DataGroupVersion::try_from(version).unwrap();
        group
    }

    #[test]
    fn the_pmt_names_one_caption_stream_with_the_descriptors_tr_b14_gives_it() {
        let (streams, _) = support::read();
        let [stream]: &[_; 1] = streams.as_slice().try_into().unwrap();
        let descriptors: Vec<_> = DescriptorLoop::from(stream.es_info.as_slice())
            .iter()
            .map(Result::unwrap)
            .collect();
        let [stream_identifier, data_component]: &[_; 2] =
            descriptors.as_slice().try_into().unwrap();
        let data_component = DataComponentDescriptor::try_from(data_component).unwrap();
        let mut expected_data_component =
            DataComponentDescriptor::new(DataComponentId::CAPTION_AND_SUPERIMPOSE);
        expected_data_component.additional_data_component_info = &[0x3d];

        assert_eq!(
            (
                stream.pid,
                stream.stream_type,
                StreamIdentifierDescriptor::try_from(stream_identifier),
                data_component,
                AdditionalAribCaptionInfo::read(data_component.additional_data_component_info),
            ),
            (
                0x0130,
                StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                Ok::<_, DescriptorError>(StreamIdentifierDescriptor::new(0x30)),
                expected_data_component,
                Ok(AdditionalAribCaptionInfo::new(
                    Dmf::try_from(0b0011).unwrap(),
                    Timing::PROGRAM_SYNCHRONOUS
                )),
            )
        );
    }

    #[test]
    fn the_three_pes_packets_carry_the_management_and_the_two_statements() {
        let (_, packets) = support::read();
        let read: Vec<_> = packets
            .iter()
            .map(|packet| {
                let pes = PesPacket::new(packet.stream_id, &packet.payload);
                let data = SynchronizedPesData::try_from(&pes).unwrap();
                (
                    packet.pts,
                    packet.stream_id,
                    DataGroup::read(data.synchronized_pes_data_byte).unwrap(),
                )
            })
            .collect();

        assert_eq!(
            read,
            [
                (
                    Some(90_000),
                    StreamId::PRIVATE_STREAM_1,
                    group(0x00, 0, &MANAGEMENT)
                ),
                (
                    Some(180_000),
                    StreamId::PRIVATE_STREAM_1,
                    group(0x01, 0, &STATEMENTS[0])
                ),
                (
                    Some(270_000),
                    StreamId::PRIVATE_STREAM_1,
                    group(0x01, 1, &STATEMENTS[1])
                ),
            ]
        );
    }

    #[test]
    fn the_data_groups_of_the_fixture_are_read_down_to_their_languages_and_data_units() {
        let management = CaptionManagementData::try_from(&group(0x00, 0, &MANAGEMENT)).unwrap();
        let [first, second] = &STATEMENTS;
        let statements = [group(0x01, 0, first), group(0x01, 1, second)]
            .map(|group| CaptionData::try_from(&group).unwrap());
        let mut languages = management.languages.iter();

        assert_eq!(
            (management, [languages.next(), languages.next()]),
            (
                CaptionManagementData::new(
                    Tmd::FREE,
                    LanguageLoop::from(&LANGUAGE[..]),
                    DataUnitLoop::from(&[][..])
                ),
                [
                    Some(Ok(Language::new(
                        LanguageTag::try_from(0).unwrap(),
                        Dmf::try_from(0b1010).unwrap(),
                        *b"jpn",
                        Format::HORIZONTAL_960_X_540,
                        Tcs::EIGHT_BIT_CODE,
                        RollupMode::NON_ROLL_UP,
                    ))),
                    None
                ]
            )
        );
        assert_eq!(
            statements
                .map(|statement| (statement, statement.data_units.iter().collect::<Vec<_>>())),
            [
                (
                    CaptionData::new(Tmd::FREE, DataUnitLoop::from(&UNITS[0][..])),
                    vec![Ok(DataUnit::new(
                        DataUnitParameter::STATEMENT_BODY,
                        &[0x24, 0x22]
                    ))]
                ),
                (
                    CaptionData::new(Tmd::FREE, DataUnitLoop::from(&UNITS[1][..])),
                    vec![Ok(DataUnit::new(
                        DataUnitParameter::STATEMENT_BODY,
                        &[0x24, 0x24]
                    ))]
                ),
            ]
        );
    }

    #[test]
    fn each_pes_packet_is_written_back_as_the_fixture_carries_it() {
        let (_, packets) = support::read();
        let written: Vec<_> = packets
            .iter()
            .map(|packet| {
                let pes = PesPacket::new(packet.stream_id, &packet.payload);
                let data = SynchronizedPesData::try_from(&pes).unwrap();
                let group = DataGroup::read(data.synchronized_pes_data_byte).unwrap();
                let mut content = Vec::new();
                if group.data_group_id.get() == 0x00 {
                    let management = CaptionManagementData::try_from(&group).unwrap();
                    management.write(&mut content).unwrap();
                } else {
                    CaptionData::try_from(&group)
                        .unwrap()
                        .write(&mut content)
                        .unwrap();
                }
                let mut group_bytes = Vec::new();
                let mut rewritten = group;
                rewritten.data_group_data_byte = &content;
                rewritten.write(&mut group_bytes).unwrap();
                let mut payload = Vec::new();
                SynchronizedPesData::new(&group_bytes)
                    .write(&mut payload)
                    .unwrap();
                payload
            })
            .collect();

        assert_eq!(
            written,
            packets
                .iter()
                .map(|packet| packet.payload.clone())
                .collect::<Vec<_>>()
        );
    }
}
