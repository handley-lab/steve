//! Preserves explicitly configured backend endpoints across credential changes.

use codex_login::CodexAuth;

/// A backend endpoint that retains whether configuration supplied an override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatgptBaseUrl {
    Configured(String),
}

impl ChatgptBaseUrl {
    /// Resolve with the same captured credentials that will authenticate the request.
    pub fn resolve(&self, _auth: &CodexAuth) -> &str {
        match self {
            Self::Configured(url) => url,
        }
    }
}

impl From<String> for ChatgptBaseUrl {
    fn from(url: String) -> Self {
        Self::Configured(url)
    }
}

impl From<&str> for ChatgptBaseUrl {
    fn from(url: &str) -> Self {
        Self::Configured(url.to_owned())
    }
}
