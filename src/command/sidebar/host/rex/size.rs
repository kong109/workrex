//! Sidebar extents under Rex. A left sidebar measures columns; a top bar
//! measures rows. Fixed cell sizes need a new split fraction when the window resizes.

use super::store::WindowSidebar;
use crate::config::SidebarPosition;
use crate::multiplexer::rex::sidebar_api::Window;

/// Columns a left sidebar always leaves for content.
const MIN_CONTENT_COLS: f64 = 10.0;

/// The window extent in cells, reconstructed from a block's extent and
/// its fraction of the matching terminal axis.
pub(super) fn window_extent(block_extent: u32, rect_extent: f64) -> Option<u32> {
    if block_extent == 0 || rect_extent.is_nan() || rect_extent <= 0.0 {
        return None;
    }
    let cells = (f64::from(block_extent) / rect_extent).round();
    (cells >= 1.0 && cells <= f64::from(u16::MAX)).then_some(cells as u32)
}

/// The split fraction that gives the sidebar its requested cells while
/// leaving room for the content on the chosen axis.
pub(super) fn sidebar_ratio(cells: u16, window_extent: u32, position: SidebarPosition) -> f64 {
    let window = f64::from(window_extent.max(1));
    let min_content = match position {
        SidebarPosition::Left => MIN_CONTENT_COLS,
        SidebarPosition::Top => 3.0,
    };
    let max = ((window - min_content) / window).max(0.05);
    (f64::from(cells.max(1)) / window).clamp(0.01, max.min(0.95))
}

