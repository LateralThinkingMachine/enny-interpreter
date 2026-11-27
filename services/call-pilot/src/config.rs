mod desktop;

use serde::Deserialize;
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

const BOOTSTRAP_SECONDS: u64 = 120;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub assets_dir: PathBuf,
    pub credentials_path: PathBuf,
    #[serde(deserialize_with = "required_duration")]
    pub duration_seconds: Option<u64>,
    pub max_input_audio_seconds: u64,
    #[serde(default = "default_operator_port")]
    pub operator_port: u16,
    #[serde(default)]
    pub repeatable_sessions: bool,
    /// Explicit provider activation is decoded only by the provider owner.
    /// Never logged or deserialized from a browser request.
    #[serde(default)]
    pub provider: Option<ProviderActivation>,
}

fn required_duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(d)
}

#[cfg(test)]
mod continuous_config_tests {
    use super::*;
    use serde_json::json;
    // Decode through the production config owner. Only forbidden operations
    // are invoked here; they must return before reserving or opening a socket.
    #[tokio::test]
    async fn finite_vendor_owns_routes_identity_and_local_reservations() {
        use crate::{
            budget::{GEMINI_SCHEDULE, ProviderVendor},
            engine::{Interpretation, Language, LiveMode, Route},
        };
        let fixture = json!({"vendor":"gemini", "api_key":"fictional-never-transmitted",
            "allowed_routes":["transcription","live"], "allowed_live_modes":["interpretation"],
            "allow_tts":false, "max_tts_requests":0, "total_duration_seconds":120,
            "route_duration_seconds":60, "max_sessions":2, "max_input_audio_seconds":180,
            "max_output_audio_seconds":180, "max_text_characters":3000, "approved_budget_microusd":200000});
        let decode = |row| serde_json::from_value::<ProviderActivation>(row);
        let backend = decode(fixture.clone())
            .unwrap()
            .into_backend(Some(120))
            .unwrap()
            .backend();
        assert_eq!(backend.vendor(), ProviderVendor::Gemini);
        assert!(backend.supports_live_mode(LiveMode::Interpretation));
        assert!(!backend.supports_live_mode(LiveMode::Assistant));
        assert!(!backend.supports(Route::Translation));
        assert!(!backend.supports_tts());
        let quote = backend.budget_snapshot();
        assert_eq!(quote.pricing_version, GEMINI_SCHEDULE);
        assert_eq!(quote.routes[0].model, "gemini-3.5-transcribe-live");
        assert_eq!(quote.routes[0].quote_microusd, Some(100000));
        assert_eq!(quote.routes[2].model, "gemini-3.5-live-translate-preview");
        assert_eq!(quote.routes[2].quote_microusd, Some(100000));
        assert_eq!(quote.routes[1].quote_microusd, None);
        assert_eq!(quote.tts.max_request_microusd, 0);
        assert!(
            backend
                .synthesize("fixture".into(), Language::En)
                .await
                .is_err()
        );
        assert!(
            backend
                .open_with_mode(Route::Live, Language::Ja, Some(LiveMode::Assistant))
                .await
                .is_err()
        );
        assert!(
            backend
                .open_interpretation(
                    Language::Ja,
                    Interpretation::TwoWay {
                        schema_version: crate::interpretation::InterpretationVersion::V1,
                        first: Language::Ja,
                        second: Language::En
                    }
                )
                .await
                .is_err()
        );
        assert_eq!(
            backend.budget_snapshot().amounts.unwrap().reserved_microusd,
            0
        );

        for (field, value) in [
            ("allow_tts", json!(true)),
            ("max_tts_requests", json!(1)),
            ("allowed_routes", json!(["translation", "live"])),
            ("allowed_live_modes", json!(["assistant"])),
            ("allowed_live_modes", json!([])),
        ] {
            let mut bad = fixture.clone();
            bad[field] = value;
            assert!(decode(bad).unwrap().into_backend(Some(120)).is_err());
        }
        for vendor in [json!("unknown"), json!(null)] {
            let mut bad = fixture.clone();
            bad["vendor"] = vendor;
            assert!(decode(bad).is_err());
        }
        for (vendor, key) in [
            ("openai", "AIza-fictional-fixture"),
            ("openai", "AQ.fictional-fixture"),
            ("gemini", "sk-fictional-fixture"),
        ] {
            let mut bad = fixture.clone();
            bad["vendor"] = json!(vendor);
            bad["api_key"] = json!(key);
            assert!(decode(bad).unwrap().into_backend(Some(120)).is_err());
        }
        assert!(decode(fixture.clone()).unwrap().into_backend(None).is_err());
        let ProviderActivation::Finite(approved) = decode(fixture.clone()).unwrap() else {
            panic!("expected finite activation")
        };
        assert!(approved.into_provider(120).is_err());
        let mut legacy = fixture.clone();
        legacy.as_object_mut().unwrap().remove("vendor");
        for config in [legacy, {
            let mut value = fixture;
            value["vendor"] = json!("openai");
            value
        }] {
            let openai = decode(config)
                .unwrap()
                .into_backend(Some(120))
                .unwrap()
                .backend();
            assert_eq!(
                openai.budget_snapshot().pricing_version,
                crate::budget::PRICING_VERSION
            );
            assert_eq!(
                openai.budget_snapshot().routes[0].quote_microusd,
                Some(17000)
            );
        }
    }

