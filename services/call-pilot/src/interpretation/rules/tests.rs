use super::*;
use crate::interpretation::{Interpretation, InterpretationVersion};
use serde_json::{Value, json};

fn rule(id: u32, source: &str, target: &str, text: bool, speech: bool) -> Value {
    json!({"id":format!("rule-{id}"),"source":source,"target":target,"text":text,"speech":speech})
}
fn policy(rules: Vec<Value>) -> Interpretation {
    serde_json::from_value(
        json!({"mode":"rules","schema_version":"interpretation-rules.v1","rules":rules}),
    )
    .unwrap()
}
fn rules(policy: &Interpretation) -> &InterpretationRules {
    let Interpretation::Rules { rules, .. } = policy else {
        panic!("rules policy")
    };
    rules
}

#[test]
fn exact_owner_matrix_preserves_text_and_speech_independently() {
    let value = policy(vec![
        rule(1, "ja", "en", true, true),
        rule(2, "hi", "en", true, true),
        rule(3, "hi", "ja", true, false),
        rule(4, "en", "ja", true, true),
        rule(5, "en", "hi", true, false),
    ]);
    assert_eq!(value.anchor(), Language::En);
    value.validate(Language::En).unwrap();
    assert!(value.validate(Language::Ja).is_err());
    assert!(value.has_text() && value.has_speech());
    let routes = rules(&value);
    assert_eq!(routes.iter().count(), 5);
    for (source, target, speech) in [
        (Language::Ja, Language::En, true),
        (Language::Hi, Language::En, true),
        (Language::Hi, Language::Ja, false),
        (Language::En, Language::Ja, true),
        (Language::En, Language::Hi, false),
    ] {
        let actual = routes.effective(source, target).unwrap();
        assert!(actual.text);
        assert_eq!(actual.speech, speech);
    }
    assert!(routes.effective(Language::Ja, Language::Hi).is_none());
    assert!(routes.effective(Language::En, Language::En).is_none());
    let instructions = value.instructions();
    assert!(instructions.contains("Japanese: English."));
    assert!(instructions.contains("Hindi: English."));
    assert!(instructions.contains("English: Japanese."));
    assert!(!instructions.contains("English: Japanese, Hindi"));
    assert!(!instructions.contains("Hindi: Japanese"));
    assert_eq!(
        serde_json::from_value::<Interpretation>(serde_json::to_value(value).unwrap()).unwrap(),
        value
    );
    fn require_copy<T: Copy>(_: T) {}
    require_copy(value);
}

#[test]
fn explicit_off_overrides_wildcard_in_either_order_without_reviving_audio() {
    for reversed in [false, true] {
        let mut entries = vec![
            rule(1, "auto", "en", true, true),
            rule(2, "ja", "en", false, false),
            rule(3, "hi", "en", true, false),
        ];
        if reversed {
            entries.reverse();
        }
        let value = policy(entries);
        let routes = rules(&value);
        let disabled = routes.effective(Language::Ja, Language::En).unwrap();
        assert!(!disabled.text && !disabled.speech);
        let text_only = routes.effective(Language::Hi, Language::En).unwrap();
        assert!(text_only.text && !text_only.speech);
        assert!(routes.effective(Language::Es, Language::En).unwrap().speech);
        assert!(routes.effective(Language::En, Language::En).is_none());
        let compiled = routes.speech_instructions();
        assert!(!compiled.contains("Japanese:"));
        assert!(!compiled.contains("Hindi:"));
        assert!(compiled.contains("Spanish: English."));
    }
    let all_overridden = policy(vec![
        rule(1, "auto", "en", true, true),
        rule(2, "ja", "en", false, false),
        rule(3, "hi", "en", false, false),
        rule(4, "es", "en", false, false),
        rule(5, "vi", "en", false, false),
    ]);
    assert!(!all_overridden.has_speech() && !all_overridden.has_text());
}

