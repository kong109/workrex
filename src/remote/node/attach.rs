//! Native remote terminals are selected by Rex.app using their recorded session IDs.

use super::{NodeRoot, refuse, validate_id};
use crate::remote::protocol::code;
use anyhow::Result;

pub const SESSION_GONE: i32 = 103;
pub const HOSTED_ON_REX: i32 = 105;

pub fn attach(root: &NodeRoot, ex_id: &str, slot: &str) -> Result<i32> {
    validate_id(ex_id, 'x', "execution id")?;
    if !super::shell::valid_slot(slot) {
        return Err(refuse(
            code::BAD_REQUEST,
            format!("invalid session '{slot}'"),
        ));
    }
    let exists = root.workspaces().iter().any(|ws| {
        ws.execution.as_ref().is_some_and(|execution| {
            execution.ex_id == ex_id
                && execution.rex.as_ref().is_some_and(|placement| {
                    if slot.starts_with(super::shell::SHELL_SLOT) {
                        placement.shells.contains_key(slot)
                    } else {
                        slot.parse::<usize>()
                            .is_ok_and(|index| placement.sessions.contains_key(&index))
                    }
                })
        })
    });
    if exists {
        eprintln!("{ex_id}-{slot} is hosted on the node's Rex server; open it in Rex.app");
        Ok(HOSTED_ON_REX)
    } else {
        Ok(SESSION_GONE)
    }
}
