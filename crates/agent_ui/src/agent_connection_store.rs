use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use acp_thread::{AgentConnection, LoadError};
use agent_servers::AcpConnection;
use agent_servers::{AgentServer, AgentServerDelegate};
use anyhow::Result;
use collections::HashMap;
use futures::{FutureExt, future::Shared};
use gpui::{App, AppContext, Context, Entity, EventEmitter, SharedString, Subscription, Task};

use project::{AgentServerStore, AgentServersUpdated, Project};
use watch::Receiver;

use crate::Agent;

const STABLE_CONNECTION_DURATION: Duration = Duration::from_secs(30);
const INITIAL_RESTART_DELAY: Duration = Duration::from_secs(1);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);

pub enum AgentConnectionEntry {
    Connecting {
        connect_task: Shared<Task<Result<AgentConnectedState, LoadError>>>,
    },
    Connected(AgentConnectedState),
    Error {
        error: LoadError,
    },
}

#[derive(Clone)]
pub struct AgentConnectedState {
    pub connection: Rc<dyn AgentConnection>,
    connected_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
}

impl AgentConnectionEntry {
    pub fn wait_for_connection(&self) -> Shared<Task<Result<AgentConnectedState, LoadError>>> {
        match self {
            AgentConnectionEntry::Connecting { connect_task } => connect_task.clone(),
            AgentConnectionEntry::Connected(state) => Task::ready(Ok(state.clone())).shared(),
            AgentConnectionEntry::Error { error } => Task::ready(Err(error.clone())).shared(),
        }
    }

    pub fn status(&self) -> AgentConnectionStatus {
        match self {
            AgentConnectionEntry::Connecting { .. } => AgentConnectionStatus::Connecting,
            AgentConnectionEntry::Connected(_) => AgentConnectionStatus::Connected,
            AgentConnectionEntry::Error { .. } => AgentConnectionStatus::Disconnected,
        }
    }
}

pub enum AgentConnectionEntryEvent {
    NewVersionAvailable(SharedString),
    LoadingStatusChanged(Option<SharedString>),
}

impl EventEmitter<AgentConnectionEntryEvent> for AgentConnectionEntry {}

#[derive(Clone)]
pub struct ActiveAcpConnection {
    pub agent_id: project::AgentId,
    pub connection: Rc<AcpConnection>,
}

pub struct AgentConnectionStore {
    project: Entity<Project>,
    entries: HashMap<Agent, Entity<AgentConnectionEntry>>,
    restart_backoffs: HashMap<Agent, ConnectionRestartBackoff>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct ConnectionRestartBackoff {
    consecutive_failures: u32,
    retry_at: Option<Instant>,
}

impl AgentConnectionStore {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let agent_server_store = project.read(cx).agent_server_store().clone();
        let subscription = cx.subscribe(&agent_server_store, Self::handle_agent_servers_updated);
        Self {
            project,
            entries: HashMap::default(),
            restart_backoffs: HashMap::default(),
            _subscriptions: vec![subscription],
        }
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    pub fn entry(&self, key: &Agent) -> Option<&Entity<AgentConnectionEntry>> {
        self.entries.get(key)
    }

    pub fn connection_status(&self, key: &Agent, cx: &App) -> AgentConnectionStatus {
        self.entries
            .get(key)
            .map(|entry| entry.read(cx).status())
            .unwrap_or(AgentConnectionStatus::Disconnected)
    }

    pub fn agent_version(&self, key: &Agent, cx: &App) -> Option<SharedString> {
        match self.entries.get(key)?.read(cx) {
            AgentConnectionEntry::Connected(state) => state.connection.agent_version(),
            AgentConnectionEntry::Connecting { .. } | AgentConnectionEntry::Error { .. } => None,
        }
    }

    pub fn active_acp_connections(&self, cx: &App) -> Vec<ActiveAcpConnection> {
        self.entries
            .values()
            .filter_map(|entry| match entry.read(cx) {
                AgentConnectionEntry::Connected(state) => state
                    .connection
                    .clone()
                    .downcast::<AcpConnection>()
                    .map(|connection| ActiveAcpConnection {
                        agent_id: state.connection.agent_id(),
                        connection,
                    }),
                AgentConnectionEntry::Connecting { .. } | AgentConnectionEntry::Error { .. } => {
                    None
                }
            })
            .collect()
    }