    #[test]
    fn persistent_lifetime_requires_explicit_null_and_matching_provider_shape() {
        let base = json!({"assets_dir":"/private/tmp/assets", "credentials_path":"/private/tmp/credential", "max_input_audio_seconds":1200});
        assert!(serde_json::from_value::<Config>(base.clone()).is_err());
        let mut persistent = base.clone();
        persistent["duration_seconds"] = json!(null);
        assert!(
            serde_json::from_value::<Config>(persistent.clone())
                .unwrap()
                .duration_seconds
                .is_none()
        );
        persistent["duration_seconds"] = json!(900);
        assert_eq!(
            serde_json::from_value::<Config>(persistent)
                .unwrap()
                .duration_seconds,
            Some(900)
        );
        let continuous = json!({"api_key":"fictional-key-never-transmitted", "continuous": {
            "route_duration_seconds":300, "max_concurrent_sessions":2,
            "max_input_audio_seconds":600, "max_output_audio_seconds":600,
            "max_text_characters":76800, "warning_threshold_microusd":2_000_000,
            "allowed_routes":["transcription","live"], "allowed_live_modes":["interpretation"]
        }});
        let provider: ProviderActivation = serde_json::from_value(continuous.clone()).unwrap();
        assert!(matches!(
            provider.into_backend(Some(900)),
            Err("provider_lifetime_mismatch")
        ));
        let provider: ProviderActivation = serde_json::from_value(continuous.clone()).unwrap();
        let backend = provider.into_backend(None).unwrap();
        assert!(backend.backend().budget_snapshot().amounts.is_none());
        let mut mixed = continuous;
        mixed["approved_budget_microusd"] = json!(2_000_000);
        assert!(serde_json::from_value::<ProviderActivation>(mixed).is_err());
        let finite = json!({"api_key":"fictional-key-never-transmitted", "allowed_routes":["transcription","live"],
            "allowed_live_modes":["interpretation"],"allow_tts":false,"total_duration_seconds":900,
            "route_duration_seconds":900,"max_sessions":2,"max_tts_requests":0,
            "max_input_audio_seconds":1800,"max_output_audio_seconds":1800,"max_text_characters":76800,
            "approved_budget_microusd":2_000_000});
        let provider: ProviderActivation = serde_json::from_value(finite).unwrap();
        assert!(matches!(
            provider.into_backend(None),
            Err("provider_lifetime_mismatch")
        ));
    }
}

