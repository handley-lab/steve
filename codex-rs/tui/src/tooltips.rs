use crate::keymap::RuntimeKeymap;
use crate::keymap::keymap_action_id;
use crate::terminal_hyperlinks::HyperlinkLine;
use codex_config::types::TuiKeymap;
use codex_features::FEATURES;
use codex_features::Feature;
use codex_features::FeatureSpec;
use codex_protocol::account::PlanType;
use lazy_static::lazy_static;
use rand::Rng;
use rand::seq::IteratorRandom;
use std::path::Path;

#[cfg(test)]
#[path = "tooltips/keybinding_tests.rs"]
mod keybinding_tests;

const IS_MACOS: bool = cfg!(target_os = "macos");
const IS_WINDOWS: bool = cfg!(target_os = "windows");

const WINDOWS_APP_TOOLTIP: &str = "Use the **desktop app**. Install it from https://chatgpt.com/codex?app-landing-page=true and run `steve app`.";
const MACOS_APP_TOOLTIP: &str =
    "Use the **desktop app**. Run `steve app` to open it. It installs automatically if needed.";
const LINUX_APP_TOOLTIP: &str = "Use the **desktop app**. Install it from https://learn.chatgpt.com/docs/linux/linux-app and run `chatgpt`.";

const RAW_TOOLTIPS: &str = include_str!("../assets/tooltips.txt");

lazy_static! {
    static ref TOOLTIPS: Vec<&'static str> = RAW_TOOLTIPS
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .chain(app_tooltip())
        .collect();
    static ref ALL_TOOLTIPS: Vec<&'static str> = {
        let mut tips = Vec::new();
        tips.extend(TOOLTIPS.iter().copied());
        tips.extend(experimental_tooltips(
            FEATURES,
            codex_realtime_webrtc::RealtimeWebrtcSession::is_supported,
        ));
        tips
    };
}

fn experimental_tooltips(
    features: &[FeatureSpec],
    voice_supported: impl Fn() -> bool,
) -> Vec<&'static str> {
    features
        .iter()
        .filter(|spec| spec.id != Feature::RealtimeConversation || voice_supported())
        .filter_map(|spec| spec.stage.experimental_announcement())
        .collect()
}

/// Pick a random tooltip to show to the user when starting Steve.
pub(crate) fn get_tooltip(plan: Option<PlanType>, keymap: &TuiKeymap) -> Option<String> {
    let mut rng = rand::rng();
    preferred_tooltip(&mut rng, plan).or_else(|| pick_tooltip(&mut rng, keymap))
}

/// Return the platform app tip most of the time. The caller supplies the local tip pool.
pub(crate) fn preferred_tooltip<R: Rng + ?Sized>(
    rng: &mut R,
    _plan: Option<PlanType>,
) -> Option<String> {
    // Leave small chance for a random tooltip to be shown.
    if rng.random_ratio(/*numerator*/ 8, /*denominator*/ 10) {
        return app_tooltip().map(str::to_string);
    }

    None
}

struct LinuxDesktopSession {
    has_display: bool,
    is_wsl: bool,
}

impl LinuxDesktopSession {
    fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self {
                has_display: std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty())
                    || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()),
                is_wsl: crate::clipboard_paste::is_probably_wsl(),
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            Self {
                has_display: false,
                is_wsl: false,
            }
        }
    }
}

fn linux_app_tooltip(session: LinuxDesktopSession) -> Option<&'static str> {
    (session.has_display && !session.is_wsl).then_some(LINUX_APP_TOOLTIP)
}

fn app_tooltip() -> Option<&'static str> {
    if IS_MACOS {
        Some(MACOS_APP_TOOLTIP)
    } else if IS_WINDOWS {
        Some(WINDOWS_APP_TOOLTIP)
    } else {
        linux_app_tooltip(LinuxDesktopSession::current())
    }
}

fn pick_tooltip<R: Rng + ?Sized>(rng: &mut R, keymap: &TuiKeymap) -> Option<String> {
    // Resolve current settings for each new tip; never replace an invalid or unbound keymap
    // with defaults, or cache shortcut text across /keymap edits.
    let keymap = RuntimeKeymap::from_config(keymap).ok();
    resolved_tooltips(keymap.as_ref()).choose(rng)
}

/// Render shared tip styling and links, retaining visible URLs when the terminal needs them.
pub(crate) fn render_tooltip_lines(tip: &str, width: usize, cwd: &Path) -> Vec<HyperlinkLine> {
    crate::markdown_render::render_streaming_markdown_lines_with_width_and_cwd(
        &format!("**Tip:** {tip}"),
        Some(width),
        Some(cwd),
        &crate::markdown_render::hide_web_link_destination,
        crate::markdown_render::ListSpacing::AfterMultiline,
    )
    .lines
}

/// Resolve the local tip pool in catalog order using the supplied runtime keymap.
/// Tips with invalid or unbound shortcuts are omitted; without a keymap, only key-free tips remain.
pub(crate) fn resolved_tooltips(
    keymap: Option<&RuntimeKeymap>,
) -> impl Iterator<Item = String> + '_ {
    ALL_TOOLTIPS
        .iter()
        .filter_map(move |tip| render_tooltip(tip, keymap))
}

pub(crate) fn tooltip_templates() -> impl Iterator<Item = &'static str> {
    ALL_TOOLTIPS.iter().copied()
}

