//! Quota selection, sparse updates, read ordering, and compact notice formatting.

use super::*;
use chrono::TimeZone;
use pretty_assertions::assert_eq;

fn snapshot(used_percent: i32) -> RateLimitSnapshot {
    serde_json::from_value(serde_json::json!({
        "primary": {"usedPercent": used_percent, "windowDurationMins": 300, "resetsAt": 1000},
    }))
    .unwrap()
}

fn notice_time() -> DateTime<Local> {
    Local
        .with_ymd_and_hms(
            /*year*/ 2026, /*month*/ 9, /*day*/ 20, /*hour*/ 12,
            /*min*/ 0, /*sec*/ 0,
        )
        .single()
        .unwrap()
}

#[test]
fn notice_details_fit_without_truncating_the_warning() {
    let now = notice_time();
    let mut primary = snapshot(/*used_percent*/ 92).primary.unwrap();
    primary.resets_at = Some((now + chrono::Duration::hours(/*hours*/ 1)).timestamp());
    for (name, state, widths) in [
        (
            "usage_notice_widths",
            UsageNoticeState {
                primary: Some(primary),
                ..Default::default()
            },
            &[98, 97, 58, 57, 38, 37, 12, 11, 0][..],
        ),
        (
            "weekly_usage_notice_widths",
            UsageNoticeState {
                secondary: Some(RateLimitWindow {
                    used_percent: 98,
                    window_duration_mins: Some(10080),
                    resets_at: Some((now + chrono::Duration::days(/*days*/ 1)).timestamp()),
                }),
                ..Default::default()
            },
            &[138, 137, 76, 75, 56, 55, 16, 15][..],
        ),
    ] {
        let mut lines = Vec::new();
        for &width in widths {
            let line = state.line(width, now, ClockFormat::TwelveHour);
            assert!(
                line.as_ref()
                    .is_none_or(|line| line.width() <= usize::from(width))
            );
            lines.push(
                format!("{width} columns: {}", line.unwrap_or_default())
                    .trim_end()
                    .to_owned(),
            );
        }
        insta::assert_snapshot!(name, lines.join("\n"));
    }
}

#[test]
fn notice_shows_only_valid_future_reset_times() {
    let now = notice_time();
    let mut state = UsageNoticeState {
        primary: snapshot(/*used_percent*/ 92).primary,
        ..Default::default()
    };
    for (reset, expected) in [
        (None, ""),
        (Some(now.timestamp() - 1), ""),
        (Some(now.timestamp()), ""),
        (Some(i64::MAX), ""),
        (
            Some((now + chrono::Duration::days(/*days*/ 1)).timestamp()),
            " · resets at 12:00 on 21 Sep",
        ),
    ] {
        state.primary.as_mut().unwrap().resets_at = reset;
        assert_eq!(
            state.line(/*width*/ 160, now, ClockFormat::TwentyFourHour),
            Some(
                Line::from(format!("⚠ 5h limit: 8% left{expected} · /status"))
                    .style(crate::style::warning_notice_style().bold())
            ),
        );
    }
}

#[test]
fn notice_emphasis_tracks_utilization_without_claiming_a_hard_stop() {
    for (used, remaining, bold) in [
        (89, "11%", false),
        (90, "10%", true),
        (94, "6%", true),
        (95, "only 5%", true),
        (100, "only <1%", true),
        (101, "only <1%", true),
    ] {
        let state = UsageNoticeState {
            primary: snapshot(used).primary,
            ..Default::default()
        };
        let style = crate::style::warning_notice_style();
        assert_eq!(
            state.line(
                /*width*/ 80,
                notice_time(),
                ClockFormat::TwentyFourHour
            ),
            Some(
                Line::from(format!("⚠ 5h limit: {remaining} left · /status")).style(if bold {
                    style.bold()
                } else {
                    style
                })
            ),
        );
    }
}