fn default_operator_port() -> u16 {
    5180
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum ProviderActivation {
    Finite(ApprovedProvider),
    Continuous(ApprovedContinuous),
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedContinuous {
    pub(crate) api_key: String,
    pub(crate) continuous: crate::continuous::ContinuousPolicy,
}
#[derive(Clone)]
pub enum ActivatedBackend {
    Finite(std::sync::Arc<crate::provider::FiniteBackend>),
    Continuous(std::sync::Arc<crate::continuous::ContinuousBackend>),
}
impl ActivatedBackend {
    pub fn backend(&self) -> std::sync::Arc<dyn crate::engine::ProviderBackend> {
        match self {
            Self::Finite(b) => b.clone(),
            Self::Continuous(b) => b.clone(),
        }
    }
    pub(crate) async fn retire(&self) -> bool {
        match self {
            Self::Finite(b) => b.retire().await,
            Self::Continuous(b) => b.retire().await,
        }
    }
    pub(crate) fn closure_confirmed(&self) -> bool {
        match self {
            Self::Finite(b) => b.closure_confirmed(),
            Self::Continuous(b) => b.closure_confirmed(),
        }
    }
    pub async fn shutdown(&self) {
        match self {
            Self::Finite(b) => b.shutdown().await,
            Self::Continuous(b) => b.shutdown().await,
        }
    }
}
impl ProviderActivation {
    pub fn into_backend(self, duration: Option<u64>) -> Result<ActivatedBackend, &'static str> {
        self.into_backend_with_allocations(
            duration,
            std::sync::Arc::new(crate::continuous::ProcessAllocations::default()),
        )
    }
    pub(crate) fn into_backend_with_allocations(
        self,
        duration: Option<u64>,
        allocations: std::sync::Arc<crate::continuous::ProcessAllocations>,
    ) -> Result<ActivatedBackend, &'static str> {
        match (self, duration) {
            (Self::Finite(p), Some(seconds)) => {
                let vendor = p.vendor;
                let desktop_meeting = p.desktop_meeting;
                let config = p.into_config(seconds)?;
                let backend = if desktop_meeting {
                    crate::provider::FiniteBackend::for_desktop_meeting(config)
                } else {
                    crate::provider::FiniteBackend::for_vendor(config, vendor)
                };
                backend
                    .map(|b| ActivatedBackend::Finite(std::sync::Arc::new(b)))
                    .map_err(|_| "provider_configuration")
            }
            (Self::Continuous(p), None) => {
                crate::continuous::ContinuousBackend::new_with_allocations(
                    p.api_key,
                    p.continuous,
                    allocations,
                )
                .map(|b| ActivatedBackend::Continuous(std::sync::Arc::new(b)))
                .map_err(|_| "provider_configuration")
            }
            _ => Err("provider_lifetime_mismatch"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedProvider {
    // Set only by the desktop frame decoder after validating both operator choices.
    #[serde(skip)]
    desktop_meeting: bool,
    #[serde(default)]
    vendor: crate::budget::ProviderVendor,
    api_key: String,
    allowed_routes: Vec<crate::engine::Route>,
    #[serde(default, deserialize_with = "explicit_live_modes")]
    allowed_live_modes: Option<Vec<crate::engine::LiveMode>>,
    allow_tts: bool,
    total_duration_seconds: u64,
    route_duration_seconds: u64,
    max_sessions: u32,
    max_tts_requests: u32,
    max_input_audio_seconds: u64,
    max_output_audio_seconds: u64,
    max_text_characters: u64,
    approved_budget_microusd: u64,
}
fn explicit_live_modes<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<crate::engine::LiveMode>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::deserialize(deserializer).map(Some)
}
impl ApprovedProvider {
    /// Legacy OpenAI-only conversion. A Gemini credential must never be passed
    /// to an existing caller which constructs the default OpenAI transport.
    pub fn into_provider(
        self,
        duration: u64,
    ) -> Result<crate::provider::ProviderConfig, &'static str> {
        if self.vendor != crate::budget::ProviderVendor::OpenAi {
            return Err("provider_vendor_mismatch");
        }
        self.into_config(duration)
    }

    fn into_config(self, duration: u64) -> Result<crate::provider::ProviderConfig, &'static str> {
        if self.total_duration_seconds != duration {
            return Err("provider_duration_mismatch");
        }
        let allowed_live_modes = self.allowed_live_modes.unwrap_or_else(|| {
            if self.allowed_routes.contains(&crate::engine::Route::Live) {
                vec![crate::engine::LiveMode::Assistant]
            } else {
                vec![]
            }
        });
        Ok(crate::provider::ProviderConfig {
            api_key: self.api_key,
            allowed_routes: self.allowed_routes,
            allowed_live_modes,
            allow_tts: self.allow_tts,
            total_duration_seconds: self.total_duration_seconds,
            route_duration_seconds: self.route_duration_seconds,
            max_sessions: self.max_sessions,
            max_tts_requests: self.max_tts_requests,
            max_input_audio_seconds: self.max_input_audio_seconds,
            max_output_audio_seconds: self.max_output_audio_seconds,
            max_text_characters: self.max_text_characters,
            approved_budget_microusd: self.approved_budget_microusd,
        })
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, &'static str> {
        let bytes = read_file(path, 16 * 1024, true)?;
        Self::decode(&bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > 16 * 1024 {
            return Err("config_bound");
        }
        let config: Self = serde_json::from_slice(bytes).map_err(|_| "invalid_configuration")?;
        config.validate()
    }

    fn validate(self) -> Result<Self, &'static str> {
        self.origins()?;
        if self.repeatable_sessions
            && (self.duration_seconds.is_some()
                || matches!(self.provider, Some(ProviderActivation::Finite(_))))
        {
            return Err("repeatable_scope_mismatch");
        }
        if self
            .duration_seconds
            .is_some_and(|seconds| !(1..=7200).contains(&seconds))
            || !(1..=14400).contains(&self.max_input_audio_seconds)
        {
            return Err("invalid_limits");
        }
        for path in [&self.assets_dir, &self.credentials_path] {
            validate_path(path)?;
        }
        let assets = fs::symlink_metadata(&self.assets_dir).map_err(|_| "assets_unavailable")?;
        if !assets.is_dir() || assets.file_type().is_symlink() {
            return Err("assets_unavailable");
        }
        Ok(self)
    }
    pub fn origins(&self) -> Result<crate::auth::LocalOrigins, &'static str> {
        crate::auth::LocalOrigins::for_port(self.operator_port)
    }
    pub fn write_credentials(&self, ticket: &str) -> Result<(), &'static str> {
        use std::io::Write;
        let value = serde_json::json!({"origin":self.origins()?.origin(crate::auth::Role::Operator),"bootstrap":ticket,"expires_in_seconds":BOOTSTRAP_SECONDS});
        let bytes = serde_json::to_vec(&value).map_err(|_| "credential_output")?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.credentials_path)
            .map_err(|_| "credential_output")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "credential_output")
    }
}

