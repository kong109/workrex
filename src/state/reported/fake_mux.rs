//! A multiplexer with fixed live panes and reported statuses.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;

use crate::config::Config;
use crate::multiplexer::types::{
    CreateSessionParams, CreateWindowInSessionParams, CreateWindowParams, LivePaneInfo,
    PaneSetupOptions, PaneSetupResult,
};
use crate::multiplexer::{Multiplexer, PaneHandshake, ReportedStatus};

pub(crate) const INSTANCE: &str = "unix:///tmp/rex.sock";

#[derive(Default)]
pub(crate) struct FakeMux {
    pub live: HashMap<String, LivePaneInfo>,
    /// What a fresh read of one pane finds.
    pub fresh: HashMap<String, LivePaneInfo>,
    /// A fresh read fails, as a Rex call that times out would.
    pub fresh_fails: bool,
    pub reported: HashMap<String, ReportedStatus>,
    /// Runs once, inside the first status query, as a hook racing a reader would.
    pub during_query: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Multiplexer for FakeMux {
    fn name(&self) -> &'static str {
        "rex"
    }

    fn is_running(&self) -> Result<bool> {
        Ok(true)
    }

    fn current_pane_id(&self) -> Option<String> {
        None
    }

    fn active_pane_id(&self) -> Option<String> {
        None
    }

    fn get_client_active_pane_path(&self) -> Result<PathBuf> {
        Ok(PathBuf::new())
    }

    fn create_window(&self, _params: CreateWindowParams) -> Result<String> {
        Ok("pane-1".to_string())
    }

    fn create_session(&self, _params: CreateSessionParams) -> Result<String> {
        Ok("pane-1".to_string())
    }

    fn create_window_in_session(&self, _params: CreateWindowInSessionParams) -> Result<String> {
        Ok("pane-1".to_string())
    }

    fn switch_to_session(&self, _prefix: &str, _name: &str) -> Result<()> {
        Ok(())
    }

    fn session_exists(&self, _full_name: &str) -> Result<bool> {
        Ok(false)
    }

    fn kill_session(&self, _full_name: &str) -> Result<()> {
        Ok(())
    }

    fn kill_window(&self, _full_name: &str) -> Result<()> {
        Ok(())
    }

    fn schedule_window_close(&self, _full_name: &str, _delay: Duration) -> Result<()> {
        Ok(())
    }

    fn schedule_session_close(&self, _full_name: &str, _delay: Duration) -> Result<()> {
        Ok(())
    }

    fn run_deferred_script(&self, _script: &str) -> Result<()> {
        Ok(())
    }

    fn shell_select_window_cmd(&self, _full_name: &str) -> Result<String> {
        Ok(String::new())
    }

    fn shell_kill_window_cmd(&self, _full_name: &str) -> Result<String> {
        Ok(String::new())
    }

    fn shell_switch_session_cmd(&self, _full_name: &str) -> Result<String> {
        Ok(String::new())
    }

    fn shell_kill_session_cmd(&self, _full_name: &str) -> Result<String> {
        Ok(String::new())
    }

    fn select_window(&self, _prefix: &str, _name: &str) -> Result<()> {
        Ok(())
    }

    fn window_exists(&self, _prefix: &str, _name: &str) -> Result<bool> {
        Ok(false)
    }

    fn window_exists_by_full_name(&self, _full_name: &str) -> Result<bool> {
        Ok(false)
    }

    fn current_window_name(&self) -> Result<Option<String>> {
        Ok(None)
    }

    fn get_all_window_names(&self) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }

    fn get_all_session_names(&self) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }

    fn filter_active_windows(&self, _windows: &[String]) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn wait_until_windows_closed(&self, _full_window_names: &[String]) -> Result<()> {
        Ok(())
    }

    fn wait_until_session_closed(&self, _full_session_name: &str) -> Result<()> {
        Ok(())
    }

    fn select_pane(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    fn switch_to_pane(&self, _pane_id: &str, _window_hint: Option<&str>) -> Result<()> {
        Ok(())
    }

    fn kill_pane(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    fn respawn_pane(&self, pane_id: &str, _cwd: &Path, _cmd: Option<&str>) -> Result<String> {
        Ok(pane_id.to_string())
    }

    fn capture_pane(&self, _pane_id: &str, _lines: u16) -> Option<String> {
        None
    }

    fn send_text_fragment(&self, _pane_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }

    fn send_enter(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    fn send_key(&self, _pane_id: &str, _key: &str) -> Result<()> {
        Ok(())
    }

    fn paste_text(&self, _pane_id: &str, _content: &str) -> Result<()> {
        Ok(())
    }

    fn get_default_shell(&self) -> Result<String> {
        Ok("/bin/sh".to_string())
    }

    fn create_handshake(&self) -> Result<Box<dyn PaneHandshake>> {
        Err(anyhow::anyhow!("not used"))
    }

    fn set_status(&self, _pane_id: &str, _icon: &str, _auto_clear_on_focus: bool) -> Result<()> {
        Ok(())
    }

    fn clear_status(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    fn ensure_status_format(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    fn split_pane(
        &self,
        _target_pane_id: &str,
        _direction: &crate::config::SplitDirection,
        _cwd: &Path,
        _size: Option<u16>,
        _percentage: Option<u8>,
        _command: Option<&str>,
    ) -> Result<String> {
        Ok("pane-2".to_string())
    }

    fn setup_panes(
        &self,
        initial_pane_id: &str,
        _panes: &[crate::config::PaneConfig],
        _working_dir: &Path,
        _options: PaneSetupOptions<'_>,
        _config: &Config,
        _task_agent: Option<&str>,
    ) -> Result<PaneSetupResult> {
        Ok(PaneSetupResult {
            focus_pane_id: initial_pane_id.to_string(),
            zoom_pane_id: None,
        })
    }

    fn instance_id(&self) -> String {
        INSTANCE.to_string()
    }

    fn get_live_pane_info(&self, pane_id: &str) -> Result<Option<LivePaneInfo>> {
        if self.fresh_fails {
            anyhow::bail!("rex timed out");
        }
        Ok(self.fresh.get(pane_id).cloned())
    }

    fn get_all_live_pane_info(&self) -> Result<HashMap<String, LivePaneInfo>> {
        Ok(self.live.clone())
    }

    fn reported_status(&self, pane_id: &str) -> Option<ReportedStatus> {
        if let Some(during_query) = self.during_query.lock().unwrap().take() {
            during_query();
        }
        self.reported.get(pane_id).copied()
    }
}
