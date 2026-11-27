//! A Gemini translation session and a dedicated ASR session have different
//! output authority. In particular, generated audio can never create a source row.
use super::*;
use crate::engine::{Provenance, TranscriptItem};

fn source_locale(language: Language) -> Result<&'static str, ProviderError> {
    match language {
        Language::En => Ok("en-US"),
        Language::Ja => Ok("ja-JP"),
        Language::Hi => Ok("hi-IN"),
        Language::Es | Language::Vi => Err(ProviderError::Unavailable),
    }
}

pub(super) struct Protocol {
    pub route: Route,
    pub setup: Value,
    pub ready: bool,
    pub frames: u32,
    finals: u32,
    // An interim Google ASR result can replace earlier text. The operator's
    // SourceDraft format only permits prefix growth, so retire it on replacement.
    draft: Option<(String, String, u32)>,
    drafts: u32,
    messages: u32,
}

impl Protocol {
    pub(super) fn new(
        route: Route,
        language: Language,
        mode: Option<LiveMode>,
        hints: Option<SpeechHints>,
        policy: Option<Interpretation>,
        context: Option<InterpretationContext>,
    ) -> Result<Self, ProviderError> {
        if policy.is_some() || context.is_some() || !language.legacy() {
            return Err(ProviderError::Unavailable);
        }
        let (model, configuration) = match (route, mode) {
            (Route::Transcription, None) => {
                let hints = hints.unwrap_or_default();
                hints.validate()?;
                if !hints.keywords.is_empty() {
                    return Err(ProviderError::Unavailable);
                }
                let locales = hints
                    .languages
                    .into_iter()
                    .map(source_locale)
                    .collect::<Result<Vec<_>, _>>()?;
                (
                    crate::budget::GEMINI_ASR_MODEL,
                    json!({
                        "generationConfig":{"responseModalities":["TEXT"]},
                        "inputAudioTranscription":{"mode":"VERBATIM","languageCodes":locales}
                    }),
                )
            }
            (Route::Live, Some(LiveMode::Interpretation)) if hints.is_none() => (
                crate::budget::GEMINI_LIVE_MODEL,
                json!({
                    "generationConfig":{"responseModalities":["AUDIO"],
                        "translationConfig":{"targetLanguageCode":super::super::protocol::lang(language),"echoTargetLanguage":true}},
                    "outputAudioTranscription":{}
                }),
            ),
            _ => return Err(ProviderError::Unavailable),
        };
        let mut setup = configuration;
        setup["model"] = Value::String(format!("models/{model}"));
        Ok(Self {
            route,
            setup: json!({"setup":setup}),
            ready: false,
            frames: 0,
            finals: 0,
            draft: None,
            drafts: 0,
            messages: 0,
        })
    }

