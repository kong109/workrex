//! Agent list management, navigation, sorting, filtering, and display helpers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::Style;

use crate::github::{CheckSummary, PrSummary};
use crate::multiplexer::rex::{AppRefused, LayerHint};
use crate::multiplexer::{AgentPane, AgentStatus, Multiplexer, RemoteAgent};

use super::DashboardTab;

use super::super::agent;
use super::super::ansi;
use super::super::settings::{load_last_pane_id, save_hide_stale, save_last_pane_id};
use super::super::sort::SortMode;
use super::super::spinner::SPINNER_FRAMES;
use super::App;
use crate::command::pane_history::pane_to_remember;

/// The footer text for a failed jump or peek. Only errors written for the
/// user reach the footer: Rex's refusal and the layer's hints. Any other
/// failure stays out of it, as a missed switch always has.
fn footer_message(err: &anyhow::Error) -> Option<String> {
    if err.is::<AppRefused>() || err.is::<LayerHint>() {
        Some(err.to_string())
    } else {
        tracing::warn!(error = %format!("{err:#}"), "dashboard: switch failed");
        None
    }
}

/// Selected agent pane target for multiplexer operations.
#[derive(Clone)]
pub(crate) struct PaneTarget {
    pub pane_id: String,
    pub window_name: String,
    pub remote: Option<RemoteAgent>,
    pub agent_kind: Option<String>,
}

impl PaneTarget {
    fn from_agent(agent: &AgentPane) -> Self {
        Self {
            pane_id: agent.pane_id.clone(),
            window_name: agent.window_name.clone(),
            remote: agent.remote.clone(),
            agent_kind: agent.agent_kind.clone(),
        }
    }

    pub(crate) fn send_keys(
        &self,
        mux: &dyn Multiplexer,
        keys: &str,
        default_agent: Option<&str>,
    ) -> anyhow::Result<()> {
        if let Some(remote) = &self.remote {
            return crate::remote::agents::send_keys_to_agent(
                &remote.at,
                keys,
                self.agent_kind.as_deref(),
            );
        }
        if mux.requires_focus_for_input() {
            mux.switch_to_pane(&self.pane_id, Some(&self.window_name))?;
        }
        mux.send_keys_to_agent(
            &self.pane_id,
            keys,
            self.agent_kind.as_deref().or(default_agent),
        )
    }

    fn send_key(&self, mux: &dyn Multiplexer, key: &str) -> anyhow::Result<()> {
        match &self.remote {
            Some(remote) => crate::remote::agents::send_key(&remote.at, key),
            None => mux.send_key(&self.pane_id, key),
        }
    }

    fn paste_text(&self, mux: &dyn Multiplexer, text: &str) -> anyhow::Result<()> {
        match &self.remote {
            Some(remote) => crate::remote::agents::paste_text(&remote.at, text),
            None => mux.paste_text(&self.pane_id, text),
        }
    }

    fn kill(&self, mux: &dyn Multiplexer) -> anyhow::Result<()> {
        match &self.remote {
            Some(remote) => crate::remote::agents::kill_pane(&remote.at),
            None => mux.kill_pane(&self.pane_id),
        }
    }
}

fn should_close_after_jump(mux_exits_on_jump: bool, close_on_jump: bool) -> bool {
    mux_exits_on_jump && close_on_jump
}

