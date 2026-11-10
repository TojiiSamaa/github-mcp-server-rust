//! Erreurs d'outil. `code()` est stable : les agents et les tests s'appuient dessus.

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub enum GhError {
    /// `compte inconnu: X (connus: a, b)`
    UnknownAccount {
        name: String,
        known: Vec<String>,
    },
    Config(String),
    InvalidArgs(String),
    /// Réponse HTTP non 2xx de GitHub.
    Http {
        status: u16,
        message: String,
    },
    GraphQl(Value),
    Network(String),
    /// Mode test : tentative de sortie autre que l'API GitHub simulée.
    TestModeEgressBlocked(String),
    /// Outil à validation sans moyen de demander à un humain.
    ConfirmationRequired(String),
    /// Validation refusée par l'humain (ou l'approbateur simulé).
    Denied(String),
    Timeout(String),
    Other(String),
}

pub type Result<T> = std::result::Result<T, GhError>;

impl GhError {
    pub fn code(&self) -> &'static str {
        match self {
            GhError::UnknownAccount { .. } => "UNKNOWN_ACCOUNT",
            GhError::Config(_) => "CONFIG",
            GhError::InvalidArgs(_) => "INVALID_ARGS",
            GhError::Http { .. } => "HTTP",
            GhError::GraphQl(_) => "GRAPHQL",
            GhError::Network(_) => "NETWORK",
            GhError::TestModeEgressBlocked(_) => "TEST_MODE_EGRESS_BLOCKED",
            GhError::ConfirmationRequired(_) => "CONFIRMATION_REQUIRED",
            GhError::Denied(_) => "DENIED",
            GhError::Timeout(_) => "TIMEOUT",
            GhError::Other(_) => "ERROR",
        }
    }
    pub fn to_json(&self) -> Value {
        let mut v =
            json!({ "error": self.code(), "message": crate::redact::redact(&self.to_string()) });
        if let GhError::Http { status, .. } = self {
            v["status"] = json!(status);
        }
        v
    }
}

impl std::fmt::Display for GhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GhError::UnknownAccount { name, known } => {
                write!(f, "compte inconnu: {name} (connus: {})", known.join(", "))
            }
            GhError::Http { status, message } => write!(f, "GitHub HTTP {status}: {message}"),
            GhError::GraphQl(v) => write!(f, "GraphQL: {v}"),
            GhError::Config(m)
            | GhError::InvalidArgs(m)
            | GhError::Network(m)
            | GhError::TestModeEgressBlocked(m)
            | GhError::ConfirmationRequired(m)
            | GhError::Denied(m)
            | GhError::Timeout(m)
            | GhError::Other(m) => {
                write!(f, "{}: {m}", self.code())
            }
        }
    }
}

impl std::error::Error for GhError {}