fn validate_path(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("invalid_path");
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if let Ok(info) = fs::symlink_metadata(&current)
            && info.file_type().is_symlink()
        {
            return Err("symlink_path");
        }
    }
    Ok(())
}

pub fn read_file(path: &Path, limit: usize, private: bool) -> Result<Vec<u8>, &'static str> {
    validate_path(path)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "file_unavailable")?;
    let info = file.metadata().map_err(|_| "file_unavailable")?;
    // A newly created zero-byte file establishes the actual effective file
    // owner without trusting UID/HOME/environment or inspecting account files.
    let uid = if private {
        Some(owned_uid(path.parent().ok_or("invalid_path")?)?)
    } else {
        None
    };
    if !info.is_file()
        || info.len() > limit as u64
        || (private && (info.permissions().mode() & 0o777 != 0o600 || uid != Some(info.uid())))
    {
        return Err("unsafe_file");
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "file_unavailable")?;
    if bytes.len() > limit {
        return Err("file_bound");
    }
    Ok(bytes)
}

fn owned_uid(parent: &Path) -> Result<u32, &'static str> {
    let probe = parent.join(format!(".pilot-owner-{}", crate::auth::secret()?));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&probe)
        .map_err(|_| "owner_unavailable")?;
    let uid = file
        .metadata()
        .map(|m| m.uid())
        .map_err(|_| "owner_unavailable");
    drop(file);
    fs::remove_file(probe).map_err(|_| "owner_cleanup")?;
    uid
}