    pub fn restart_connection(
        &mut self,
        key: Agent,
        server: Rc<dyn AgentServer>,
        cx: &mut Context<Self>,
    ) -> Entity<AgentConnectionEntry> {
        if let Some(entry) = self.entries.get(&key) {
            if matches!(entry.read(cx), AgentConnectionEntry::Connecting { .. }) {
                return entry.clone();
            }
        }

        self.entries.remove(&key);
        self.request_connection(key, server, cx)
    }

    pub fn request_connection(
        &mut self,
        key: Agent,
        server: Rc<dyn AgentServer>,
        cx: &mut Context<Self>,
    ) -> Entity<AgentConnectionEntry> {
        if let Some(entry) = self.entries.get(&key).cloned() {
            let retirement =
                match entry.read(cx) {
                    AgentConnectionEntry::Connected(state) => current_retirement(&state.connection)
                        .map(|error| (error, state.connected_at)),
                    AgentConnectionEntry::Connecting { .. }
                    | AgentConnectionEntry::Error { .. } => None,
                };
            if let Some((error, connected_at)) = retirement {
                self.record_retirement(&key, connected_at, &error, cx);
                self.entries.remove(&key);
            } else {
                return entry;
            }
        }

        let restart_delay = self.restart_delay(&key, cx);
        if !restart_delay.is_zero() {
            log::warn!(
                "Delaying restart of agent `{}` by {restart_delay:?} after repeated connection failures",
                server.agent_id()
            );
        }
        let (mut new_version_rx, mut loading_status_rx, connect_task) =
            self.start_connection(server, restart_delay, cx);
        let connect_task = connect_task.shared();

        let entry = cx.new(|_cx| AgentConnectionEntry::Connecting {
            connect_task: connect_task.clone(),
        });

        self.entries.insert(key.clone(), entry.clone());
        cx.notify();

        cx.spawn({
            let key = key.clone();
            let entry = entry.downgrade();
            async move |this, cx| match connect_task.await {
                Ok(connected_state) => {
                    let retirement = connected_state.connection.retirement();
                    let connection_agent_id = connected_state.connection.agent_id();
                    let connected_at = connected_state.connected_at;
                    this.update(cx, {
                        let key = key.clone();
                        let entry = entry.clone();
                        move |this, cx| {
                            if this.entries.get(&key) != entry.upgrade().as_ref() {
                                return;
                            }

                            entry
                                .update(cx, move |entry, cx| {
                                    if let AgentConnectionEntry::Connecting { .. } = entry {
                                        *entry = AgentConnectionEntry::Connected(connected_state);
                                        cx.notify();
                                    }
                                })
                                .ok();
                            cx.notify();
                        }
                    })
                    .ok();

                    let Some(mut retirement) = retirement else {
                        return;
                    };
                    let current_error = {
                        let current_error = retirement.borrow().clone();
                        current_error
                    };
                    let error = match current_error {
                        Some(error) => error,
                        None => match retirement.recv().await {
                            Ok(Some(error)) => error,
                            Ok(None) | Err(_) => return,
                        },
                    };
                    this.update(cx, move |this, cx| {
                        if this.entries.get(&key) != entry.upgrade().as_ref() {
                            return;
                        }

                        log::error!(
                            "Retiring agent connection for `{}`: {error}",
                            connection_agent_id
                        );
                        this.record_retirement(&key, connected_at, &error, cx);
                        entry
                            .update(cx, {
                                let error = error.clone();
                                move |entry, cx| {
                                    *entry = AgentConnectionEntry::Error { error };
                                    cx.notify();
                                }
                            })
                            .ok();
                        this.entries.remove(&key);
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) => {
                    this.update(cx, move |this, cx| {
                        if this.entries.get(&key) != entry.upgrade().as_ref() {
                            return;
                        }

                        entry
                            .update(cx, {
                                let error = error.clone();
                                move |entry, cx| {
                                    if let AgentConnectionEntry::Connecting { .. } = entry {
                                        *entry = AgentConnectionEntry::Error { error };
                                        cx.notify();
                                    }
                                }
                            })
                            .ok();
                        this.record_connection_failure(&key, &error, cx);
                        this.entries.remove(&key);
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();

        cx.spawn({
            let key = key.clone();
            let entry = entry.downgrade();
            async move |this, cx| {
                while let Ok(version) = new_version_rx.recv().await {
                    let Some(version) = version else {
                        continue;
                    };

                    this.update(cx, move |this, cx| {
                        if this.entries.get(&key) != entry.upgrade().as_ref() {
                            return;
                        }

                        entry
                            .update(cx, move |_entry, cx| {
                                cx.emit(AgentConnectionEntryEvent::NewVersionAvailable(
                                    version.into(),
                                ));
                            })
                            .ok();
                        this.entries.remove(&key);
                        cx.notify();
                    })
                    .ok();
                    break;
                }
            }
        })
        .detach();

        cx.spawn({
            let entry = entry.downgrade();
            async move |this, cx| {
                while let Ok(status) = loading_status_rx.recv().await {
                    let status = status.map(SharedString::from);
                    let key = key.clone();
                    let entry = entry.clone();
                    this.update(cx, move |this, cx| {
                        if this.entries.get(&key) != entry.upgrade().as_ref() {
                            return;
                        }

                        entry
                            .update(cx, move |_entry, cx| {
                                cx.emit(AgentConnectionEntryEvent::LoadingStatusChanged(status));
                            })
                            .ok();
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();

        entry
    }

    fn handle_agent_servers_updated(
        &mut self,
        store: Entity<AgentServerStore>,
        _: &AgentServersUpdated,
        cx: &mut Context<Self>,
    ) {
        let store = store.read(cx);
        self.entries.retain(|key, _| match key {
            Agent::NativeAgent => true,
            Agent::Custom { id } => store.external_agents.contains_key(id),
            #[cfg(any(test, feature = "test-support"))]
            Agent::Stub => true,
        });
        cx.notify();
    }

    fn start_connection(
        &self,
        server: Rc<dyn AgentServer>,
        restart_delay: Duration,
        cx: &mut Context<Self>,
    ) -> (
        Receiver<Option<String>>,
        Receiver<Option<String>>,
        Task<Result<AgentConnectedState, LoadError>>,
    ) {
        let (new_version_tx, new_version_rx) = watch::channel::<Option<String>>(None);
        let (loading_status_tx, loading_status_rx) = watch::channel::<Option<String>>(None);

        let agent_server_store = self.project.read(cx).agent_server_store().clone();
        let delegate = AgentServerDelegate::new(
            agent_server_store,
            Some(new_version_tx),
            Some(loading_status_tx),
        );

        let project = self.project.clone();
        let connect_task = cx.spawn(async move |_this, cx| {
            if !restart_delay.is_zero() {
                cx.background_executor().timer(restart_delay).await;
            }
            let connect_task = cx.update(|cx| server.connect(delegate, project.clone(), cx));
            match connect_task.await {
                Ok(connection) => Ok(AgentConnectedState {
                    connection,
                    connected_at: cx.background_executor().now(),
                }),
                Err(err) => match err.downcast::<LoadError>() {
                    Ok(load_error) => Err(load_error),
                    Err(err) => Err(LoadError::Other(SharedString::from(err.to_string()))),
                },
            }
        });
        (new_version_rx, loading_status_rx, connect_task)
    }

    fn restart_delay(&mut self, key: &Agent, cx: &App) -> Duration {
        let Some(backoff) = self.restart_backoffs.get_mut(key) else {
            return Duration::ZERO;
        };
        let Some(retry_at) = backoff.retry_at else {
            return Duration::ZERO;
        };
        let now = cx.background_executor().now();
        if now >= retry_at {
            backoff.retry_at = None;
            Duration::ZERO
        } else {
            retry_at - now
        }
    }

    fn record_retirement(
        &mut self,
        key: &Agent,
        connected_at: Instant,
        error: &LoadError,
        cx: &App,
    ) {
        let now = cx.background_executor().now();
        if now.saturating_duration_since(connected_at) >= STABLE_CONNECTION_DURATION {
            self.restart_backoffs.remove(key);
            return;
        }
        self.record_connection_failure(key, error, cx);
    }

    fn record_connection_failure(&mut self, key: &Agent, error: &LoadError, cx: &App) {
        let now = cx.background_executor().now();
        let backoff = self.restart_backoffs.entry(key.clone()).or_default();
        backoff.consecutive_failures = backoff.consecutive_failures.saturating_add(1);
        let exponent = backoff.consecutive_failures.saturating_sub(1).min(5);
        let delay = (INITIAL_RESTART_DELAY * (1 << exponent)).min(MAX_RESTART_DELAY);
        backoff.retry_at = Some(now + delay);
        log::warn!(
            "Agent connection failed {count} consecutive time(s): {error}; next restart allowed in {delay:?}",
            count = backoff.consecutive_failures
        );
    }
}

fn current_retirement(connection: &Rc<dyn AgentConnection>) -> Option<LoadError> {
    let mut retirement = connection.retirement()?;
    let error = retirement.borrow().clone();
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_test;
    use agent_servers::{AcpConnectionRetired, FakeAcpAgentServer};
    use fs::FakeFs;
    use gpui::TestAppContext;
    use project::AgentId;
    use std::sync::atomic::Ordering;
    use util::path_list::PathList;

    #[gpui::test]
    async fn retired_connection_is_replaced_with_crash_loop_backoff(cx: &mut TestAppContext) {
        init_test(cx);
        let project = Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let store = cx.new(|cx| AgentConnectionStore::new(project.clone(), cx));
        let server = Rc::new(FakeAcpAgentServer::new());
        let connect_count = server.connect_count();
        let key = Agent::Custom { id: "Test".into() };

        let first_entry = store.update(cx, |store, cx| {
            store.request_connection(key.clone(), server.clone(), cx)
        });
        cx.run_until_parked();
        let first_connection = first_entry.read_with(cx, |entry, _cx| match entry {
            AgentConnectionEntry::Connected(state) => state.connection.clone(),
            _ => panic!("first connection should be established"),
        });
        assert_eq!(connect_count.load(Ordering::SeqCst), 1);

        server.simulate_server_exit();
        cx.run_until_parked();
        assert!(
            store.read_with(cx, |store, _cx| store.entry(&key).is_none()),
            "retired connection must be removed from the registry"
        );

        let stale_session_task = cx.update(|cx| {
            first_connection
                .clone()
                .new_session(project.clone(), PathList::default(), cx)
        });
        let stale_error = stale_session_task
            .await
            .expect_err("retired connection must reject session/new");
        let retired_error = stale_error
            .downcast_ref::<AcpConnectionRetired>()
            .expect("retired connection must return its typed error");
        assert_eq!(retired_error.agent_id, AgentId::new("test"));
        assert!(matches!(retired_error.reason, LoadError::Exited { .. }));

        let second_entry = store.update(cx, |store, cx| {
            store.request_connection(key.clone(), server.clone(), cx)
        });
        cx.run_until_parked();
        assert_eq!(
            connect_count.load(Ordering::SeqCst),
            1,
            "first crash must delay the replacement by one second"
        );
        cx.executor().advance_clock(INITIAL_RESTART_DELAY);
        cx.run_until_parked();
        second_entry.read_with(cx, |entry, _cx| {
            assert!(matches!(entry, AgentConnectionEntry::Connected(_)));
        });
        assert_eq!(connect_count.load(Ordering::SeqCst), 2);

        server.simulate_server_exit();
        cx.run_until_parked();
        let third_entry = store.update(cx, |store, cx| {
            store.request_connection(key, server.clone(), cx)
        });
        cx.run_until_parked();
        cx.executor().advance_clock(INITIAL_RESTART_DELAY);
        cx.run_until_parked();
        assert_eq!(
            connect_count.load(Ordering::SeqCst),
            2,
            "second crash must increase the delay to two seconds"
        );
        cx.executor().advance_clock(INITIAL_RESTART_DELAY);
        cx.run_until_parked();
        third_entry.read_with(cx, |entry, _cx| {
            assert!(matches!(entry, AgentConnectionEntry::Connected(_)));
        });
        assert_eq!(connect_count.load(Ordering::SeqCst), 3);

        drop(first_connection);
        drop(first_entry);
        drop(second_entry);
        drop(third_entry);
        drop(store);
        drop(project);
        drop(server);
        cx.run_until_parked();
    }
}
