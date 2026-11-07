//! Window targets that callers spell the rex way.

use super::WindowId;

/// The window in an `after_window` target: a bare Rex window id, or the
/// rex-style `=<session>:<window id>` that remote siblings build.
pub(super) fn after_window(target: &str) -> WindowId {
    let id = match target.strip_prefix('=') {
        Some(scoped) => scoped.rfind("window:").map_or(scoped, |at| &scoped[at..]),
        None => target,
    };
    WindowId(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_window_id_is_kept() {
        assert_eq!(after_window("window:7f3a").as_str(), "window:7f3a");
    }

    #[test]
    fn a_session_scoped_target_names_its_window() {
        assert_eq!(after_window("=wr-feat:window:7f3a").as_str(), "window:7f3a");
        assert_eq!(after_window("=a:b window:x:window:9").as_str(), "window:9");
    }
}
