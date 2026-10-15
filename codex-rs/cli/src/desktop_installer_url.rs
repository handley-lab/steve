//! Caller-supplied desktop installer URL.
//!
//! A missing URL is an error. This module does not select a host.

#[cfg(any(target_os = "macos", test))]
use anyhow::Context;

#[cfg(any(target_os = "macos", test))]
pub(crate) fn explicit_installer_url(download_url: Option<String>) -> anyhow::Result<String> {
    download_url.context("Desktop app is not installed")
}
