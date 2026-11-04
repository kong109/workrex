//! Sidebar width under Rex. Rex stores a split as a fraction of the window,
//! so a fixed column width needs a new fraction whenever the window resizes.

use super::store::WindowSidebar;

/// Columns the content side always keeps.
const MIN_CONTENT_COLS: f64 = 10.0;

/// The window's width in columns, from one block's columns and the share of
/// the window its rect covers.
pub(super) fn window_cols(block_cols: u32, rect_w: f64) -> Option<u32> {
    if block_cols == 0 || rect_w.is_nan() || rect_w <= 0.0 {
        return None;
    }
    let cols = (f64::from(block_cols) / rect_w).round();
    (cols >= 1.0 && cols <= f64::from(u16::MAX)).then_some(cols as u32)
}

/// The fraction of a `window_cols` wide window that leaves the sidebar `cols`
/// wide, kept inside (0, 1) with room for the content.
pub(super) fn sidebar_ratio(cols: u16, window_cols: u32) -> f64 {
    let window = f64::from(window_cols.max(1));
    let max = ((window - MIN_CONTENT_COLS) / window).max(0.05);
    (f64::from(cols.max(1)) / window).clamp(0.01, max.min(0.95))
}

/// The window width estimate a size policy works from, in `u16` for
/// `resolve_width_for`.
pub(super) fn extent(window_cols: u32) -> u16 {
    u16::try_from(window_cols).unwrap_or(u16::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum SizeAction {
    Keep,
    /// The window resized, the target changed, or Rex shared the row out
    /// evenly after a pane opened or closed: move the border.
    Enforce {
        ratio: f64,
    },
    /// The user dragged the border and no width is configured: the dragged
    /// width becomes every sidebar's width.
    PersistDrag {
        cols: u16,
    },
    /// The user dragged the border but a width is configured.
    SnapBack {
        ratio: f64,
    },
}

/// How close a ratio must be to the even share to count as Rex's balance.
/// Rex writes the exact fraction, and a drag lands on a pixel.
const EVEN_SHARE_EPSILON: f64 = 1e-6;

/// What one sidebar needs, given the ratio Rex shows now, the ratio Rex
/// would set to balance the row, its columns, the window width, and the
/// target width for that window.
pub(super) fn decide(
    record: &WindowSidebar,
    view_ratio: f64,
    even_ratio: f64,
    cols: u32,
    window_cols: u32,
    target: u16,
    width_configured: bool,
) -> SizeAction {
    let window = f64::from(window_cols.max(1));
    let moved = (view_ratio - record.applied_ratio).abs() > 0.5 / window;
    if moved && (view_ratio - even_ratio).abs() < EVEN_SHARE_EPSILON {
        return SizeAction::Enforce {
            ratio: sidebar_ratio(target, window_cols),
        };
    }
    if moved {
        return if width_configured {
            SizeAction::SnapBack {
                ratio: sidebar_ratio(target, window_cols),
            }
        } else {
            SizeAction::PersistDrag {
                cols: extent((view_ratio * window).round() as u32).max(1),
            }
        };
    }
    if cols.abs_diff(u32::from(target)) > 1 {
        return SizeAction::Enforce {
            ratio: sidebar_ratio(target, window_cols),
        };
    }
    SizeAction::Keep
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The even ratio of a sidebar beside one pane.
    const HALF: f64 = 0.5;

    fn record(applied_ratio: f64) -> WindowSidebar {
        WindowSidebar {
            session_id: "session:1".into(),
            window_id: "window:1".into(),
            block_id: "block:1".into(),
            split_id: "split:1".into(),
            applied_ratio,
            last_cols: 30,
            window_cols: 218,
        }
    }

    #[test]
    fn the_ratio_renders_the_target_within_a_column() {
        for window in [80u32, 120, 218, 300] {
            let ratio = sidebar_ratio(30, window);
            let rendered = (ratio * f64::from(window)).round() as u32;
            assert!(rendered.abs_diff(30) <= 1, "{window}: {rendered}");
        }
    }

    #[test]
    fn tiny_windows_keep_room_for_content() {
        let ratio = sidebar_ratio(30, 20);
        assert!(ratio > 0.0 && ratio < 1.0);
        assert!(ratio * 20.0 <= 10.0 + 1e-9);
        assert!(sidebar_ratio(0, 0) > 0.0);
    }

    #[test]
    fn window_cols_follow_from_a_block_and_its_rect() {
        assert_eq!(window_cols(30, 0.18), Some(167));
        assert_eq!(window_cols(98, 0.492), Some(199));
        assert_eq!(window_cols(80, 0.0), None);
        assert_eq!(window_cols(0, 0.5), None);
    }

    #[test]
    fn a_window_resize_enforces_the_target() {
        let applied = sidebar_ratio(30, 218);
        // Same ratio at 160 columns renders 22: recompute for 30.
        let cols = (applied * 160.0).round() as u32;
        assert_eq!(
            decide(&record(applied), applied, HALF, cols, 160, 30, false),
            SizeAction::Enforce {
                ratio: sidebar_ratio(30, 160)
            }
        );
    }

    #[test]
    fn a_sidebar_within_a_column_of_its_target_is_kept() {
        let applied = sidebar_ratio(30, 218);
        assert_eq!(
            decide(&record(applied), applied, HALF, 31, 218, 30, false),
            SizeAction::Keep
        );
        assert_eq!(
            decide(&record(applied), applied + 0.001, HALF, 30, 218, 30, true),
            SizeAction::Keep
        );
    }

    #[test]
    fn a_drag_persists_without_a_configured_width_and_snaps_back_with_one() {
        let applied = sidebar_ratio(30, 200);
        let dragged = 0.25;
        assert_eq!(
            decide(&record(applied), dragged, HALF, 50, 200, 30, false),
            SizeAction::PersistDrag { cols: 50 }
        );
        assert_eq!(
            decide(&record(applied), dragged, HALF, 50, 200, 30, true),
            SizeAction::SnapBack {
                ratio: sidebar_ratio(30, 200)
            }
        );
    }

    #[test]
    fn rex_balancing_the_row_restores_the_width_instead_of_saving_it() {
        let applied = sidebar_ratio(30, 200);
        let target = SizeAction::Enforce {
            ratio: sidebar_ratio(30, 200),
        };
        // A pane opened beside the only pane: Rex gives each of the three a third.
        let third = 1.0 / 3.0;
        for configured in [false, true] {
            assert_eq!(
                decide(&record(applied), third, third, 67, 200, 30, configured),
                target
            );
        }
        // The pane closed again: half each.
        assert_eq!(
            decide(&record(applied), HALF, HALF, 100, 200, 30, false),
            target
        );
        // A drag in the same window still persists.
        assert_eq!(
            decide(&record(applied), 0.3, third, 60, 200, 30, false),
            SizeAction::PersistDrag { cols: 60 }
        );
    }

    #[test]
    fn drift_without_recompute_exceeds_two_columns_quickly() {
        // The lane bound is 2 columns; a fixed ratio breaks it by 200 columns.
        let ratio = sidebar_ratio(30, 218);
        let drift = |window: f64| ((ratio * window).round() - 30.0).abs();
        assert!(drift(200.0) <= 2.0);
        assert!(drift(160.0) > 2.0);
        assert!(drift(300.0) > 2.0);
    }
}
