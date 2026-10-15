//! Caller-supplied desktop installer URL.
//!
//! A missing URL is an error. This module does not select a host.

use anyhow::Context;

#[cfg(any(target_os = "macos", test))]
pub(crate) fn explicit_installer_url(download_url: Option<String>) -> anyhow::Result<String> {
    download_url.context("Desktop app is not installed")
}

#[cfg(test)]
mod tests {
    use super::explicit_installer_url;

    #[test]
    fn missing_installer_url_does_not_select_oaistatic() {
        let message = explicit_installer_url(None)
            .expect_err("missing installer url")
            .to_string();
        assert!(!message.contains("oaistatic.com"));
        assert!(!message.contains("https://"));
    }

    #[test]
    fn explicit_installer_url_is_unchanged() {
        let url = "https://example.invalid/Codex.dmg";
        assert_eq!(
            explicit_installer_url(Some(url.to_string())).expect("explicit url"),
            url
        );
    }
}