    fn text<'a>(shared: &Shared, value: &'a Value) -> Result<&'a str, ProviderError> {
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .ok_or(ProviderError::Protocol)?;
        if text.len() > 16_384 || text.chars().count() > 4096 || text.contains('\0') {
            return Err(ProviderError::Protocol);
        }
        shared.text(text)?;
        Ok(text)
    }

    fn retire_draft(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some((item_id, _, revision)) = self.draft.take() {
            out.push(StreamEvent::SourceDraft {
                item_id,
                revision,
                text: String::new(),
                retired: true,
            });
        }
    }

    fn interim(&mut self, text: &str, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if let Some((id, old, revision)) = &mut self.draft {
            if text == old {
                return Ok(());
            }
            if text.starts_with(old.as_str()) && *revision < 4096 {
                *revision += 1;
                *old = text.into();
                out.push(StreamEvent::SourceDraft {
                    item_id: id.clone(),
                    revision: *revision,
                    text: text.into(),
                    retired: false,
                });
                return Ok(());
            }
        }
        self.retire_draft(out);
        if !text.is_empty() {
            self.drafts += 1;
            if self.drafts > 4096 {
                return Err(ProviderError::TranscriptItemLimit);
            }
            let id = format!("gemini-draft-{}", self.drafts);
            self.draft = Some((id.clone(), text.into(), 1));
            out.push(StreamEvent::SourceDraft {
                item_id: id,
                revision: 1,
                text: text.into(),
                retired: false,
            });
        }
        Ok(())
    }

    pub(super) fn consume(
        &mut self,
        shared: &Shared,
        value: Value,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        self.messages += 1;
        if self.messages > 64 + self.frames * 12 {
            return Err(ProviderError::ProtocolCapacityLimit);
        }
        let object = value.as_object().ok_or(ProviderError::Protocol)?;
        // The session is opened with no tools, resumption or other capabilities.
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "setupComplete" | "serverContent" | "usageMetadata" | "goAway"
            )
        }) {
            return Err(ProviderError::Protocol);
        }
        if object.contains_key("goAway") {
            return Err(ProviderError::ConnectionLost);
        }
        if object.get("usageMetadata").is_some_and(|v| !v.is_object()) {
            return Err(ProviderError::Protocol);
        }
        if !self.ready {
            if !object.get("setupComplete").is_some_and(Value::is_object)
                || object.contains_key("serverContent")
            {
                return Err(ProviderError::Protocol);
            }
            self.ready = true;
            return Ok(Vec::new());
        }
        if object.contains_key("setupComplete") {
            return Err(ProviderError::Protocol);
        }
        let Some(content) = object.get("serverContent") else {
            return if object.contains_key("usageMetadata") {
                Ok(Vec::new())
            } else {
                Err(ProviderError::Protocol)
            };
        };
        if !content.is_object() || self.frames == 0 {
            return Err(ProviderError::Protocol);
        }
        let mut out = Vec::new();
        if self.route == Route::Transcription {
            if content.get("modelTurn").is_some() || content.get("outputTranscription").is_some() {
                return Err(ProviderError::Protocol);
            }
            if let Some(value) = content.get("interimInputTranscription") {
                self.interim(Self::text(shared, value)?, &mut out)?;
            }
            // The dedicated Live Transcribe API defines inputTranscription as
            // finalized chunks; interimInputTranscription is explicitly mutable.
            if let Some(finalized) = content.get("inputTranscription") {
                let text = Self::text(shared, finalized)?;
                self.retire_draft(&mut out);
                if !text.is_empty() {
                    self.finals += 1;
                    if self.finals > 4096 {
                        return Err(ProviderError::TranscriptItemLimit);
                    }
                    out.push(StreamEvent::Transcript {
                        provenance: Provenance::SourceAsr,
                        text: text.into(),
                        final_fragment: true,
                        item: Some(TranscriptItem {
                            id: format!("gemini-final-{}", self.finals),
                            sequence: self.finals,
                            revision: 1,
                            source_audio: None,
                        }),
                    });
                }
            }
        } else {
            if content.get("inputTranscription").is_some()
                || content.get("interimInputTranscription").is_some()
            {
                return Err(ProviderError::Protocol);
            }
            if content.get("interrupted") == Some(&Value::Bool(true)) {
                // This bounded profile cannot retract frames already queued for
                // playback. Retire the route and let the client discard its queue.
                return Err(ProviderError::InputInterrupted);
            }
            if let Some(value) = content.get("outputTranscription") {
                let text = Self::text(shared, value)?;
                if !text.is_empty() {
                    out.push(StreamEvent::Transcript {
                        provenance: Provenance::LiveOutput,
                        text: text.into(),
                        final_fragment: false,
                        item: None,
                    });
                }
            }
            if let Some(turn) = content.get("modelTurn") {
                let parts = turn
                    .get("parts")
                    .and_then(Value::as_array)
                    .filter(|p| !p.is_empty() && p.len() <= 16)
                    .ok_or(ProviderError::Protocol)?;
                for part in parts {
                    let audio = part.get("inlineData").ok_or(ProviderError::Protocol)?;
                    let encoded = audio
                        .get("data")
                        .and_then(Value::as_str)
                        .ok_or(ProviderError::Protocol)?;
                    if audio["mimeType"] != "audio/pcm;rate=24000" || encoded.len() > 64_000 {
                        return Err(ProviderError::Protocol);
                    }
                    let pcm16 = STANDARD
                        .decode(encoded)
                        .map_err(|_| ProviderError::Protocol)?;
                    if pcm16.is_empty() || !pcm16.len().is_multiple_of(2) || pcm16.len() > 48_000 {
                        return Err(ProviderError::Protocol);
                    }
                    shared.output(pcm16.len(), 24_000)?;
                    out.push(StreamEvent::Audio {
                        pcm16,
                        sample_rate: 24_000,
                    });
                }
            }
        }
        Ok(out)
    }
}
