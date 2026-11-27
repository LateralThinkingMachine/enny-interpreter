use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

pub const ORIGIN: &str = "http://127.0.0.1:5180";
pub const HOST: &str = "127.0.0.1:5180";
pub const PUBLIC_ORIGIN: &str = "http://127.0.0.1:5182";
pub const PUBLIC_HOST: &str = "127.0.0.1:5182";

/// One reviewed pair per process. Neither request headers nor an arbitrary
/// browser origin may choose or widen the listener's authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalOrigins {
    operator_port: u16,
}
impl Default for LocalOrigins {
    fn default() -> Self {
        Self {
            operator_port: 5180,
        }
    }
}
impl LocalOrigins {
    pub fn for_port(port: u16) -> Result<Self, &'static str> {
        match port {
            5080 | 5180 => Ok(Self {
                operator_port: port,
            }),
            _ => Err("invalid_operator_port"),
        }
    }
    pub fn host(self, role: Role) -> &'static str {
        match (self.operator_port, role) {
            (5080, Role::Operator) => "127.0.0.1:5080",
            (5080, Role::Public) => "127.0.0.1:5082",
            (_, Role::Operator) => HOST,
            (_, Role::Public) => PUBLIC_HOST,
        }
    }
    pub fn origin(self, role: Role) -> &'static str {
        match (self.operator_port, role) {
            (5080, Role::Operator) => "http://127.0.0.1:5080",
            (5080, Role::Public) => "http://127.0.0.1:5082",
            (_, Role::Operator) => ORIGIN,
            (_, Role::Public) => PUBLIC_ORIGIN,
        }
    }
    pub fn content_security_policy(self, role: Role) -> &'static str {
        match (self.operator_port, role) {
            (5080, Role::Operator) => {
                "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self' ws://127.0.0.1:5080; media-src 'self' blob:; img-src 'self' data:; worker-src 'self' blob:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
            }
            (5080, Role::Public) => {
                "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self' ws://127.0.0.1:5082; media-src 'self' blob:; img-src 'self' data:; worker-src 'self' blob:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
            }
            (_, Role::Operator) => {
                "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self' ws://127.0.0.1:5180; media-src 'self' blob:; img-src 'self' data:; worker-src 'self' blob:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
            }
            (_, Role::Public) => {
                "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self' ws://127.0.0.1:5182; media-src 'self' blob:; img-src 'self' data:; worker-src 'self' blob:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
            }
        }
    }
}

pub fn secret() -> Result<String, &'static str> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "random_unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn hash(text: &[u8]) -> [u8; 32] {
    Sha256::digest(text).into()
}
pub fn hex_hash(bytes: &[u8]) -> String {
    hash(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Operator,
    Public,
}
pub(crate) const EVENTS_PROTOCOL: &str = "pilot.events.v1";

pub struct Ticket {
    digest: [u8; 32],
    deadline: Instant,
}
impl Ticket {
    pub fn new(value: &str, deadline: Instant) -> Self {
        Self {
            digest: hash(value.as_bytes()),
            deadline: deadline.min(Instant::now() + Duration::from_secs(120)),
        }
    }
    pub fn matches(&self, value: &str) -> bool {
        value.len() == 64 && Instant::now() < self.deadline && self.digest == hash(value.as_bytes())
    }
}

pub struct Session {
    pub id: String,
    digest: [u8; 32],
    pub csrf: String,
    pub role: Role,
}
impl Session {
    pub fn new(role: Role) -> Result<(Self, String), &'static str> {
        let token = secret()?;
        Ok((
            Self {
                id: secret()?,
                digest: hash(token.as_bytes()),
                csrf: secret()?,
                role,
            },
            token,
        ))
    }
    pub fn matches(&self, token: Option<&str>) -> bool {
        token.is_some_and(|value| value.len() == 64 && hash(value.as_bytes()) == self.digest)
    }
    pub fn csrf_matches(&self, headers: &HeaderMap) -> bool {
        single(headers, "x-pilot-csrf").is_some_and(|value| {
            value.len() == 64 && hash(value.as_bytes()) == hash(self.csrf.as_bytes())
        })
    }
}

pub fn single<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut all = headers.get_all(name).iter();
    let first = all.next()?.to_str().ok()?;
    if all.next().is_some() {
        None
    } else {
        Some(first)
    }
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    single(headers, "authorization")?.strip_prefix("Bearer ")
}

/// Browser WebSockets cannot set Authorization. Only the event upgrade reads
/// this offer. The handshake selects the fixed public protocol name.
pub(crate) fn event_bearer(headers: &HeaderMap) -> Option<&str> {
    if headers.contains_key("authorization") {
        return None;
    }
    let (name, proof) = single(headers, "sec-websocket-protocol")?.split_once(',')?;
    if name.trim() != EVENTS_PROTOCOL {
        return None;
    }
    proof.trim().strip_prefix("pilot.auth.")
}

#[derive(Clone, Copy, Debug)]
pub enum ApiError {
    Unauthorized,
    Forbidden,
    Invalid,
    Stopped,
    Unavailable,
    Capacity,
    Conflict,
}
impl ApiError {
    pub fn status(self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Invalid => StatusCode::BAD_REQUEST,
            Self::Stopped | Self::Conflict => StatusCode::CONFLICT,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Capacity => StatusCode::TOO_MANY_REQUESTS,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::Invalid => "invalid_request",
            Self::Stopped => "stopped",
            Self::Unavailable => "unavailable",
            Self::Capacity => "capacity",
            Self::Conflict => "conflict",
        }
    }
}
