//! The packaged launchers send only the two choices the listener makes.
//! Legacy framed configurations (including key-free rehearsal) keep their existing schema.
use super::{ApprovedProvider, Config, ProviderActivation, default_operator_port};
use crate::{
    budget::{ProviderVendor, stream_cost},
    engine::{LiveMode, Route},
};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopMeeting {
    assets_dir: PathBuf,
    credentials_path: PathBuf,
    #[serde(default = "default_operator_port")]
    operator_port: u16,
    meeting_minutes: u64,
    budget_cents: u64,
    api_key: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DesktopFrame {
    Meeting(DesktopMeeting),
    Existing(Config),
}

impl Config {
    /// Only the paid packaged meeting frame opts in to the launcher exit protocol.
    pub fn paid_desktop_meeting(&self) -> bool {
        matches!(&self.provider, Some(ProviderActivation::Finite(p)) if p.desktop_meeting)
    }

    pub fn decode_desktop(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > 16 * 1024 {
            return Err("config_bound");
        }
        match serde_json::from_slice(bytes).map_err(|_| "invalid_configuration")? {
            DesktopFrame::Meeting(meeting) => meeting.into_config()?.validate(),
            DesktopFrame::Existing(config) => config.validate(),
        }
    }
}

impl DesktopMeeting {
    fn into_config(self) -> Result<Config, &'static str> {
        if !(1..=60).contains(&self.meeting_minutes) || !(1..=100_000).contains(&self.budget_cents)
        {
            return Err("invalid_meeting_limits");
        }
        let seconds = self.meeting_minutes * 60;
        let approved = self.budget_cents * 10_000;
        let routes = [Route::Transcription, Route::Live];
        let required = routes.iter().try_fold(0u64, |sum, &route| {
            sum.checked_add(stream_cost(route, seconds)?)
        });
        if required.is_none_or(|minimum| approved < minimum) {
            return Err("meeting_budget_too_low");
        }

        // Give the listener the bootstrap window to sign in and choose a tab.
        // That extra wall time is never part of either paid route quote.
        let lifetime = seconds + super::BOOTSTRAP_SECONDS;
        let provider = ApprovedProvider {
            desktop_meeting: true,
            vendor: ProviderVendor::OpenAi,
            api_key: self.api_key,
            allowed_routes: routes.to_vec(),
            allowed_live_modes: Some(vec![LiveMode::Interpretation]),
            allow_tts: false,
            total_duration_seconds: lifetime,
            route_duration_seconds: seconds,
            max_sessions: 2,
            max_tts_requests: 0,
            max_input_audio_seconds: seconds * 4,
            max_output_audio_seconds: seconds * 2,
            // Same text headroom as the finite activation profile: two admissions,
            // two text representations and 64 Unicode scalars per second each.
            max_text_characters: (seconds * 2 * 2 * 64).min(1_000_000),
            approved_budget_microusd: approved,
        };
        Ok(Config {
            assets_dir: self.assets_dir,
            credentials_path: self.credentials_path,
            operator_port: self.operator_port,
            duration_seconds: Some(lifetime),
            max_input_audio_seconds: seconds * 2,
            repeatable_sessions: false,
            provider: Some(ProviderActivation::Finite(provider)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn request(minutes: u64, cents: u64) -> Value {
        let assets = std::env::current_dir().unwrap();
        json!({
            "assets_dir": assets, "credentials_path": assets.join("unused-test-credentials.json"),
            "api_key": "fictional-inert-credential", "operator_port": 5180,
            "meeting_minutes": minutes, "budget_cents": cents,
        })
    }

    #[test]
    fn selected_limits_reach_the_finite_provider_and_its_operator_quote() {
        for (minutes, cents, captions, spoken) in [
            (1, 7, 17_000, 50_000),
            (17, 114, 289_000, 850_000),
            (60, 402, 1_020_000, 3_000_000),
            (60, 100_000, 1_020_000, 3_000_000),
        ] {
            let seconds = minutes * 60;
            let frame = request(minutes, cents).to_string();
            assert!(Config::decode(frame.as_bytes()).is_err());
            let config = Config::decode_desktop(frame.as_bytes()).unwrap();
            assert_eq!(config.duration_seconds, Some(seconds + 120));
            assert_eq!(config.max_input_audio_seconds, seconds * 2);
            assert!(!config.repeatable_sessions);
            let backend = config
                .provider
                .unwrap()
                .into_backend(config.duration_seconds)
                .unwrap()
                .backend();
            let quote = backend.budget_snapshot();
            assert_eq!(quote.route_duration_seconds, Some(seconds));
            assert_eq!(quote.remaining_seconds, Some(seconds));
            assert_eq!(quote.routes[0].quote_microusd, Some(captions));
            assert_eq!(quote.routes[2].quote_microusd, Some(spoken));
            assert!(quote.routes[0].can_reserve && quote.routes[2].can_reserve);
            assert!(!quote.routes[1].configured && !quote.tts.configured);
            assert!(backend.supports_live_mode(LiveMode::Interpretation));
            assert!(!backend.supports_live_mode(LiveMode::Assistant));
            let amounts = quote.amounts.unwrap();
            assert_eq!(amounts.approved_microusd, cents * 10_000);
            assert_eq!(amounts.reserved_microusd, 0);
            assert_eq!(quote.stream_reservations.unwrap().limit, 2);
        }
    }

    #[test]
    fn rejects_invalid_choices_and_mixed_approval_frames() {
        for (minutes, cents) in [
            (0, 500),
            (61, 500),
            (1, 0),
            (1, 6),
            (17, 113),
            (60, 401),
            (60, 100_001),
        ] {
            assert!(
                Config::decode_desktop(request(minutes, cents).to_string().as_bytes()).is_err()
            );
        }
        for (name, value) in [
            ("budget_cents", json!(402.1)),
            ("meeting_minutes", json!("60")),
            ("provider", Value::Null),
            ("allowed_routes", json!(["live"])),
            ("repeatable_sessions", json!(true)),
            ("duration_seconds", json!(600)),
        ] {
            let mut row = request(60, 402);
            row[name] = value;
            assert!(Config::decode_desktop(row.to_string().as_bytes()).is_err());
        }
    }
}
