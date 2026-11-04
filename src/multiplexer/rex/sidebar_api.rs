//! Typed Rex calls for the sidebar host. JSON stays in this module: callers
//! get layout trees and ids as plain structs.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use super::{BlockId, RexBackend, SessionId, WindowId, found, parallel_map, shell_block};
use crate::multiplexer::LivePaneInfo;

/// The label sidebar blocks carry, for people reading `rex` output. Labels
/// are not unique, so the host identifies its blocks by the ids it records.
pub(crate) const SIDEBAR_LABEL: &str = "workrex-sidebar";

/// `session.view` with the layout tree of each layer.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct View {
    pub session_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub active_window_id: Option<String>,
    #[serde(default)]
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Window {
    pub window_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused_block_id: Option<String>,
    #[serde(default)]
    pub layers: Vec<Layer>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Layer {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub layout: Option<Node>,
    #[serde(default)]
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Block {
    pub block_id: String,
    #[serde(default)]
    pub creator_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Node {
    pub rect: [f64; 4],
    #[serde(default)]
    pub block_id: Option<String>,
    #[serde(default)]
    pub split: Option<Box<Split>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Split {
    pub split_id: String,
    pub ratio: f64,
    pub before: Node,
    pub after: Node,
}

impl Node {
    /// The first block in reading order and its rect width.
    pub(crate) fn first_block(&self) -> Option<(&str, f64)> {
        match (&self.block_id, &self.split) {
            (Some(block), _) => Some((block, self.rect[2])),
            (None, Some(split)) => split.before.first_block(),
            (None, None) => None,
        }
    }
}

impl Window {
    pub(crate) fn tiled(&self) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.kind == "tiled")
    }

    pub(crate) fn block_ids(&self) -> impl Iterator<Item = &str> {
        self.layers
            .iter()
            .flat_map(|layer| layer.blocks.iter().map(|block| block.block_id.as_str()))
    }

    /// The tiled layer's root split, when the root is a split.
    pub(crate) fn root_split(&self) -> Option<&Split> {
        self.tiled()?.layout.as_ref()?.split.as_deref()
    }

    /// The node a new full-height left block splits: the root split, or the
    /// only block of a window that has no split.
    pub(crate) fn anchor(&self) -> Option<&str> {
        let node = self.tiled()?.layout.as_ref()?;
        match (&node.split, &node.block_id) {
            (Some(split), _) => Some(&split.split_id),
            (None, Some(block)) => Some(block),
            (None, None) => None,
        }
    }
}

/// A block's terminal size. A block no app has sized reports Rex's headless
/// 80x24.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BlockSize {
    pub columns: u32,
}

#[derive(Debug, Deserialize)]
struct CreatedSplit {
    block_ids: Vec<String>,
    split_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedSidebar {
    pub block_id: String,
    pub split_id: String,
}

/// The block a sidebar process runs in, from its own environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnBlock {
    pub session_id: String,
    pub block_id: String,
}

impl RexBackend {
    /// A backend for another thread that runs the same `rex` against the
    /// same server, and shares none of this one's reads.
    pub(crate) fn detached(&self) -> RexBackend {
        RexBackend {
            program: self.program.clone(),
            reuse: self.reuse,
            ..RexBackend::with_server(self.server.clone())
        }
    }

    /// A backend for the daemon's thread. The daemon reads a session because
    /// an event says it changed, so a reused read would predate the change.
    pub(crate) fn detached_for_events(&self) -> RexBackend {
        RexBackend {
            reuse: Duration::ZERO,
            ..self.detached()
        }
    }

    pub(crate) fn server_address(&self) -> Option<&str> {
        self.server.as_deref()
    }

    pub(crate) fn own_block() -> Option<OwnBlock> {
        let caller = super::Caller::from_env();
        Some(OwnBlock {
            session_id: caller.session?.0,
            block_id: caller.block?.0,
        })
    }

    pub(crate) fn sidebar_views(&self) -> Result<Vec<View>> {
        let sessions = self.sessions()?;
        let views = parallel_map(&sessions, |session| {
            self.sidebar_view(session.session_id.as_str())
        });
        Ok(views
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    pub(crate) fn sidebar_view(&self, session_id: &str) -> Result<Option<View>> {
        found(self.api("session.view", json!({ "session_id": session_id })))
    }

    pub(crate) fn block_size(&self, block: &str) -> Result<Option<BlockSize>> {
        found(self.block_call(&BlockId(block.to_string()), "size", json!({})))
    }

    pub(crate) fn boot_id(&self) -> Result<String> {
        Ok(self.whoami()?.boot_id)
    }

    /// Split a full-height block running `command` off the left of the
    /// window at `anchor`, without moving focus.
    pub(crate) fn new_sidebar_split(
        &self,
        session_id: &str,
        anchor: &str,
        ratio: f64,
        cwd: &Path,
        command: &[String],
    ) -> Result<CreatedSidebar> {
        let command: Vec<&str> = command.iter().map(String::as_str).collect();
        let mut layout = shell_block(cwd, &command);
        layout["block"]["label"] = json!(SIDEBAR_LABEL);
        let split: CreatedSplit = self
            .api(
                "session.new_split",
                json!({
                    "session_id": session_id,
                    "anchor_id": anchor,
                    "direction": "horizontal",
                    "side": "before",
                    "ratio": ratio,
                    "focus": false,
                    "layout": layout,
                }),
            )
            .map_err(anyhow::Error::from)
            .context("Failed to create the Rex sidebar block")?;
        let created = CreatedSidebar {
            block_id: split.block_ids.into_iter().next().unwrap_or_default(),
            split_id: split.split_ids.into_iter().next().unwrap_or_default(),
        };
        if created.block_id.is_empty() || created.split_id.is_empty() {
            return Err(anyhow!("Rex created no sidebar block"));
        }
        Ok(created)
    }

    /// Move the border between the sidebar and the content beside it.
    pub(crate) fn resize_sidebar(
        &self,
        session_id: &str,
        sidebar: &str,
        neighbor: &str,
        ratio: f64,
    ) -> Result<()> {
        self.api::<Value>(
            "session.resize_pane",
            json!({
                "session_id": session_id,
                "direction": "horizontal",
                "before_block_id": sidebar,
                "after_block_id": neighbor,
                "ratio": ratio,
            }),
        )?;
        Ok(())
    }

    /// Close a sidebar block. `keep_last` refuses to close a window's only
    /// block, which the caller handles by closing the window.
    pub(crate) fn close_sidebar_block(&self, session_id: &str, block: &str) -> Result<()> {
        found(self.api::<Value>(
            "block.close",
            json!({ "session_id": session_id, "block_id": block, "keep_last": true }),
        ))?;
        Ok(())
    }

    pub(crate) fn close_window_by_id(&self, session_id: &str, window_id: &str) -> Result<()> {
        self.close_window(
            &SessionId(session_id.to_string()),
            &WindowId(window_id.to_string()),
        )
    }

    /// Live pane info for every tiled terminal block in `views` except
    /// `skip`, the blocks the backend's own pane scan reads.
    /// A block that fails to answer is left out rather than failing the scan.
    pub(crate) fn sidebar_live_panes(
        &self,
        views: &[View],
        skip: &HashSet<String>,
    ) -> HashMap<String, LivePaneInfo> {
        let mut blocks = Vec::new();
        for view in views {
            for window in &view.windows {
                for block in window.tiled().into_iter().flat_map(|layer| &layer.blocks) {
                    if block.creator_name == super::TERMINAL_CREATOR
                        && !skip.contains(&block.block_id)
                    {
                        blocks.push((view, window, block.block_id.as_str()));
                    }
                }
            }
        }
        let panes = parallel_map(&blocks, |(view, window, block)| {
            let info = self.block_live_pane(view, window, block);
            if let Err(error) = &info {
                tracing::debug!(%error, block, "sidebar scan skipped a Rex block");
            }
            info.ok().flatten().map(|info| (block.to_string(), info))
        });
        panes.into_iter().flatten().collect()
    }

    fn block_live_pane(
        &self,
        view: &View,
        window: &Window,
        block: &str,
    ) -> Result<Option<LivePaneInfo>> {
        let block = BlockId(block.to_string());
        let Some(process) =
            found(self.block_call::<super::ProcessReport>(&block, "process", json!({})))?
        else {
            return Ok(None);
        };
        if process.has_exited() {
            return Ok(None);
        }
        let title = found(self.block_call::<super::TitleReport>(&block, "title", json!({})))?
            .map(|report| report.title)
            .unwrap_or_default();
        Ok(Some(LivePaneInfo {
            pid: process.child.as_ref().map(|child| child.pid),
            current_command: process.foreground.as_ref().map(|fg| fg.name.clone()),
            working_dir: process.working_dir(),
            title: (!title.is_empty()).then_some(title),
            session: Some(view.label.clone()),
            window: Some(window.label.clone()),
            session_id: Some(view.session_id.clone()),
            window_id: Some(window.window_id.clone()),
            window_index: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_SIDEBAR: &str = include_str!("../testdata/rex_sidebar_view.json");
    const BEFORE_SIDEBAR: &str = include_str!("../testdata/rex_sidebar_view_before.json");
    const MULTI_WINDOW: &str = include_str!("../testdata/rex_sidebar_view_multi.json");

    #[test]
    fn a_window_with_a_sidebar_has_it_as_the_root_split_before_side() {
        let view: View = serde_json::from_str(WITH_SIDEBAR).unwrap();
        let window = &view.windows[0];
        let root = window.root_split().unwrap();
        assert_eq!(
            root.before.block_id.as_deref(),
            Some("block:01a19e93-b76d-7c44-9a83-651cbd8da21e")
        );
        assert_eq!(root.ratio, 0.18);
        assert_eq!(
            root.after.first_block(),
            Some(("block:01a19e92-e5aa-7e82-bff4-588c27417684", 0.492))
        );
        assert_eq!(window.block_ids().count(), 3);
    }

    #[test]
    fn the_anchor_is_the_root_split_or_the_only_block() {
        let view: View = serde_json::from_str(BEFORE_SIDEBAR).unwrap();
        assert_eq!(
            view.windows[0].anchor(),
            Some("split:01a19e93-0b96-7724-a0f0-21894c01a0be")
        );

        let view: View = serde_json::from_str(MULTI_WINDOW).unwrap();
        let single = view
            .windows
            .iter()
            .find(|window| window.root_split().is_none())
            .expect("fixture has a one-block window");
        let only = single.block_ids().next().unwrap();
        assert_eq!(single.anchor(), Some(only));
        assert!(
            view.windows
                .iter()
                .any(|window| window.root_split().is_some())
        );
    }

    #[test]
    fn the_first_block_carries_its_share_of_the_window() {
        let view: View = serde_json::from_str(WITH_SIDEBAR).unwrap();
        let layout = view.windows[0].tiled().unwrap().layout.as_ref().unwrap();
        let (block, width) = layout.first_block().unwrap();
        assert_eq!(block, "block:01a19e93-b76d-7c44-9a83-651cbd8da21e");
        assert!((width - 0.18).abs() < 1e-9);
    }
}
