use super::*;
use crate::history_cell::HistoryCell;
use codex_protocol::openai_models::ModelAccessPrograms;
use pretty_assertions::assert_eq;

#[test]
fn turn_program_uses_catalog_and_preserves_legacy_models() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![
            CyberAccessProgram::Standard,
            CyberAccessProgram::DaybreakBlue,
        ],
    });
    let models = vec![model.clone()];
    for (name, enabled, expected) in [
        (
            model.model.as_str(),
            true,
            Some(CyberAccessProgram::DaybreakBlue),
        ),
        (
            model.model.as_str(),
            false,
            Some(CyberAccessProgram::Standard),
        ),
        ("unlisted-model", false, None),
        ("unlisted-model", true, None),
    ] {
        assert_eq!(
            program_for_turn(&models, name, /*eligible_account*/ true, enabled),
            Ok(expected)
        );
    }
    assert!(
        program_for_turn(
            &models,
            &model.model,
            /*eligible_account*/ false,
            /*enabled*/ true
        )
        .is_err()
    );
    for (programs, rejects) in [
        (None, false),
        (Some(vec![]), false),
        (Some(vec![CyberAccessProgram::Standard]), true),
    ] {
        model.available_access_programs = programs.map(|cyber| ModelAccessPrograms { cyber });
        let result = program_for_turn(
            std::slice::from_ref(&model),
            &model.model,
            /*eligible_account*/ true,
            /*enabled*/ true,
        );
        if rejects {
            assert!(result.is_err());
        } else {
            assert_eq!(result, Ok(None));
        }
    }
}

#[test]
fn refusal_guidance_uses_the_model_catalog() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::Standard],
    });
    for (name, expected) in [
        ("gpt-5.6-sol", Notice::Unavailable),
        ("gpt-6-astra", Notice::Unavailable),
        ("gpt-6-astra-wm", Notice::Unavailable),
    ] {
        model.model = name.into();
        assert_eq!(
            notice_for_setting(
                std::slice::from_ref(&model),
                name,
                /*enabled*/ false,
                /*can_enable_daybreak*/ true,
            ),
            expected
        );
    }
    model
        .available_access_programs
        .as_mut()
        .unwrap()
        .cyber
        .push(CyberAccessProgram::DaybreakBlue);
    assert_eq!(
        notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            /*enabled*/ false,
            /*can_enable_daybreak*/ true,
        ),
        Notice::Disabled
    );
    assert_eq!(
        notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            /*enabled*/ true,
            /*can_enable_daybreak*/ true,
        ),
        Notice::Enabled
    );
    model.available_access_programs.as_mut().unwrap().cyber =
        vec![CyberAccessProgram::DaybreakBlue];
    assert_eq!(
        notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            /*enabled*/ false,
            /*can_enable_daybreak*/ true
        ),
        Notice::Disabled
    );
    let other = model.clone();
    model.model = "gpt-5.6-sol".into();
    model.available_access_programs.as_mut().unwrap().cyber = vec![CyberAccessProgram::Standard];
    assert_eq!(
        notice_for_setting(
            &[model.clone(), other],
            &model.model,
            /*enabled*/ false,
            /*can_enable_daybreak*/ true
        ),
        Notice::Unavailable
    );
    assert_eq!(
        notice_for_setting(
            &[],
            "gpt-6-astra",
            /*enabled*/ false,
            /*can_enable_daybreak*/ true
        ),
        Notice::Undetermined
    );
}

#[test]
fn provider_neutral_refusal_describes_standard_without_toggle_eligibility() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.model = "provider-model".into();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::Standard],
    });
    for can_enable in [false, true] {
        let notice = notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            false,
            can_enable,
        );
        let cell = crate::history_cell::new_cyber_policy_error_event(notice);
        let text = cell
            .display_lines(120)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("does not advertise a Daybreak program"),
            "{text}"
        );
    }
}

#[test]
fn provider_neutral_refusal_describes_advertised_program_without_toggle() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::DaybreakBlue],
    });
    let notice = notice_for_setting(std::slice::from_ref(&model), &model.model, false, false);
    let cell = crate::history_cell::new_cyber_policy_error_event(notice);
    let text = cell
        .display_lines(120)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("advertises a Daybreak program"), "{text}");
    assert!(!text.contains("turn it on"), "{text}");
}

#[test]
fn provider_neutral_refusal_uses_metadata_for_every_model_name() {
    let mut model = crate::test_support::TEST_MODEL_PRESETS[0].clone();
    model.model = "provider-model".into();
    model.available_access_programs = Some(ModelAccessPrograms {
        cyber: vec![CyberAccessProgram::Standard],
    });
    for can_enable in [true, false] {
        let expected = notice_for_setting(
            std::slice::from_ref(&model),
            &model.model,
            false,
            can_enable,
        );
        for name in ["gpt-6-astra", "gpt-6-astra-wm", "another-model"] {
            model.model = name.into();
            assert_eq!(
                notice_for_setting(std::slice::from_ref(&model), name, false, can_enable),
                expected
            );
        }
    }
}
