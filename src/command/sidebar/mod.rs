//! Rex sidebar clients, event-driven daemon, and shared rendering rules.
mod app;
mod client;
mod daemon;
mod daemon_ctrl;
#[cfg(target_os = "macos")]
mod fsevent;
pub(crate) mod host;
mod runtime;
mod snapshot;
mod template;
mod ui;
pub(crate) mod view;

pub(crate) use self::app::templates_use_prompt;
use self::daemon_ctrl::signal_daemon_for;
use crate::config::SidebarPosition;
use anyhow::{Result, bail};

const MIN_WIDTH: u16 = 25;
const MAX_WIDTH: u16 = 50;
const MAX_SANE_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 1;
const MAX_HEIGHT: u16 = 5;

fn default_width_for(tw: u16) -> u16 {
    if tw == 0 {
        return MIN_WIDTH;
    }
    (tw * 10 / 100).clamp(MIN_WIDTH, MAX_WIDTH)
}

pub(super) fn width_exceeds_defensive_max(width: u16) -> bool {
    width > MAX_SANE_WIDTH
}

fn resolve_width_for(config: &crate::config::Config, tw: u16, synced_width: Option<u16>) -> u16 {
    if let Some(ref w) = config.sidebar.width {
        let width = w.resolve(tw).max(1);
        return if width_exceeds_defensive_max(width) {
            default_width_for(tw)
        } else {
            width
        };
    }

    if let Some(w) = synced_width
        && !width_exceeds_defensive_max(w)
    {
        let max_w = tw.saturating_sub(10).max(1);
        return w.clamp(1, max_w);
    }

    default_width_for(tw)
}

fn resolve_height_for(config: &crate::config::Config, th: u16, synced_height: Option<u16>) -> u16 {
    let max_h = th.saturating_sub(3).max(1);
    if let Some(ref h) = config.sidebar.height {
        return h.resolve(th).clamp(1, max_h);
    }

    if let Some(h) = synced_height {
        return h.clamp(1, max_h);
    }

    let default = if th == 0 {
        3
    } else {
        (th * 10 / 100).clamp(MIN_HEIGHT, MAX_HEIGHT)
    };
    default.clamp(1, max_h)
}

pub(crate) fn request_refresh_for(mux: &dyn crate::multiplexer::Multiplexer) {
    signal_daemon_for(mux);
}

pub fn run_daemon() -> Result<()> {
    daemon::run()
}

pub fn run_sidebar() -> Result<()> {
    runtime::run_sidebar()
}

fn compute_nav_target(action: &NavAction, current_idx: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match action {
        NavAction::Next => {
            let i = current_idx.unwrap_or(len - 1);
            if i >= len - 1 { 0 } else { i + 1 }
        }
        NavAction::Prev => {
            let i = current_idx.unwrap_or(0);
            if i == 0 { len - 1 } else { i - 1 }
        }
        NavAction::Jump(n) => {
            let idx = n - 1;
            if idx >= len {
                return None;
            }
            idx
        }
    })
}

fn parse_sidebar_filter_mode(raw: &str) -> Result<app::SidebarFilterMode> {
    match raw.trim().to_lowercase().as_str() {
        "none" | "all" => Ok(app::SidebarFilterMode::None),
        "session" | "project" => Ok(app::SidebarFilterMode::Session),
        other => bail!("invalid sidebar filter mode {other:?}; expected none or session"),
    }
}

pub(crate) fn parse_sidebar_group_by(raw: &str) -> Result<Option<crate::config::SidebarGroupBy>> {
    match raw.trim().to_lowercase().as_str() {
        "none" | "off" | "flat" => Ok(None),
        "project" => Ok(Some(crate::config::SidebarGroupBy::Project)),
        "session" => Ok(Some(crate::config::SidebarGroupBy::Session)),
        other => bail!("invalid sidebar grouping {other:?}; expected none, project or session"),
    }
}

pub(crate) fn group_by_option_value(
    group_by: Option<crate::config::SidebarGroupBy>,
) -> &'static str {
    match group_by {
        None | Some(crate::config::SidebarGroupBy::None) => "none",
        Some(crate::config::SidebarGroupBy::Project) => "project",
        Some(crate::config::SidebarGroupBy::Session) => "session",
    }
}

