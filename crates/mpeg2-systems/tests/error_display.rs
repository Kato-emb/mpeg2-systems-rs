//! Errors that wrap another, whose `Display` leaves the wrapped error to
//! `source`

#[cfg(test)]
mod tests {
    use core::error::Error;

    use mpeg2_systems::psi::{cat, pat, pmt, section};
    use mpeg2_systems::ts::{adaptation_field, packet};
    use mpeg2_systems::{field, pes};

    #[test]
    fn an_error_that_wraps_another_does_not_repeat_its_message() {
        let cause = field::Error::Truncated {
            required_bits: 13,
            remaining_bits: 5,
        };
        let section = section::Error::Field {
            field: "section_length",
            error: cause,
        };
        let errors: [&dyn Error; 10] = [
            &packet::Error::AdaptationField(adaptation_field::Error::Field {
                field: "PCR",
                error: cause,
            }),
            &packet::Error::Field {
                field: "PID",
                error: cause,
            },
            &adaptation_field::Error::Field {
                field: "PCR",
                error: cause,
            },
            &pes::Error::Field {
                field: "PTS",
                error: cause,
            },
            &section,
            &pat::Error::Field {
                field: "program_map_PID",
                error: cause,
            },
            &pat::Error::Section(section),
            &pmt::Error::Field {
                field: "ES_info_length",
                error: cause,
            },
            &pmt::Error::Section(section),
            &cat::Error::Section(section),
        ];

        assert_eq!(
            errors.map(|error| {
                let source = error.source().map(ToString::to_string);
                (
                    source.is_some(),
                    source.is_some_and(|source| error.to_string().contains(&source)),
                )
            }),
            [(true, false); 10]
        );
    }
}
