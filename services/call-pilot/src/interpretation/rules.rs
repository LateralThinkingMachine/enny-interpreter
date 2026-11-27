//! Closed routing configuration; this does not establish observed source language.
use crate::engine::{Language, ProviderError};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeSeq};
use std::fmt;

pub const INTERPRETATION_LANGUAGES: [Language; 5] = [
    Language::Ja,
    Language::En,
    Language::Hi,
    Language::Es,
    Language::Vi,
];
const MAX_RULES: usize = 30;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum InterpretationRulesVersion {
    #[serde(rename = "interpretation-rules.v1")]
    V1,
}

/// Numeric storage keeps the policy Copy without accepting arbitrary prompt text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleId(u32);
impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rule-{}", self.0)
    }
}
impl Serialize for RuleId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
impl<'de> Deserialize<'de> for RuleId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor;
        impl de::Visitor<'_> for IdVisitor {
            type Value = RuleId;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("rule- followed by one to six decimal digits without leading zero")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<RuleId, E> {
                let digits = value
                    .strip_prefix("rule-")
                    .filter(|v| {
                        (1..=6).contains(&v.len())
                            && !v.starts_with('0')
                            && v.bytes().all(|b| b.is_ascii_digit())
                    })
                    .ok_or_else(|| E::custom("invalid rule identity"))?;
                digits.parse().map(RuleId).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(IdVisitor)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleSource {
    Ja,
    En,
    Hi,
    Es,
    Vi,
    Auto,
}
impl RuleSource {
    pub fn language(self) -> Option<Language> {
        match self {
            Self::Ja => Some(Language::Ja),
            Self::En => Some(Language::En),
            Self::Hi => Some(Language::Hi),
            Self::Es => Some(Language::Es),
            Self::Vi => Some(Language::Vi),
            Self::Auto => None,
        }
    }
}
impl From<Language> for RuleSource {
    fn from(value: Language) -> Self {
        match value {
            Language::Ja => Self::Ja,
            Language::En => Self::En,
            Language::Hi => Self::Hi,
            Language::Es => Self::Es,
            Language::Vi => Self::Vi,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InterpretationRule {
    pub id: RuleId,
    pub source: RuleSource,
    pub target: Language,
    pub text: bool,
    pub speech: bool,
}
impl<'de> Deserialize<'de> for InterpretationRule {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            id: RuleId,
            source: RuleSource,
            target: Language,
            text: bool,
            speech: bool,
        }
        struct RuleVisitor;
        impl<'de> de::Visitor<'de> for RuleVisitor {
            type Value = InterpretationRule;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a translation rule object")
            }
            fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let rule = Fields::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(InterpretationRule {
                    id: rule.id,
                    source: rule.source,
                    target: rule.target,
                    text: rule.text,
                    speech: rule.speech,
                })
            }
        }
        deserializer.deserialize_map(RuleVisitor)
    }
}

/// Fixed capacity bounds admission and keeps immutable route metadata Copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterpretationRules {
    entries: [Option<InterpretationRule>; MAX_RULES],
    len: u8,
}
impl Default for InterpretationRules {
    fn default() -> Self {
        Self {
            entries: [None; MAX_RULES],
            len: 0,
        }
    }
}
impl InterpretationRules {
    pub fn new(rules: impl IntoIterator<Item = InterpretationRule>) -> Result<Self, ProviderError> {
        let mut result = Self::default();
        for rule in rules {
            result.push(rule)?;
        }
        Ok(result)
    }
    fn push(&mut self, rule: InterpretationRule) -> Result<(), ProviderError> {
        if usize::from(self.len) == MAX_RULES
            || rule.source.language() == Some(rule.target)
            || self.iter().any(|prior| {
                prior.id == rule.id || (prior.source == rule.source && prior.target == rule.target)
            })
        {
            return Err(ProviderError::Protocol);
        }
        self.entries[usize::from(self.len)] = Some(rule);
        self.len += 1;
        Ok(())
    }
    pub fn iter(&self) -> impl Iterator<Item = &InterpretationRule> {
        self.entries[..usize::from(self.len)].iter().flatten()
    }
    pub fn validate(&self) -> Result<(), ProviderError> {
        Self::new(self.iter().copied()).map(|_| ())
    }
    pub fn effective(&self, source: Language, target: Language) -> Option<&InterpretationRule> {
        if source == target {
            return None;
        }
        self.iter()
            .find(|rule| rule.source.language() == Some(source) && rule.target == target)
            .or_else(|| {
                self.iter()
                    .find(|rule| rule.source == RuleSource::Auto && rule.target == target)
            })
    }
    pub fn has_speech(&self) -> bool {
        INTERPRETATION_LANGUAGES
            .into_iter()
            .any(|target| self.has_speech_target(target))
    }
    pub fn has_text(&self) -> bool {
        INTERPRETATION_LANGUAGES.into_iter().any(|source| {
            INTERPRETATION_LANGUAGES
                .into_iter()
                .any(|target| self.effective(source, target).is_some_and(|rule| rule.text))
        })
    }
    pub(super) fn has_speech_target(&self, target: Language) -> bool {
        INTERPRETATION_LANGUAGES.into_iter().any(|source| {
            self.effective(source, target)
                .is_some_and(|rule| rule.speech)
        })
    }
    pub(super) fn speech_instructions(&self) -> String {
        let mut direction = String::from(
            "Detect each meaningful source segment within the supported language catalog. The following are the only spoken routes (source: ordered targets). Render each segment once per listed target, one target after another in that order; never mix voices or announce language names. Remain silent for an unlisted, same-language, unclear or unsupported direction. Never translate generated output as input.\n",
        );
        for source in INTERPRETATION_LANGUAGES {
            // Iterating saved rules, rather than target enum order, preserves
            // the moderator's explicit serialization order after overrides.
            let targets: Vec<&str> = self
                .iter()
                .filter(|rule| {
                    rule.speech
                        && self
                            .effective(source, rule.target)
                            .is_some_and(|effective| effective.id == rule.id)
                })
                .map(|rule| rule.target.name())
                .collect();
            if !targets.is_empty() {
                direction.push_str(source.name());
                direction.push_str(": ");
                direction.push_str(&targets.join(", "));
                direction.push_str(".\n");
            }
        }
        if !self.has_speech() {
            direction.push_str("No spoken routes are enabled. Remain silent.\n");
        }
        direction
    }
}
impl Serialize for InterpretationRules {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(usize::from(self.len)))?;
        for rule in self.iter() {
            seq.serialize_element(rule)?;
        }
        seq.end()
    }
}
impl<'de> Deserialize<'de> for InterpretationRules {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RulesVisitor;
        impl<'de> de::Visitor<'de> for RulesVisitor {
            type Value = InterpretationRules;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("at most 30 unique translation rules")
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                if seq.size_hint().is_some_and(|len| len > MAX_RULES) {
                    return Err(de::Error::custom("too many translation rules"));
                }
                let mut result = InterpretationRules::default();
                while let Some(rule) = seq.next_element::<InterpretationRule>()? {
                    result
                        .push(rule)
                        .map_err(|_| de::Error::custom("invalid translation rules"))?;
                }
                Ok(result)
            }
        }
        deserializer.deserialize_seq(RulesVisitor)
    }
}

#[cfg(test)]
mod tests;