/// Substitute `{key:context.action}` with the current primary shortcut in a Markdown code span.
/// Skip the tip if a placeholder is invalid or its action has no binding.
pub(crate) fn render_tooltip(mut template: &str, keymap: Option<&RuntimeKeymap>) -> Option<String> {
    let mut rendered = String::new();
    while let Some((prefix, rest)) = template.split_once("{key:") {
        let (action, suffix) = rest.split_once('}')?;
        let (context, action) = action.split_once('.')?;
        let action = keymap_action_id(context, action)?;
        let hint = keymap?.primary_hint(action.context, action.action)?;
        rendered.push_str(prefix);
        // A key or two-key chord can contain literal backticks; use a padded code span.
        rendered.push_str(&format!("`` {} ``", hint.display_label()));
        template = suffix;
    }
    rendered.push_str(template);
    Some(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn experimental_voice_tooltip_requires_runtime_support() {
        let mut features = FEATURES.to_vec();
        features
            .iter_mut()
            .find(|spec| spec.id == Feature::RealtimeConversation)
            .unwrap()
            .stage = codex_features::Stage::Experimental {
            name: "Voice conversations",
            menu_description: "Talk with Steve using /voice.",
            announcement: "NEW: Voice conversations can now be enabled from /experimental. Restart Steve after enabling, then use /voice.",
        };
        let unavailable = experimental_tooltips(&features, || false);
        let available = experimental_tooltips(&features, || true);
        let voice_tip = features
            .iter()
            .find(|spec| spec.id == Feature::RealtimeConversation)
            .and_then(|spec| spec.stage.experimental_announcement())
            .expect("voice has an experimental announcement");
        assert_eq!(
            unavailable,
            available
                .iter()
                .copied()
                .filter(|tip| *tip != voice_tip)
                .collect::<Vec<_>>()
        );
        insta::assert_snapshot!(
            "experimental_voice_tooltip",
            available
                .into_iter()
                .filter(|tip| *tip == voice_tip)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn random_tooltip_returns_some_tip_when_available() {
        let mut rng = StdRng::seed_from_u64(42);
        assert!(pick_tooltip(&mut rng, &TuiKeymap::default()).is_some());
    }

    #[test]
    fn random_tooltip_is_reproducible_with_seed() {
        let expected = {
            let mut rng = StdRng::seed_from_u64(7);
            pick_tooltip(&mut rng, &TuiKeymap::default())
        };

        let mut rng = StdRng::seed_from_u64(7);
        assert_eq!(expected, pick_tooltip(&mut rng, &TuiKeymap::default()));
    }

    #[test]
    fn desktop_app_tips_render_at_narrow_width() {
        let cwd = std::env::current_dir().unwrap();
        for (platform, tip) in [
            ("macos", MACOS_APP_TOOLTIP),
            ("windows", WINDOWS_APP_TOOLTIP),
            ("linux", LINUX_APP_TOOLTIP),
        ] {
            let rendered = render_tooltip_lines(tip, /*width*/ 40, &cwd)
                .iter()
                .map(|line| format!("{:?} {:?}", line.line, line.hyperlinks))
                .collect::<Vec<_>>()
                .join("\n");
            insta::assert_snapshot!(format!("desktop_app_tip_{platform}"), rendered);
        }
    }

    #[test]
    fn desktop_app_tooltip_uses_supported_platform_launcher() {
        let tooltip = TOOLTIPS
            .iter()
            .copied()
            .find(|tip| tip.contains("desktop app"));

        if linux_app_tooltip(LinuxDesktopSession::current()).is_some() {
            let tooltip = tooltip.expect("Linux should advertise the desktop app");
            assert_eq!(app_tooltip(), Some(tooltip));
        } else if IS_MACOS {
            let tooltip = tooltip.expect("macOS should advertise the desktop app");
            assert_eq!(app_tooltip(), Some(tooltip));
        } else if IS_WINDOWS {
            let tooltip = tooltip.expect("Windows should advertise the desktop app");
            assert_eq!(app_tooltip(), Some(tooltip));
        } else {
            assert_eq!(tooltip, None);
            assert_eq!(app_tooltip(), None);
        }
    }

    #[test]
    fn linux_desktop_app_tooltip_requires_graphical_native_session() {
        assert_eq!(
            linux_app_tooltip(LinuxDesktopSession {
                has_display: true,
                is_wsl: false,
            }),
            Some(LINUX_APP_TOOLTIP)
        );

        assert_eq!(
            linux_app_tooltip(LinuxDesktopSession {
                has_display: false,
                is_wsl: false,
            }),
            None
        );
        assert_eq!(
            linux_app_tooltip(LinuxDesktopSession {
                has_display: true,
                is_wsl: true,
            }),
            None
        );
    }

    #[test]
    fn plans_use_platform_app_tip() {
        for plan in [
            Some(PlanType::Free),
            Some(PlanType::Go),
            Some(PlanType::Plus),
            Some(PlanType::Pro),
            None,
        ] {
            let mut seen = std::collections::BTreeSet::new();
            for seed in 0..32 {
                let mut rng = StdRng::seed_from_u64(seed);
                seen.insert(preferred_tooltip(&mut rng, plan));
            }
            assert_eq!(
                seen,
                std::collections::BTreeSet::from([None, app_tooltip().map(str::to_string)])
            );
        }
    }
}
