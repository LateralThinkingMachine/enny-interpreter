//! Read-only local reservation metadata. This is not an invoice or a permit.
use crate::engine::Route;
use serde::{Deserialize, Serialize};

pub const PRICING_VERSION: &str = "pilot_audio_2026_09_13_v1";
pub(crate) const LIVE_MODEL: &str = "gpt-live-1";
pub(crate) const ASR_MODEL: &str = "gpt-live-transcribe";
pub(crate) const TRANSLATION_MODEL: &str = "gpt-realtime-translate";
pub(crate) const TTS_MODEL: &str = "tts-1-hd";
pub(crate) const MAX_TTS_TEXT_BYTES: usize = 2000;
pub(crate) const TTS_MICROUSD_PER_INPUT_BYTE: u64 = 30;

pub(crate) const GEMINI_LIVE_MODEL: &str = "gemini-3.5-live-translate-preview";
pub(crate) const GEMINI_ASR_MODEL: &str = "gemini-3.5-transcribe-live";
pub const GEMINI_SCHEDULE: &str = "gemini_preview_reserve_2026_11_27_v1";

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderVendor {
    #[default]
    OpenAi,
    Gemini,
}

impl ProviderVendor {
    fn rate(self, route: Route) -> u64 {
        match (self, route) {
            (Self::OpenAi, route) => rate(route),
            (Self::Gemini, Route::Translation) => 0,
            (Self::Gemini, _) => 100_000,
        }
    }

    pub(crate) fn stream_cost(self, route: Route, seconds: u64) -> Option<u64> {
        let rate = self.rate(route);
        if rate == 0 {
            return None;
        }
        seconds.checked_mul(rate).map(|total| total.div_ceil(60))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetState {
    Disabled,
    Active,
    Finishing,
    Stopped,
    Expired,
    Uncertain,
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
pub struct Amounts {
    pub approved_microusd: u64,
    pub reserved_microusd: u64,
    pub unallocated_microusd: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Reservations {
    pub used: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct RouteQuote {
    pub route: Route,
    pub model: &'static str,
    pub per_minute_microusd: u64,
    pub configured: bool,
    pub quote_seconds: Option<u64>,
    pub quote_microusd: Option<u64>,
    pub can_reserve: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TtsQuote {
    pub model: &'static str,
    pub configured: bool,
    pub per_input_utf8_byte_microusd: u64,
    pub max_input_utf8_bytes: u64,
    pub max_request_microusd: u64,
    pub can_reserve_maximum: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContinuousSpending {
    pub warning_threshold_microusd: u64,
    pub allocated_microusd: u64,
    pub windows_started: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BudgetSnapshot {
    pub version: u8,
    pub generation: u64,
    pub state: BudgetState,
    pub currency: &'static str,
    pub pricing_version: &'static str,
    pub uncertain: Option<bool>,
    pub amounts: Option<Amounts>,
    pub remaining_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuous: Option<ContinuousSpending>,
    pub route_duration_seconds: Option<u64>,
    pub stream_reservations: Option<Reservations>,
    pub tts_reservations: Option<Reservations>,
    pub routes: [RouteQuote; 3],
    pub tts: TtsQuote,
    pub invoice_verified: bool,
    pub account_hard_cap_proven: bool,
}

impl BudgetSnapshot {
    /// A static price preview carries no configured scope or invented balance.
    pub fn inactive(state: BudgetState) -> Self {
        Self::inactive_for(ProviderVendor::OpenAi, state)
    }
    pub(crate) fn inactive_for(vendor: ProviderVendor, state: BudgetState) -> Self {
        let gemini = vendor == ProviderVendor::Gemini;
        Self {
            version: 1,
            generation: 0,
            state,
            currency: "USD",
            pricing_version: if gemini {
                GEMINI_SCHEDULE
            } else {
                PRICING_VERSION
            },
            uncertain: None,
            amounts: None,
            remaining_seconds: Some(0),
            continuous: None,
            route_duration_seconds: None,
            stream_reservations: None,
            tts_reservations: None,
            routes: [Route::Transcription, Route::Translation, Route::Live].map(|route| {
                RouteQuote {
                    route,
                    model: match (vendor, route) {
                        (ProviderVendor::OpenAi, route) => model(route),
                        (ProviderVendor::Gemini, Route::Live) => GEMINI_LIVE_MODEL,
                        (ProviderVendor::Gemini, Route::Transcription) => GEMINI_ASR_MODEL,
                        (ProviderVendor::Gemini, Route::Translation) => "unavailable",
                    },
                    per_minute_microusd: vendor.rate(route),
                    configured: false,
                    quote_seconds: None,
                    quote_microusd: None,
                    can_reserve: false,
                }
            }),
            tts: TtsQuote {
                model: if gemini { "unavailable" } else { TTS_MODEL },
                configured: false,
                per_input_utf8_byte_microusd: if gemini {
                    0
                } else {
                    TTS_MICROUSD_PER_INPUT_BYTE
                },
                max_input_utf8_bytes: if gemini { 0 } else { MAX_TTS_TEXT_BYTES as u64 },
                max_request_microusd: if gemini {
                    0
                } else {
                    MAX_TTS_TEXT_BYTES as u64 * TTS_MICROUSD_PER_INPUT_BYTE
                },
                can_reserve_maximum: false,
            },
            invoice_verified: false,
            account_hard_cap_proven: false,
        }
    }

    pub fn block(&mut self, state: BudgetState) {
        self.state = state;
        for quote in &mut self.routes {
            quote.can_reserve = false;
        }
        self.tts.can_reserve_maximum = false;
    }
}

pub fn rate(route: Route) -> u64 {
    match route {
        Route::Transcription => 17000,
        Route::Translation => 34000,
        Route::Live => 50000,
    }
}

fn model(route: Route) -> &'static str {
    match route {
        Route::Transcription => ASR_MODEL,
        Route::Translation => TRANSLATION_MODEL,
        Route::Live => LIVE_MODEL,
    }
}

/// Whole micro-USD rounding only; never round a partial minute to a whole minute.
pub fn stream_cost(route: Route, seconds: u64) -> Option<u64> {
    ProviderVendor::OpenAi.stream_cost(route, seconds)
}
