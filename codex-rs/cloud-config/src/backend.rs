use codex_backend_client::Client as BackendClient;
use codex_backend_client::ConfigBundleResponse;
use codex_backend_client::DeliveredTomlFragment;
use codex_config::CloudConfigBundle;
use codex_config::CloudConfigFragment;
use codex_config::CloudConfigTomlBundle;
use codex_config::CloudRequirementsFragment;
use codex_config::CloudRequirementsTomlBundle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetryableFailureKind {
    Request { status_code: Option<u16> },
    AuthRecovery { status_code: Option<u16> },
}

impl RetryableFailureKind {
    pub(crate) fn status_code(self) -> Option<u16> {
        match self {
            Self::Request { status_code } | Self::AuthRecovery { status_code } => status_code,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BundleRequestError {
    Policy(codex_http_client::NetworkPolicyDenied),
    Fatal {
        error: codex_config::CloudConfigBundleLoadError,
        metric_reason: Option<&'static str>,
    },
    Retryable(RetryableFailureKind),
    Unauthorized {
        status_code: Option<u16>,
        message: String,
    },
}

pub(crate) async fn fetch_bundle(
    client: &BackendClient,
) -> Result<CloudConfigBundle, BundleRequestError> {
    client
        .get_config_bundle()
        .await
        .map(bundle_from_response)
        .map_err(bundle_request_error)
}

fn bundle_request_error(error: codex_backend_client::RequestError) -> BundleRequestError {
    if let codex_backend_client::RequestError::Policy(denied) = &error {
        return BundleRequestError::Policy(*denied);
    }
    let status_code = error.status().map(|status| status.as_u16());
    if error.is_unauthorized() {
        BundleRequestError::Unauthorized {
            status_code,
            message: "Managed configuration sign-in is required".into(),
        }
    } else {
        BundleRequestError::Retryable(RetryableFailureKind::Request { status_code })
    }
}

pub(crate) fn bundle_from_response(response: ConfigBundleResponse) -> CloudConfigBundle {
    let config_toml = response
        .config_toml
        .flatten()
        .map(|config_toml| *config_toml)
        .and_then(|config_toml| config_toml.enterprise_managed.flatten())
        .unwrap_or_default()
        .into_iter()
        .map(config_fragment_from_delivered)
        .collect();
    let requirements_toml = response
        .requirements_toml
        .flatten()
        .map(|requirements_toml| *requirements_toml)
        .and_then(|requirements_toml| requirements_toml.enterprise_managed.flatten())
        .unwrap_or_default()
        .into_iter()
        .map(requirements_fragment_from_delivered)
        .collect();

    CloudConfigBundle {
        config_toml: CloudConfigTomlBundle {
            enterprise_managed: config_toml,
        },
        requirements_toml: CloudRequirementsTomlBundle {
            enterprise_managed: requirements_toml,
        },
    }
}

fn config_fragment_from_delivered(fragment: DeliveredTomlFragment) -> CloudConfigFragment {
    CloudConfigFragment {
        id: fragment.id,
        name: fragment.name,
        contents: fragment.contents,
    }
}

fn requirements_fragment_from_delivered(
    fragment: DeliveredTomlFragment,
) -> CloudRequirementsFragment {
    CloudRequirementsFragment {
        id: fragment.id,
        name: fragment.name,
        contents: fragment.contents,
    }
}