fn current_listed_window_pane<'a>(
    panes: &'a [&str],
    current_pane_id: &'a str,
    current_window_id: &str,
    pane_window_ids: &std::collections::HashMap<String, String>,
) -> Option<&'a str> {
    if panes.contains(&current_pane_id) {
        return Some(current_pane_id);
    }

    panes.iter().copied().find(|pane_id| {
        pane_window_ids
            .get(*pane_id)
            .is_some_and(|window_id| window_id == current_window_id)
    })
}

fn navigation_anchor_pane<'a>(
    panes: &'a [&str],
    current_pane_id: &'a str,
    current_window_id: &str,
    pane_window_ids: &std::collections::HashMap<String, String>,
) -> Option<&'a str> {
    current_listed_window_pane(panes, current_pane_id, current_window_id, pane_window_ids)
        .or(Some(current_pane_id).filter(|pane_id| panes.contains(pane_id)))
}

pub enum NavAction {
    Next,
    Prev,
    Jump(usize),
}

pub(super) fn read_sidebar_position(config: &crate::config::Config) -> SidebarPosition {
    config.sidebar.position.unwrap_or_default()
}

pub(crate) fn sleeping_pane_ids(
    mux: &dyn crate::multiplexer::Multiplexer,
) -> std::collections::HashSet<String> {
    host::sleeping_pane_ids(mux).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sidebar_group_by_accepts_every_runtime_value() {
        assert_eq!(parse_sidebar_group_by("none").unwrap(), None);
        assert_eq!(parse_sidebar_group_by("Off").unwrap(), None);
        assert_eq!(
            parse_sidebar_group_by(" Project ").unwrap(),
            Some(crate::config::SidebarGroupBy::Project)
        );
        assert_eq!(
            parse_sidebar_group_by("session").unwrap(),
            Some(crate::config::SidebarGroupBy::Session)
        );
        assert!(parse_sidebar_group_by("porject").is_err());
    }

    #[test]
    fn group_by_option_values_round_trip() {
        for mode in [
            None,
            Some(crate::config::SidebarGroupBy::Project),
            Some(crate::config::SidebarGroupBy::Session),
        ] {
            assert_eq!(
                parse_sidebar_group_by(group_by_option_value(mode)).unwrap(),
                mode
            );
        }
    }

    #[test]
    fn parse_sidebar_filter_mode_rejects_invalid_values() {
        assert!(parse_sidebar_filter_mode("sessoin").is_err());
    }

    #[test]
    fn next_wraps_from_last_to_first() {
        assert_eq!(compute_nav_target(&NavAction::Next, Some(2), 3), Some(0));
    }

    #[test]
    fn next_advances_normally() {
        assert_eq!(compute_nav_target(&NavAction::Next, Some(0), 3), Some(1));
        assert_eq!(compute_nav_target(&NavAction::Next, Some(1), 3), Some(2));
    }

    #[test]
    fn next_without_current_wraps_from_last() {
        // No current window match: starts from last, wraps to first
        assert_eq!(compute_nav_target(&NavAction::Next, None, 3), Some(0));
    }

    #[test]
    fn prev_wraps_from_first_to_last() {
        assert_eq!(compute_nav_target(&NavAction::Prev, Some(0), 3), Some(2));
    }

    #[test]
    fn prev_goes_back_normally() {
        assert_eq!(compute_nav_target(&NavAction::Prev, Some(2), 3), Some(1));
        assert_eq!(compute_nav_target(&NavAction::Prev, Some(1), 3), Some(0));
    }

    #[test]
    fn prev_without_current_wraps_to_last() {
        // No current window match: starts from 0, wraps to last
        assert_eq!(compute_nav_target(&NavAction::Prev, None, 3), Some(2));
    }

    #[test]
    fn jump_converts_1_indexed_to_0_indexed() {
        assert_eq!(compute_nav_target(&NavAction::Jump(1), None, 3), Some(0));
        assert_eq!(compute_nav_target(&NavAction::Jump(2), None, 3), Some(1));
        assert_eq!(compute_nav_target(&NavAction::Jump(3), None, 3), Some(2));
    }

    #[test]
    fn jump_out_of_range_returns_none() {
        assert_eq!(compute_nav_target(&NavAction::Jump(4), None, 3), None);
        assert_eq!(compute_nav_target(&NavAction::Jump(10), None, 3), None);
    }

    #[test]
    fn empty_list_returns_none() {
        assert_eq!(compute_nav_target(&NavAction::Next, None, 0), None);
        assert_eq!(compute_nav_target(&NavAction::Prev, None, 0), None);
        assert_eq!(compute_nav_target(&NavAction::Jump(1), None, 0), None);
    }

    #[test]
    fn single_agent_next_stays() {
        assert_eq!(compute_nav_target(&NavAction::Next, Some(0), 1), Some(0));
    }

    #[test]
    fn single_agent_prev_stays() {
        assert_eq!(compute_nav_target(&NavAction::Prev, Some(0), 1), Some(0));
    }

    #[test]
    fn resolve_width_uses_synced_width() {
        let config = crate::config::Config::default();
        assert_eq!(resolve_width_for(&config, 200, Some(40)), 40);
    }

    #[test]
    fn resolve_width_preserves_narrow_explicit_widths() {
        let mut config = crate::config::Config::default();
        for (width, expected) in [(0, 1), (1, 1), (8, 8)] {
            config.sidebar.width = Some(crate::config::SidebarWidth::Absolute(width));
            assert_eq!(resolve_width_for(&config, 200, Some(40)), expected);
        }
    }

    #[test]
    fn resolve_width_preserves_narrow_synced_widths() {
        let config = crate::config::Config::default();
        for (width, expected) in [(0, 1), (1, 1), (8, 8)] {
            assert_eq!(resolve_width_for(&config, 200, Some(width)), expected);
        }
    }

    #[test]
    fn resolve_width_preserves_narrow_percentages() {
        let mut config = crate::config::Config::default();
        config.sidebar.width = Some(crate::config::SidebarWidth::Percent(4));
        assert_eq!(resolve_width_for(&config, 200, None), 8);
        assert_eq!(resolve_width_for(&config, 20, None), 1);
    }

    #[test]
    fn resolve_width_reserves_content_space_in_small_windows() {
        let config = crate::config::Config::default();
        for (window, expected) in [(18, 8), (15, 5), (11, 1), (10, 1), (0, 1)] {
            assert_eq!(resolve_width_for(&config, window, Some(8)), expected);
        }
    }

    #[test]
    fn resolve_width_clamps_synced_to_window() {
        let config = crate::config::Config::default();
        assert_eq!(resolve_width_for(&config, 60, Some(80)), 50);
        assert_eq!(resolve_width_for(&config, 200, Some(5)), 5);
    }

    #[test]
    fn resolve_width_clamps_narrow_window() {
        let config = crate::config::Config::default();
        assert_eq!(resolve_width_for(&config, 25, Some(50)), 15);
    }

    #[test]
    fn resolve_width_falls_back_to_default_without_sync() {
        let config = crate::config::Config::default();
        assert_eq!(resolve_width_for(&config, 200, None), 25);
        assert_eq!(resolve_width_for(&config, 500, None), 50);
        assert_eq!(resolve_width_for(&config, 400, None), 40);
    }

    #[test]
    fn resolve_width_uses_default_for_excessive_synced_width() {
        let config = crate::config::Config::default();
        assert_eq!(resolve_width_for(&config, 200, Some(150)), 25);
    }

    #[test]
    fn resolve_width_uses_default_for_excessive_explicit_width() {
        let mut config = crate::config::Config::default();
        config.sidebar.width = Some(crate::config::SidebarWidth::Absolute(150));
        assert_eq!(resolve_width_for(&config, 200, Some(40)), 25);
    }

    #[test]
    fn resolve_width_keeps_explicit_width_at_defensive_max() {
        let mut config = crate::config::Config::default();
        config.sidebar.width = Some(crate::config::SidebarWidth::Absolute(80));
        assert_eq!(resolve_width_for(&config, 200, None), 80);
    }

    #[test]
    fn resolve_height_uses_explicit_config_before_synced_height() {
        let mut config = crate::config::Config::default();
        config.sidebar.height = Some(crate::config::SidebarHeight::Absolute(2));
        assert_eq!(resolve_height_for(&config, 40, Some(1)), 2);
    }

    #[test]
    fn resolve_width_uses_explicit_config_before_synced_width() {
        let mut config = crate::config::Config::default();
        config.sidebar.width = Some(crate::config::SidebarWidth::Percent(10));
        assert_eq!(resolve_width_for(&config, 200, Some(150)), 20);
    }

    #[test]
    fn resolve_width_keeps_explicit_width_upper_bound() {
        let mut config = crate::config::Config::default();
        config.sidebar.width = Some(crate::config::SidebarWidth::Absolute(80));
        assert_eq!(resolve_width_for(&config, 100, None), 80);
    }

    #[test]
    fn current_window_falls_back_to_listed_agent_pane() {
        let panes = vec!["%1", "%2"];
        let pane_window_ids = std::collections::HashMap::from([
            ("%1".to_string(), "@10".to_string()),
            ("%2".to_string(), "@20".to_string()),
        ]);

        assert_eq!(
            current_listed_window_pane(&panes, "%99", "@20", &pane_window_ids),
            Some("%2")
        );
    }

    #[test]
    fn current_listed_pane_wins_over_window_fallback() {
        let panes = vec!["%1", "%2"];
        let pane_window_ids = std::collections::HashMap::from([
            ("%1".to_string(), "@10".to_string()),
            ("%2".to_string(), "@10".to_string()),
        ]);

        assert_eq!(
            current_listed_window_pane(&panes, "%2", "@10", &pane_window_ids),
            Some("%2")
        );
    }

    #[test]
    fn navigation_anchor_uses_agent_in_current_window() {
        let panes = vec!["%1", "%2"];
        let pane_window_ids = std::collections::HashMap::from([
            ("%sidebar".to_string(), "@10".to_string()),
            ("%1".to_string(), "@10".to_string()),
            ("%2".to_string(), "@20".to_string()),
        ]);

        assert_eq!(
            navigation_anchor_pane(&panes, "%sidebar", "@10", &pane_window_ids),
            Some("%1")
        );
    }

    #[test]
    fn navigation_anchor_prefers_current_agent_pane() {
        let panes = vec!["%1", "%2"];
        let pane_window_ids = std::collections::HashMap::from([
            ("%1".to_string(), "@10".to_string()),
            ("%2".to_string(), "@10".to_string()),
        ]);

        assert_eq!(
            navigation_anchor_pane(&panes, "%2", "@10", &pane_window_ids),
            Some("%2")
        );
    }

    #[test]
    fn next_from_sidebar_in_filtered_two_window_session_advances_to_other_window() {
        let panes = vec!["%0", "%317"];
        let pane_window_ids = std::collections::HashMap::from([
            ("%sidebar".to_string(), "@0".to_string()),
            ("%0".to_string(), "@0".to_string()),
            ("%317".to_string(), "@33".to_string()),
        ]);

        let current_idx = navigation_anchor_pane(&panes, "%sidebar", "@0", &pane_window_ids)
            .and_then(|pane_id| panes.iter().position(|&pid| pid == pane_id));

        assert_eq!(current_idx, Some(0));
        assert_eq!(
            compute_nav_target(&NavAction::Next, current_idx, panes.len()),
            Some(1)
        );
    }

    #[test]
    fn next_from_agent_in_filtered_two_window_session_advances_to_other_window() {
        let panes = vec!["%0", "%317"];
        let pane_window_ids = std::collections::HashMap::from([
            ("%0".to_string(), "@0".to_string()),
            ("%317".to_string(), "@33".to_string()),
        ]);

        let current_idx = navigation_anchor_pane(&panes, "%0", "@0", &pane_window_ids)
            .and_then(|pane_id| panes.iter().position(|&pid| pid == pane_id));

        assert_eq!(current_idx, Some(0));
        assert_eq!(
            compute_nav_target(&NavAction::Next, current_idx, panes.len()),
            Some(1)
        );
    }
}
