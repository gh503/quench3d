//! Bounded requests into the live editor, shared by browser automation and agents.
//!
//! Requests execute on the editor frame thread; this bridge never owns a second world.
use crate::{agent::AgentPanel, commands, doc::Doc};
use quench3d::Engine;
use serde_json::{json, Value};
use std::{cell::RefCell, collections::VecDeque};

const CAPACITY: usize = 32;
#[derive(Default)]
struct Bridge {
    next: u32,
    pending: VecDeque<(u32, Value)>,
    results: VecDeque<(u32, String)>,
}
thread_local! { static BRIDGE: RefCell<Bridge> = RefCell::new(Bridge::default()); }

/// Queue a JSON request: `start`, `cancel`, `status`, `inspect`, or `command`.
/// Returns an ID used with [`take_result`]. At most 32 pending or unread results
/// can exist. Read completed results to release capacity; results are never evicted.
pub fn enqueue(request: &str) -> Result<u32, String> {
    if request.len() > 65_536 {
        return Err("request exceeds 64 KiB".into());
    }
    let request: Value = serde_json::from_str(request).map_err(|e| e.to_string())?;
    if !request.is_object() {
        return Err("request must be an object".into());
    }
    BRIDGE.with(|cell| {
        let mut bridge = cell.borrow_mut();
        if bridge.pending.len() + bridge.results.len() >= CAPACITY {
            return Err("editor request queue is full".into());
        }
        bridge.next = bridge.next.checked_add(1).ok_or("request ID exhausted")?;
        let id = bridge.next;
        bridge.pending.push_back((id, request));
        Ok(id)
    })
}

/// Retrieve and remove one completed response. Unknown/pending IDs return `None`.
pub fn take_result(id: u32) -> Option<String> {
    BRIDGE.with(|cell| {
        let mut bridge = cell.borrow_mut();
        let index = bridge.results.iter().position(|(key, _)| *key == id)?;
        bridge.results.remove(index).map(|(_, result)| result)
    })
}

pub(crate) fn poll(engine: &mut Engine, doc: &mut Doc, agent: &mut AgentPanel) {
    let request = BRIDGE.with(|cell| cell.borrow_mut().pending.pop_front());
    let Some((id, request)) = request else {
        return;
    };
    let response = match request["action"].as_str() {
        Some("start") => match request["prompt"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(prompt) => {
                agent.open = true;
                agent.start_goal(prompt);
                json!({"ok":true})
            }
            None => json!({"ok":false,"error":"prompt must be nonempty"}),
        },
        Some("cancel") => {
            agent.cancel();
            json!({"ok":true})
        }
        Some("status") => json!({"ok":true,"agent":agent.snapshot(),"frame":engine.frame_count}),
        Some("inspect") => commands::dispatch(engine, doc, "inspect_scene", json!({})),
        Some("command") => match request["name"].as_str() {
            Some(name) => commands::dispatch(engine, doc, name, request["arguments"].clone()),
            None => json!({"ok":false,"error":"command name required"}),
        },
        _ => json!({"ok":false,"error":"unknown editor bridge action"}),
    };
    BRIDGE.with(|cell| {
        let mut bridge = cell.borrow_mut();
        bridge.results.push_back((id, response.to_string()));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_rejects_invalid_input_and_preserves_request_order() {
        BRIDGE.with(|cell| *cell.borrow_mut() = Bridge::default());
        assert!(enqueue("[]").is_err());
        assert!(enqueue("{broken").is_err());
        for expected in 1..=CAPACITY as u32 {
            assert_eq!(enqueue(r#"{"action":"status"}"#).unwrap(), expected);
        }
        assert!(enqueue(r#"{"action":"status"}"#).is_err());
        BRIDGE.with(|cell| {
            let mut bridge = cell.borrow_mut();
            for expected in 1..=CAPACITY as u32 {
                assert_eq!(bridge.pending.pop_front().unwrap().0, expected);
                bridge
                    .results
                    .push_back((expected, format!("result-{expected}")));
            }
        });
        assert!(
            enqueue(r#"{"action":"status"}"#).is_err(),
            "unread results retain capacity"
        );
        assert_eq!(take_result(1).as_deref(), Some("result-1"));
        assert_eq!(
            enqueue(r#"{"action":"status"}"#).unwrap(),
            CAPACITY as u32 + 1
        );
        assert_eq!(
            take_result(2).as_deref(),
            Some("result-2"),
            "new requests cannot evict existing results"
        );
        assert!(take_result(999).is_none());
    }
}
