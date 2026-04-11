//! Integration tests for the Phase 4a routing primitives:
//! `AgentRegistry` and `MessageQueue`.
//!
//! Verifies the public crate surface (the bits operators or the
//! tunnel layer touch) plus the basic broker semantics: register,
//! list, describe, drain on reconnect, capacity overflow.

use std::sync::Arc;

use overacp_server::auth::Claims;
use overacp_server::registry::{AgentEntry, QueueError};
use overacp_server::{AgentRegistry, AppState, MessageQueue, StaticJwtAuthenticator};
use tokio::sync::mpsc;
use uuid::Uuid;

fn fresh_state() -> AppState {
    AppState::new(Arc::new(StaticJwtAuthenticator::new("k", "overacp")))
}

fn live_entry() -> (mpsc::UnboundedReceiver<String>, AgentEntry, Claims) {
    let agent_id = Uuid::new_v4();
    let claims = Claims::agent(agent_id, Some(Uuid::new_v4()), 60, "overacp");
    let (tx, rx) = mpsc::unbounded_channel();
    (rx, AgentEntry::new(tx, claims.clone()), claims)
}

#[test]
fn registry_default_is_exposed_via_appstate() {
    let state = fresh_state();
    assert!(state.registry.list().is_empty());
    assert_eq!(state.message_queue.capacity(), 64);
}

#[test]
fn register_then_describe_returns_connected() {
    let registry = AgentRegistry::new();
    let (_rx, entry, claims) = live_entry();
    let agent_id = claims.sub;

    registry.register(agent_id, entry);
    let desc = registry.describe(agent_id).expect("describe");
    assert!(desc.connected);
    assert_eq!(desc.user, claims.user);
    assert!(desc.uptime_secs.is_some());
}

#[test]
fn register_unregister_round_trip() {
    let registry = AgentRegistry::new();
    let (_rx, entry, claims) = live_entry();
    let agent_id = claims.sub;

    registry.register(agent_id, entry);
    assert!(registry.is_connected(agent_id));
    registry.unregister(agent_id);
    assert!(!registry.is_connected(agent_id));

    // Still findable in the recently-disconnected log.
    let desc = registry.describe(agent_id).expect("recent");
    assert!(!desc.connected);
}

#[test]
fn list_includes_both_connected_and_recent() {
    let registry = AgentRegistry::new();
    let (_rx1, entry1, c1) = live_entry();
    let (_rx2, entry2, c2) = live_entry();
    registry.register(c1.sub, entry1);
    registry.register(c2.sub, entry2);
    registry.unregister(c1.sub);

    let listed = registry.list();
    assert_eq!(listed.len(), 2);
    let connected_ids: Vec<Uuid> = listed
        .iter()
        .filter(|d| d.connected)
        .map(|d| d.agent_id)
        .collect();
    let disconnected_ids: Vec<Uuid> = listed
        .iter()
        .filter(|d| !d.connected)
        .map(|d| d.agent_id)
        .collect();
    assert_eq!(connected_ids, vec![c2.sub]);
    assert_eq!(disconnected_ids, vec![c1.sub]);
}

#[test]
fn message_queue_full_error_is_re_exported_from_crate_root() {
    // Smoke check that the public crate surface exposes both the
    // queue type and its error variant. Per-method behaviour
    // (push/drain/overflow ordering) is covered by the queue's
    // own unit tests in registry/queue.rs.
    let q = MessageQueue::new(1);
    let id = Uuid::new_v4();
    q.push(id, "a".into()).unwrap();
    let err = q.push(id, "b".into()).unwrap_err();
    let QueueError::Full { agent_id, capacity } = err;
    assert_eq!(agent_id, id);
    assert_eq!(capacity, 1);
}

#[test]
fn drain_on_reconnect_via_appstate() {
    // Smoke: simulate the "agent disconnected, REST pushes, agent
    // reconnects, drain happens" loop using the public surface.
    let state = fresh_state();
    let agent_id = Uuid::new_v4();

    // 1. Push two notifications while disconnected.
    state.message_queue.push(agent_id, "first".into()).unwrap();
    state.message_queue.push(agent_id, "second".into()).unwrap();
    assert_eq!(state.message_queue.len(agent_id), 2);

    // 2. Reconnect: register an entry, then drain the queue and
    //    forward to the entry.
    let (mut rx, entry, claims) = live_entry();
    // Use the test agent_id, not the live_entry random one.
    let claims = Claims::agent(agent_id, claims.user, 60, "overacp");
    let entry = AgentEntry::new(entry.tx.clone(), claims);
    state.registry.register(agent_id, entry);

    let buffered = state.message_queue.drain(agent_id);
    let registered = state.registry.get(agent_id).expect("registered");
    for frame in buffered {
        registered.tx.send(frame).unwrap();
    }

    // 3. The reconnecting agent receives both buffered frames in
    //    order before any live traffic. The channel is already
    //    populated synchronously by the for-loop above, so
    //    `try_recv` is sufficient — no need to spin up tokio.
    assert_eq!(rx.try_recv().ok(), Some("first".to_string()));
    assert_eq!(rx.try_recv().ok(), Some("second".to_string()));
    assert!(state.message_queue.is_empty(agent_id));
}
