//! Verifies that concurrent rejected requests share one external-owner refresh.

use super::auth_provider_from_auth_manager;
use codex_login::AuthHeaders;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::ExternalAuth;
use codex_login::RefreshTokenError;
use codex_login::auth::RefreshTokenFailedError;
use codex_login::auth::RefreshTokenFailedReason;
use http::HeaderMap;
use http::HeaderValue;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Poll;
use tokio::sync::Barrier;

struct RotatingExternalAuth {
    refreshes: AtomicUsize,
    barrier: Barrier,
}

impl ExternalAuth for RotatingExternalAuth {
    fn resolve(&self) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async {
            let token = if self.refreshes.load(Ordering::SeqCst) == 0 {
                "Bearer initial"
            } else {
                "Bearer replacement"
            };
            Ok(CodexAuth::Headers(AuthHeaders::new(HeaderMap::from_iter(
                [(http::header::AUTHORIZATION, HeaderValue::from_static(token))],
            ))))
        })
    }

    fn refresh(
        &self,
        _context: codex_login::ExternalAuthRefreshContext,
    ) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async {
            if self.refreshes.fetch_add(1, Ordering::SeqCst) != 0 {
                return Err(std::io::Error::other("redundant refresh rejected"));
            }
            self.barrier.wait().await;
            self.resolve().await
        })
    }

    fn classify_error(&self, error: std::io::Error) -> RefreshTokenError {
        RefreshTokenError::Permanent(RefreshTokenFailedError::new(
            RefreshTokenFailedReason::Other,
            error.to_string(),
        ))
    }
}

#[tokio::test]
async fn concurrent_external_rejections_share_one_refresh() {
    let external = Arc::new(RotatingExternalAuth {
        refreshes: AtomicUsize::new(0),
        barrier: Barrier::new(2),
    });
    let initial = external.resolve().await.unwrap();
    let manager = AuthManager::from_auth_for_testing(initial.clone());
    manager.set_external_auth(external.clone()).await.unwrap();
    let provider = auth_provider_from_auth_manager(manager, &initial);
    let rejected = provider.to_auth_headers().unwrap();
    let mut first = provider.refresh_after_unauthorized(rejected.clone());
    let mut second = provider.refresh_after_unauthorized(rejected.clone());
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    external.barrier.wait().await;
    assert_eq!(tokio::join!(first, second), (true, true));
    assert!(provider.refresh_after_unauthorized(rejected).await);
    assert_eq!(external.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider.apply_auth_headers(HeaderMap::new()).await.unwrap()[http::header::AUTHORIZATION],
        "Bearer replacement"
    );
}