impl App {
    /// Apply name and stale filters to the cached agent list, sort, and restore selection.
    /// This is fast (in-memory only) and safe to call on every filter keystroke.
    pub fn apply_filters(&mut self) {
        self.agents = self.all_agents.clone();

        // Apply name filter if active
        if !self.filter_text.is_empty() {
            let filter_lower = self.filter_text.to_lowercase();
            let window_prefix = self.config.window_prefix();
            self.agents.retain(|a| {
                let project = Self::extract_project_name(a).to_lowercase();
                let (worktree, _) = agent::extract_worktree_name(
                    &a.session,
                    &a.window_name,
                    window_prefix,
                    &a.path,
                );
                let worktree = match &a.remote {
                    Some(remote) => crate::remote::agents::label(&worktree, &remote.at.remote),
                    None => worktree,
                };
                let worktree_lower = worktree.to_lowercase();
                project.contains(&filter_lower) || worktree_lower.contains(&filter_lower)
            });
        }

        // Filter out stale agents if hide_stale is enabled
        if self.hide_stale {
            let threshold = self.stale_threshold_secs;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let sleeping = &self.sleeping_pane_ids;
            let interrupted = &self.interrupted_pane_ids;
            self.agents.retain(|agent| {
                !crate::agent_staleness::is_stale(
                    agent.activity_ts(),
                    agent.status,
                    now,
                    threshold,
                    sleeping.contains(&agent.pane_id),
                    interrupted.contains(&agent.pane_id),
                )
            });
        }

        self.sort_agents();

        // Restore selection by pane_id to follow the item across reorders
        if let Some(ref pane_id) = self.selected_pane_id {
            if let Some(new_idx) = self.agents.iter().position(|a| &a.pane_id == pane_id) {
                self.table_state.select(Some(new_idx));
            } else {
                self.selected_pane_id = None;
                if self.agents.is_empty() {
                    self.table_state.select(None);
                } else if let Some(selected) = self.table_state.selected() {
                    if selected >= self.agents.len() {
                        self.table_state.select(Some(self.agents.len() - 1));
                    }
                    if let Some(idx) = self.table_state.selected() {
                        self.selected_pane_id = self.agents.get(idx).map(|a| a.pane_id.clone());
                    }
                }
            }
        } else if let Some(selected) = self.table_state.selected() {
            if selected >= self.agents.len() {
                self.table_state.select(if self.agents.is_empty() {
                    None
                } else {
                    Some(self.agents.len() - 1)
                });
            }
            if let Some(idx) = self.table_state.selected() {
                self.selected_pane_id = self.agents.get(idx).map(|a| a.pane_id.clone());
            }
        }

        // Fallback: if nothing is selected but agents exist, select the first one.
        // This handles the case where filtering produced zero matches (clearing selection)
        // and then results reappear.
        if self.selected_pane_id.is_none()
            && self.table_state.selected().is_none()
            && !self.agents.is_empty()
        {
            self.table_state.select(Some(0));
            self.selected_pane_id = self.agents.first().map(|a| a.pane_id.clone());
        }

        self.update_preview();
    }

    /// Rex block IDs contain time-ordered UUIDs, so their text is a stable order.
    fn block_order_key(pane_id: &str) -> String {
        pane_id.to_owned()
    }

    /// Sort agents based on the current sort mode
    fn sort_agents(&mut self) {
        let stale_threshold = self.stale_threshold_secs;
        let sleeping = &self.sleeping_pane_ids;
        let interrupted = &self.interrupted_pane_ids;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Helper closure to get status priority (lower = higher priority)
        let get_priority = |agent: &AgentPane| -> u8 {
            let is_stale = crate::agent_staleness::is_stale(
                agent.activity_ts(),
                agent.status,
                now,
                stale_threshold,
                sleeping.contains(&agent.pane_id),
                interrupted.contains(&agent.pane_id),
            );

            if is_stale {
                return 4; // Stale: lowest priority
            }

            match agent.status {
                Some(AgentStatus::Waiting) => 0, // Waiting: needs input
                Some(AgentStatus::Done) => 1,    // Done: needs review
                Some(AgentStatus::Working) => 2, // Working: no action needed
                None => 3,                       // Unknown/other: lowest priority
            }
        };

        // Helper closure to get activity age (lower = more recent)
        let get_activity_age = |agent: &AgentPane| -> u64 {
            agent
                .activity_ts()
                .map(|ts| now.saturating_sub(ts))
                .unwrap_or(u64::MAX)
        };

        // Helper closure to get numeric pane_id for stable ordering
        let pane_num = |agent: &AgentPane| Self::block_order_key(&agent.pane_id);

        // Use sort_by_cached_key for better performance (calls key fn O(N) times vs O(N log N))
        // Include pane_id as final tiebreaker for stable ordering within groups
        match self.sort_mode {
            SortMode::Priority => {
                // Sort by priority, then by elapsed time (most recent first), then by pane_id
                self.agents
                    .sort_by_cached_key(|a| (get_priority(a), get_activity_age(a), pane_num(a)));
            }
            SortMode::Project => {
                // Sort by project name first, then by status priority within each project
                self.agents.sort_by_cached_key(|a| {
                    (Self::extract_project_name(a), get_priority(a), pane_num(a))
                });
            }
            SortMode::Recency => {
                self.agents
                    .sort_by_cached_key(|a| (get_activity_age(a), pane_num(a)));
            }
            SortMode::Natural => {
                self.agents.sort_by_cached_key(pane_num);
            }
        }
    }