/// Saturate a window extent to the cell count accepted by size policies.
pub(super) fn extent(window_extent: u32) -> u16 {
    u16::try_from(window_extent).unwrap_or(u16::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum SizeAction {
    Keep,
    /// The window resized, the target changed, or Rex shared the row out
    /// evenly after a pane opened or closed: move the border.
    Enforce {
        ratio: f64,
    },
    /// The user dragged the border and no extent is configured: the dragged
    /// extent becomes every sidebar's extent.
    PersistDrag {
        cells: u16,
    },
    /// The user dragged the border but a extent is configured.
    SnapBack {
        ratio: f64,
    },
}

/// How close a ratio must be to an even share to count as Rex's balance.
/// Rex writes the exact fraction, and a drag lands on a pixel.
const EVEN_SHARE_EPSILON: f64 = 1e-6;

/// Whether a ratio is a share Rex's balance can write. The sidebar is the
/// root's single `before` leaf, so a balance puts it at exactly 1/n. Any n
/// counts: a second pane change can land before the daemon reads the first
/// balance, so the row the daemon sees may not be the row Rex balanced.
fn is_even_share(ratio: f64) -> bool {
    (2..=64u32).any(|n| (ratio - 1.0 / f64::from(n)).abs() < EVEN_SHARE_EPSILON)
}

/// How long after a pane opens or closes in a window a move to an even share
/// still counts as Rex's balance. Rex balances within milliseconds, but a
/// pass can read the new pane before the balance lands, and the next pass
/// may come late under load.
const BALANCE_WINDOW_MS: u64 = 5_000;

/// Note the window's tiled blocks in its record. Returns whether a pass saw
/// them change within `BALANCE_WINDOW_MS` of `now_ms`. Keyboard resizes step
/// the ratio by 5%, onto shares like 1/5 and 1/20, so an even share with no
/// pane change is the user's.
pub(super) fn note_panes(record: &mut WindowSidebar, window: &Window, now_ms: u64) -> bool {
    let panes = panes(window, None);
    if record.panes != panes {
        record.panes = panes;
        record.panes_changed_ms = now_ms;
    }
    now_ms
        .checked_sub(record.panes_changed_ms)
        .is_some_and(|age| age < BALANCE_WINDOW_MS)
}

/// The window's tiled blocks, plus `added` when a sidebar is being placed in
/// it, in the form `WindowSidebar::panes` keeps.
pub(super) fn panes(window: &Window, added: Option<&str>) -> String {
    let mut blocks: Vec<&str> = window
        .tiled()
        .map(|layer| layer.blocks.iter().map(|b| b.block_id.as_str()).collect())
        .unwrap_or_default();
    blocks.extend(added);
    blocks.sort_unstable();
    blocks.join(" ")
}

/// What one sidebar needs, given the ratio Rex shows now, its columns, the
/// window width, the target width for that window, and whether a pane opened
/// or closed in the window just now.
pub(super) fn decide(
    record: &WindowSidebar,
    view_ratio: f64,
    cells: u32,
    window_extent: u32,
    target: u16,
    extent_configured: bool,
    panes_changed: bool,
) -> SizeAction {
    let window = f64::from(window_extent.max(1));
    let moved = (view_ratio - record.applied_ratio).abs() > 0.5 / window;
    if moved && panes_changed && is_even_share(view_ratio) {
        return SizeAction::Enforce {
            ratio: sidebar_ratio(target, window_extent, record.position),
        };
    }
    if moved {
        return if extent_configured {
            SizeAction::SnapBack {
                ratio: sidebar_ratio(target, window_extent, record.position),
            }
        } else {
            SizeAction::PersistDrag {
                cells: extent((view_ratio * window).round() as u32).max(1),
            }
        };
    }
    if cells.abs_diff(u32::from(target)) > 1 {
        return SizeAction::Enforce {
            ratio: sidebar_ratio(target, window_extent, record.position),
        };
    }
    SizeAction::Keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(applied_ratio: f64) -> WindowSidebar {
        WindowSidebar {
            session_id: "session:1".into(),
            window_id: "window:1".into(),
            block_id: "block:1".into(),
            split_id: "split:1".into(),
            position: SidebarPosition::Left,
            stray: None,
            applied_ratio,
            last_extent: 30,
            window_extent: 218,
            panes: String::new(),
            panes_changed_ms: 0,
        }
    }

    fn window(blocks: &[&str]) -> Window {
        let blocks: Vec<_> = blocks
            .iter()
            .map(|id| serde_json::json!({"block_id": id}))
            .collect();
        serde_json::from_value(serde_json::json!({
            "window_id": "window:1",
            "layers": [{"kind": "tiled", "blocks": blocks}],
        }))
        .unwrap()
    }

    #[test]
    fn the_ratio_renders_the_target_within_a_column() {
        for window in [80u32, 120, 218, 300] {
            let ratio = sidebar_ratio(30, window, SidebarPosition::Left);
            let rendered = (ratio * f64::from(window)).round() as u32;
            assert!(rendered.abs_diff(30) <= 1, "{window}: {rendered}");
        }
    }

    #[test]
    fn tiny_windows_keep_room_for_content() {
        let ratio = sidebar_ratio(30, 20, SidebarPosition::Left);
        assert!(ratio > 0.0 && ratio < 1.0);
        assert!(ratio * 20.0 <= 10.0 + 1e-9);
        assert!(sidebar_ratio(0, 0, SidebarPosition::Left) > 0.0);
    }

    #[test]
    fn window_extent_follow_from_a_block_and_its_rect() {
        assert_eq!(window_extent(30, 0.18), Some(167));
        assert_eq!(window_extent(98, 0.492), Some(199));
        assert_eq!(window_extent(80, 0.0), None);
        assert_eq!(window_extent(0, 0.5), None);
    }

    #[test]
    fn a_window_resize_enforces_the_target() {
        let applied = sidebar_ratio(30, 218, SidebarPosition::Left);
        // Same ratio at 160 columns renders 22: recompute for 30.
        let cells = (applied * 160.0).round() as u32;
        assert_eq!(
            decide(&record(applied), applied, cells, 160, 30, false, true),
            SizeAction::Enforce {
                ratio: sidebar_ratio(30, 160, SidebarPosition::Left)
            }
        );
    }

    #[test]
    fn a_sidebar_within_a_column_of_its_target_is_kept() {
        let applied = sidebar_ratio(30, 218, SidebarPosition::Left);
        assert_eq!(
            decide(&record(applied), applied, 31, 218, 30, false, true),
            SizeAction::Keep
        );
        assert_eq!(
            decide(&record(applied), applied + 0.001, 30, 218, 30, true, true),
            SizeAction::Keep
        );
    }

    #[test]
    fn a_drag_persists_without_a_configured_width_and_snaps_back_with_one() {
        let applied = sidebar_ratio(30, 200, SidebarPosition::Left);
        let dragged = 0.26;
        assert_eq!(
            decide(&record(applied), dragged, 52, 200, 30, false, true),
            SizeAction::PersistDrag { cells: 52 }
        );
        assert_eq!(
            decide(&record(applied), dragged, 52, 200, 30, true, true),
            SizeAction::SnapBack {
                ratio: sidebar_ratio(30, 200, SidebarPosition::Left)
            }
        );
    }

    #[test]
    fn rex_balancing_the_row_restores_the_width_instead_of_saving_it() {
        let applied = sidebar_ratio(30, 200, SidebarPosition::Left);
        let target = SizeAction::Enforce {
            ratio: sidebar_ratio(30, 200, SidebarPosition::Left),
        };
        // A pane opened beside the only pane: Rex gives each of the three a third.
        let third = 1.0 / 3.0;
        for configured in [false, true] {
            assert_eq!(
                decide(&record(applied), third, 67, 200, 30, configured, true),
                target
            );
        }
        // The pane closed again: half each.
        assert_eq!(
            decide(&record(applied), 0.5, 100, 200, 30, false, true),
            target
        );
        // A drag in the same window still persists.
        assert_eq!(
            decide(&record(applied), 0.3, 60, 200, 30, false, true),
            SizeAction::PersistDrag { cells: 60 }
        );
    }

    #[test]
    fn rex_balancing_a_wide_row_restores_the_width() {
        let applied = sidebar_ratio(30, 200, SidebarPosition::Left);
        for n in [5.0, 8.0] {
            assert_eq!(
                decide(&record(applied), 1.0 / n, 40, 200, 30, false, true),
                SizeAction::Enforce {
                    ratio: sidebar_ratio(30, 200, SidebarPosition::Left)
                }
            );
        }
    }

    #[test]
    fn a_keyboard_step_onto_an_even_share_persists_when_no_pane_changed() {
        // Rex's pane.resize moves the ratio by 0.05: 0.15 to 1/5, 0.1 to 1/20.
        let applied = sidebar_ratio(30, 200, SidebarPosition::Left);
        assert_eq!(
            decide(&record(applied), 0.2, 40, 200, 30, false, false),
            SizeAction::PersistDrag { cells: 40 }
        );
        assert_eq!(
            decide(&record(applied), 0.05, 10, 200, 30, false, false),
            SizeAction::PersistDrag { cells: 10 }
        );
        assert_eq!(
            decide(&record(applied), 0.2, 40, 200, 30, true, false),
            SizeAction::SnapBack {
                ratio: sidebar_ratio(30, 200, SidebarPosition::Left)
            }
        );
    }

    #[test]
    fn a_pane_change_counts_for_the_balance_window_only() {
        let mut record = record(0.15);
        assert!(note_panes(
            &mut record,
            &window(&["block:1", "block:2"]),
            1_000
        ));
        // A later pass that sees the same panes is still inside the window.
        assert!(note_panes(
            &mut record,
            &window(&["block:2", "block:1"]),
            5_999
        ));
        assert!(!note_panes(
            &mut record,
            &window(&["block:1", "block:2"]),
            6_000
        ));
        assert!(note_panes(
            &mut record,
            &window(&["block:1", "block:2", "block:3"]),
            9_000
        ));
        assert!(!note_panes(
            &mut record,
            &window(&["block:1", "block:2", "block:3"]),
            14_000
        ));
    }

    #[test]
    fn floating_a_pane_is_a_pane_change() {
        // Rex.app balances the root row when a pane floats or unfloats.
        let layers = |tiled: &[&str], floating: &[&str]| -> Window {
            let ids = |ids: &[&str]| -> Vec<_> {
                ids.iter()
                    .map(|id| serde_json::json!({"block_id": id}))
                    .collect()
            };
            serde_json::from_value(serde_json::json!({
                "window_id": "window:1",
                "layers": [
                    {"kind": "tiled", "blocks": ids(tiled)},
                    {"kind": "floating", "blocks": ids(floating)},
                ],
            }))
            .unwrap()
        };
        let mut record = record(0.15);
        note_panes(
            &mut record,
            &layers(&["block:1", "block:2", "block:3"], &[]),
            1_000,
        );
        assert!(!note_panes(
            &mut record,
            &layers(&["block:1", "block:2", "block:3"], &[]),
            7_000
        ));
        assert!(note_panes(
            &mut record,
            &layers(&["block:1", "block:2"], &["block:3"]),
            8_000
        ));
    }

    #[test]
    fn a_sidebar_already_at_the_even_share_is_kept() {
        let applied = sidebar_ratio(40, 80, SidebarPosition::Left);
        assert_eq!(applied, 0.5);
        assert_eq!(
            decide(&record(applied), applied, 40, 80, 40, false, true),
            SizeAction::Keep
        );
    }

    #[test]
    fn a_drag_a_pixel_off_the_even_share_persists() {
        let applied = sidebar_ratio(30, 200, SidebarPosition::Left);
        let dragged = 1.0 / 3.0 + 1.0 / 1400.0;
        assert_eq!(
            decide(&record(applied), dragged, 67, 200, 30, false, true),
            SizeAction::PersistDrag { cells: 67 }
        );
    }

    #[test]
    fn drift_without_recompute_exceeds_two_columns_quickly() {
        // The lane bound is 2 columns; a fixed ratio breaks it by 200 columns.
        let ratio = sidebar_ratio(30, 218, SidebarPosition::Left);
        let drift = |window: f64| ((ratio * window).round() - 30.0).abs();
        assert!(drift(200.0) <= 2.0);
        assert!(drift(160.0) > 2.0);
        assert!(drift(300.0) > 2.0);
    }
}