#[test]
fn multiple_speech_targets_follow_rule_order_and_text_only_is_absent() {
    let value = policy(vec![
        rule(1, "en", "vi", true, true),
        rule(2, "auto", "es", false, true),
        rule(3, "en", "hi", true, false),
        rule(4, "en", "ja", false, true),
    ]);
    let compiled = rules(&value).speech_instructions();
    assert!(compiled.contains("English: Vietnamese, Spanish, Japanese."));
    assert!(!compiled.contains("English: Vietnamese, Spanish, Hindi"));
    assert!(compiled.contains("one target after another in that order"));
    let text_only = policy(vec![rule(1, "en", "ja", true, false)]);
    assert!(!text_only.has_speech() && text_only.has_text());
    assert!(
        !text_only
            .instructions()
            .contains(crate::interpretation::JAPANESE_SPEECH_STYLE)
    );
    assert!(!text_only.instructions().contains("Japanese"));
    let empty = policy(vec![]);
    assert_eq!(empty.anchor(), Language::Ja);
    assert!(!empty.has_speech() && !empty.has_text());
    assert!(
        empty
            .instructions()
            .contains("No spoken routes are enabled")
    );
}

#[test]
fn closed_schema_rejects_duplicates_unknown_fields_languages_and_unbounded_input() {
    let valid = rule(1, "ja", "en", true, true);
    for entries in [
        vec![valid.clone(), valid.clone()],
        vec![valid.clone(), rule(2, "ja", "en", false, false)],
        vec![valid.clone(), rule(1, "hi", "en", false, false)],
        vec![rule(1, "en", "en", true, true)],
        vec![rule(1, "unknown", "en", true, true)],
        vec![rule(1, "ja", "auto", true, true)],
        vec![valid.clone(); 31],
        vec![json!(["rule-1", "ja", "en", true, true])],
    ] {
        assert!(serde_json::from_value::<InterpretationRules>(json!(entries)).is_err());
    }
    for id in [
        "rule-0",
        "rule-01",
        "rule-1000000",
        "rule-+1",
        "rule-1 ",
        "command",
        "rule-١",
    ] {
        let mut invalid = valid.clone();
        invalid["id"] = json!(id);
        assert!(serde_json::from_value::<InterpretationRules>(json!([invalid])).is_err());
    }
    for key in ["id", "source", "target", "text", "speech"] {
        let mut invalid = valid.clone();
        invalid.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<InterpretationRules>(json!([invalid])).is_err());
    }
    let mut extra = valid.clone();
    extra["authority"] = json!(true);
    assert!(serde_json::from_value::<InterpretationRules>(json!([extra])).is_err());
    assert!(serde_json::from_str::<InterpretationRules>(r#"[{"id":"rule-1","source":"ja","target":"en","text":true,"text":false,"speech":true}]"#).is_err());
    for invalid in [
        json!({"mode":"rules","schema_version":"interpretation.v1","rules":[valid.clone()]}),
        json!({"mode":"rules","schema_version":"interpretation-rules.v1","rules":[valid.clone()],"target":"en"}),
        json!({"mode":"two_way","schema_version":"interpretation-rules.v1","first":"ja","second":"en"}),
    ] {
        assert!(serde_json::from_value::<Interpretation>(invalid).is_err());
    }
}

#[test]
fn maximum_effective_matrix_remains_bounded_and_legacy_keeps_its_scope() {
    let mut entries = Vec::new();
    let languages = ["ja", "en", "hi", "es", "vi"];
    for source in ["auto", "ja", "en", "hi", "es", "vi"] {
        for target in languages {
            if source != target {
                entries.push(rule(entries.len() as u32 + 1, source, target, true, true));
            }
        }
    }
    let value = policy(entries);
    value.validate(value.anchor()).unwrap();
    assert_eq!(rules(&value).iter().count(), 25);
    assert!(value.instructions().len() <= 8192);
    let legacy = Interpretation::TwoWay {
        schema_version: InterpretationVersion::V1,
        first: Language::Ja,
        second: Language::En,
    };
    assert!(legacy.has_speech());
    assert!(!legacy.has_text());
}