    pub fn cycle_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.next();
        self.sort_mode.save();
        self.sort_agents();
    }

    /// Toggle between showing all agents or only the current session's agents
    pub fn toggle_scope_mode(&mut self) {
        self.scope_mode = self.scope_mode.toggle();
        self.scope_mode.save();
        self.refresh();
    }

    /// Toggle hiding stale agents
    pub fn toggle_stale_filter(&mut self) {
        self.hide_stale = !self.hide_stale;
        save_hide_stale(self.hide_stale);
        self.refresh();
    }

    pub fn select_agent(&mut self, index: usize) {
        if index >= self.agents.len() {
            return;
        }
        self.table_state.select(Some(index));
        self.selected_pane_id = self.agents.get(index).map(|agent| agent.pane_id.clone());
        self.update_preview();
    }

    pub fn next(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.agents.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.select_agent(i);
    }

    pub fn previous(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.agents.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.select_agent(i);
    }

    /// Switch to a pane and track the previous pane for toggle feature.
    /// This is the single source of truth for all pane switching.
    pub(crate) fn switch_to_pane_and_track(&mut self, target_pane_id: &str) {
        // Get the REAL current pane from the multiplexer (not UI state)
        let current_pane = self.mux.active_pane_id();

        // Attempt the switch first - only update state on success
        // Look up window_name for the target pane (needed to select its Rex window)
        let window_hint = self
            .agents
            .iter()
            .find(|a| a.pane_id == target_pane_id)
            .map(|a| a.window_name.as_str());
        if let Err(err) = self.mux.switch_to_pane(target_pane_id, window_hint) {
            self.show_switch_error(&err);
            return;
        }

        if self.should_close_after_jump() {
            self.should_jump = true;
        }

        if let Some(current) = pane_to_remember(current_pane.as_deref(), target_pane_id) {
            self.last_pane_id = Some(current.to_string());
            save_last_pane_id(current);
        }
    }

    fn show_switch_error(&mut self, err: &anyhow::Error) {
        if let Some(msg) = footer_message(err) {
            self.hold_status(msg);
        }
    }

    pub(crate) fn should_close_after_jump(&self) -> bool {
        should_close_after_jump(
            self.mux.should_exit_on_jump(),
            self.config.dashboard.close_on_jump(),
        )
    }

    pub fn jump_to_selected(&mut self) {
        let Some(target) = self.selected_agent_target() else {
            return;
        };
        if let Some(remote) = &target.remote {
            self.jump_to_remote_and_track(remote);
        } else {
            self.switch_to_pane_and_track(&target.pane_id);
        }
    }

    fn jump_to_remote_and_track(&mut self, remote: &RemoteAgent) {
        let current = self.mux.active_pane_id();
        if self.open_remote(remote) {
            if self.should_close_after_jump() {
                self.should_jump = true;
            }
            if let Some(current) = current {
                self.last_pane_id = Some(current.clone());
                save_last_pane_id(&current);
            }
        }
    }

    /// Select the recorded native remote session and block, reporting failure.
    fn open_remote(&mut self, remote: &RemoteAgent) -> bool {
        match crate::remote::agents::open_window(remote) {
            Ok(()) => {
                self.refresh();
                true
            }
            Err(err) => {
                self.hold_status(format!("open {}: {err:#}", remote.handle));
                false
            }
        }
    }

    pub(crate) fn selected_agent_target(&self) -> Option<PaneTarget> {
        self.agents
            .get(self.table_state.selected()?)
            .map(PaneTarget::from_agent)
    }

    pub(crate) fn send_keys_to_selected_agent(&mut self, keys: &str) {
        let Some(target) = self.selected_agent_target() else {
            return;
        };
        if let Err(err) = target.send_keys(self.mux.as_ref(), keys, self.config.agent.as_deref()) {
            self.hold_status(format!("send command: {err:#}"));
        }
    }

    pub fn jump_to_index(&mut self, index: usize) {
        if index < self.agents.len() {
            self.table_state.select(Some(index));
            self.selected_pane_id = self.agents.get(index).map(|a| a.pane_id.clone());
            self.jump_to_selected();
        }
    }

    /// Jump using current row metadata, including remote execution identity.
    pub fn jump_to_last(&mut self) {
        self.last_pane_id = load_last_pane_id();
        let Some(last_id) = self.last_pane_id.clone() else {
            return;
        };
        if let Some(idx) = self.agents.iter().position(|a| a.pane_id == last_id) {
            self.table_state.select(Some(idx));
            self.selected_pane_id = Some(last_id.clone());
        }
        let remote = self
            .all_agents
            .iter()
            .chain(&self.agents)
            .find(|agent| agent.pane_id == last_id)
            .and_then(|agent| agent.remote.clone());
        if let Some(remote) = remote {
            self.jump_to_remote_and_track(&remote);
        } else if last_id.starts_with("%remote-") {
            self.hold_status("The last remote agent is no longer available".to_string());
        } else {
            self.switch_to_pane_and_track(&last_id);
        }
    }

    pub fn peek_selected(&mut self) {
        let Some(target) = self.selected_agent_target() else {
            return;
        };
        if let Some(remote) = &target.remote {
            self.open_remote(remote);
        } else if let Err(err) = self
            .mux
            .peek_pane(&target.pane_id, Some(&target.window_name))
        {
            self.show_switch_error(&err);
        }
    }

    /// Confirm a working agent's exact target before killing it.
    pub fn kill_selected(&mut self) {
        let Some(target) = self.selected_agent_target() else {
            return;
        };
        if self
            .agents
            .get(self.table_state.selected().unwrap())
            .is_some_and(|agent| agent.status == Some(AgentStatus::Working))
        {
            self.pending_kill_target = Some(target);
        } else {
            self.do_kill(&target);
        }
    }

    /// Execute the pending kill confirmation.
    pub fn confirm_kill(&mut self) {
        if let Some(target) = self.pending_kill_target.clone()
            && self.do_kill(&target)
        {
            self.pending_kill_target = None;
        }
    }

    /// Kill a pane and remove it from the agent list.
    fn do_kill(&mut self, target: &PaneTarget) -> bool {
        if let Err(err) = target.kill(self.mux.as_ref()) {
            self.hold_status(format!("kill agent: {err:#}"));
            return false;
        }
        // Only remove the execution that was successfully killed. A refresh may
        // already have replaced the row while its confirmation was open.
        let matches = |agent: &AgentPane| {
            agent.pane_id == target.pane_id
                && agent.remote.as_ref().map(|remote| &remote.at)
                    == target.remote.as_ref().map(|remote| &remote.at)
        };
        let selected = self.table_state.selected().unwrap_or(0);
        self.agents.retain(|a| !matches(a));
        self.all_agents.retain(|a| !matches(a));
        if self.agents.is_empty() {
            self.table_state.select(None);
            self.selected_pane_id = None;
        } else {
            let new_idx = selected.min(self.agents.len() - 1);
            self.table_state.select(Some(new_idx));
            self.selected_pane_id = self.agents.get(new_idx).map(|a| a.pane_id.clone());
        }
        self.preview_pane_id = None;
        self.update_preview();
        true
    }

    /// Send a key to the selected agent's pane
    pub fn send_key_to_selected(&mut self, key: &str) {
        if let Some(target) = self.selected_agent_target()
            && let Err(err) = target.send_key(self.mux.as_ref(), key)
        {
            self.hold_status(format!("send key: {err:#}"));
        }
    }

    /// Paste text to the selected agent's pane.
    pub fn paste_text_to_selected(&mut self, text: &str) {
        if let Some(target) = self.selected_agent_target()
            && let Err(err) = target.paste_text(self.mux.as_ref(), text)
        {
            self.hold_status(format!("paste text: {err:#}"));
        }
    }

    pub fn format_duration(&self, secs: u64) -> String {
        agent::format_duration(secs)
    }

    pub fn is_stale(&self, agent: &AgentPane) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        crate::agent_staleness::is_stale(
            agent.activity_ts(),
            agent.status,
            now,
            self.stale_threshold_secs,
            self.sleeping_pane_ids.contains(&agent.pane_id),
            self.interrupted_pane_ids.contains(&agent.pane_id),
        )
    }

    pub fn get_elapsed(&self, agent: &AgentPane) -> Option<u64> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        agent::elapsed_secs(agent.status_ts, now)
    }

    pub fn get_status_display(&self, agent: &AgentPane) -> Vec<(String, Style)> {
        let is_stale = self.is_stale(agent);

        // Map status enum to icon and color
        let (icon, base_color, is_working) = match agent.status {
            Some(AgentStatus::Working) => {
                (self.config.status_icons.working(), self.palette.info, true)
            }
            Some(AgentStatus::Waiting) => (
                self.config.status_icons.waiting(),
                self.palette.accent,
                false,
            ),
            Some(AgentStatus::Done) => {
                (self.config.status_icons.done(), self.palette.success, false)
            }
            None => ("", self.palette.text, false),
        };

        let base_style = Style::default().fg(base_color);
        let mut spans = ansi::parse_terminal_styles(icon, base_style);
        let disconnected = agent
            .remote
            .as_ref()
            .is_some_and(|remote| !remote.connected);

        if disconnected {
            // The last status the remote reported, which may have moved on.
            let dimmed = Style::default().fg(self.palette.dimmed);
            for span in &mut spans {
                span.1 = dimmed;
            }
        } else if is_stale {
            // Override all styling for stale agents
            let dimmed = Style::default().fg(self.palette.dimmed);
            for span in &mut spans {
                span.1 = dimmed;
            }
            spans.push((" \u{f051b}".to_string(), dimmed));
        } else if is_working {
            // Add animated spinner when agent is working
            let spinner = SPINNER_FRAMES[self.spinner_frame as usize];
            spans.push((format!(" {}", spinner), base_style));
        }

        spans
    }

    /// Extract the worktree name from an agent.
    /// Returns (worktree_name, is_main) where is_main indicates if this is the main worktree.
    pub fn extract_worktree_name(&self, agent_pane: &AgentPane) -> (String, bool) {
        let (name, is_main) = agent::extract_worktree_name(
            &agent_pane.session,
            &agent_pane.window_name,
            self.config.window_prefix(),
            &agent_pane.path,
        );
        match &agent_pane.remote {
            Some(remote) => (
                crate::remote::agents::label(&name, &remote.at.remote),
                is_main,
            ),
            None => (name, is_main),
        }
    }

    pub fn extract_project_name(agent_pane: &AgentPane) -> String {
        agent::extract_project_name(&agent_pane.path)
    }

    /// Get PR info for an agent by looking up its branch in PR statuses
    pub fn get_pr_for_agent(&self, agent: &AgentPane) -> Option<&PrSummary> {
        let repo_root = self.repo_keys.get(&agent.path)?;
        let git_status = self.git_statuses.get(&agent.path)?;
        let branch = git_status.branch.as_ref()?;
        // Don't show PRs for main/master - you merge INTO main, not FROM it
        if branch == "main" || branch == "master" {
            return None;
        }
        self.pr_statuses.get(repo_root)?.get(branch)
    }

    pub fn get_checks_for_agent(&self, agent: &AgentPane) -> Option<&CheckSummary> {
        let repo_root = self.repo_keys.get(&agent.path)?;
        let branch = self.git_statuses.get(&agent.path)?.branch.as_ref()?;
        self.check_statuses
            .get(repo_root)?
            .get(branch)
            .filter(|summary| summary.should_display_for_branch(branch))
    }

    pub fn get_checks_for_worktree(
        &self,
        worktree: &crate::workflow::types::WorktreeInfo,
    ) -> Option<&CheckSummary> {
        self.check_statuses
            .get(&worktree.repo_key)?
            .get(&worktree.branch)
            .filter(|summary| summary.should_display_for_branch(&worktree.branch))
    }

    /// Whether a PR fetch is currently in progress
    pub fn is_pr_fetching(&self) -> bool {
        self.is_pr_fetching.load(Ordering::Relaxed)
    }

    /// Whether any agent has a matching PR or GitHub checks.
    pub fn has_any_github_status(&self) -> bool {
        self.agents.iter().any(|agent| {
            self.get_pr_for_agent(agent).is_some() || self.get_checks_for_agent(agent).is_some()
        })
    }

    /// Get PR statuses for caching
    pub fn pr_statuses(&self) -> &HashMap<PathBuf, HashMap<String, PrSummary>> {
        &self.pr_statuses
    }

    pub fn check_statuses(&self) -> &HashMap<PathBuf, HashMap<String, CheckSummary>> {
        &self.check_statuses
    }

    /// Open the PR associated with the selected agent or worktree in the browser.
    pub fn open_pr_for_selected(&mut self) {
        self.open_pr_url(|url| url.to_string());
    }

    /// Open the PR checks page for the selected agent or worktree in the browser.
    pub fn open_pr_checks_for_selected(&mut self) {
        self.open_pr_url(|url| format!("{url}/checks"));
    }

    /// Open a URL derived from the selected item's PR URL.
    fn open_pr_url(&mut self, make_url: impl FnOnce(&str) -> String) {
        let pr = match self.active_tab {
            DashboardTab::Agents => self
                .table_state
                .selected()
                .and_then(|i| self.agents.get(i))
                .and_then(|agent| self.get_pr_for_agent(agent))
                .cloned(),
            DashboardTab::Worktrees => self
                .worktree_table_state
                .selected()
                .and_then(|i| self.worktrees.get(i))
                .and_then(|wt| wt.pr_info.clone()),
        };

        match pr {
            Some(ref pr) if pr.url.is_some() => {
                let url = make_url(pr.url.as_ref().unwrap());
                if let Err(e) = open_url(&url) {
                    self.status_message = Some((
                        format!("Failed to open browser: {e}"),
                        std::time::Instant::now(),
                    ));
                }
            }
            Some(_) => {
                self.status_message = Some((
                    "PR URL not available yet".to_string(),
                    std::time::Instant::now(),
                ));
            }
            None => {
                self.status_message = Some((
                    "No PR found for selected item".to_string(),
                    std::time::Instant::now(),
                ));
            }
        }
    }

    /// Open the remote servers guide and retire the tip. When no browser
    /// opens, the tip stays and the footer shows the address instead. Does
    /// nothing unless the tip is on screen.
    pub fn open_remote_guide(&mut self) {
        if super::super::mouse::visible_remote_tip_footer(self).is_none() {
            return;
        }
        let url = crate::tips::REMOTE_GUIDE_URL;
        let message = match open_url(url) {
            Ok(()) => {
                self.show_remote_tip = false;
                crate::tips::dismiss_remote_tip();
                format!("Opened {url}")
            }
            Err(e) => format!("Failed to open browser: {e}. Guide: {url}"),
        };
        self.status_message = Some((message, std::time::Instant::now()));
    }

    /// Count this session toward the remote servers tip's view limit the
    /// first time the footer draws it.
    pub fn note_remote_tip_drawn(&mut self) {
        if !self.remote_tip_counted {
            self.remote_tip_counted = true;
            crate::tips::record_remote_tip_view();
        }
    }

    /// Hide the remote servers tip for good. Does nothing unless the tip is
    /// on screen.
    pub fn dismiss_remote_tip(&mut self) {
        if super::super::mouse::visible_remote_tip_footer(self).is_none() {
            return;
        }
        self.show_remote_tip = false;
        crate::tips::dismiss_remote_tip();
        self.status_message = Some((
            format!(
                "Remote servers tip hidden. Guide: {}",
                crate::tips::REMOTE_GUIDE_URL
            ),
            std::time::Instant::now(),
        ));
    }
}

/// Open `url` in the default browser.
fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(target_os = "macos"))]
    let cmd = "xdg-open";

    std::process::Command::new(cmd).arg(url).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{AppRefused, footer_message, should_close_after_jump};

    #[test]
    fn a_refused_rex_jump_shows_the_fix() {
        assert_eq!(
            footer_message(&AppRefused.into()),
            Some(AppRefused.to_string())
        );
    }

    #[test]
    fn a_switch_error_not_written_for_the_user_leaves_the_footer_empty() {
        let err = anyhow::anyhow!(r#"Rex session selection failed"#);
        assert_eq!(footer_message(&err), None);
    }

    #[test]
    fn jump_closes_only_when_config_and_multiplexer_allow_it() {
        assert!(should_close_after_jump(true, true));
        assert!(!should_close_after_jump(true, false));
        assert!(!should_close_after_jump(false, true));
        assert!(!should_close_after_jump(false, false));
    }
}
