use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeaveSessionStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaveSession {
    pub id: String,
    pub name: Option<String>,
    pub status: WeaveSessionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaveAgent {
    pub id: String,
    pub name: Option<String>,
    pub lead: bool,
    pub status: Option<String>,
}

impl WeaveAgent {
    pub fn id(&self) -> &str {
        self.id.as_str()
    }

    pub fn is_lead(&self) -> bool {
        self.lead
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeaveMessageKind {
    User,
    Reply,
    Control,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaveMessage {
    pub id: String,
    pub session_id: String,
    pub src: String,
    pub dst: Option<String>,
    pub kind: WeaveMessageKind,
    pub text: String,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaveOutboundMessage {
    pub session_id: String,
    pub src: String,
    pub dst: Option<String>,
    pub kind: WeaveMessageKind,
    pub text: String,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeaveRuntimeInput {
    SessionUpdated(WeaveSession),
    AgentsUpdated {
        session_id: String,
        agents: Vec<WeaveAgent>,
    },
    MessageReceived(WeaveMessage),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeaveRuntimeAction {
    SendMessage(WeaveOutboundMessage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeadPolicy {
    Single,
    Multiple,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopGuardConfig {
    pub suppress_reply_relay: bool,
}

impl Default for LoopGuardConfig {
    fn default() -> Self {
        Self {
            suppress_reply_relay: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaveRuntimeConfig {
    pub lead_policy: LeadPolicy,
    pub loop_guard: LoopGuardConfig,
}

impl Default for WeaveRuntimeConfig {
    fn default() -> Self {
        Self {
            lead_policy: LeadPolicy::Single,
            loop_guard: LoopGuardConfig::default(),
        }
    }
}

#[derive(Debug)]
pub struct LoopGuard {
    config: LoopGuardConfig,
}

impl LoopGuard {
    pub fn new(config: LoopGuardConfig) -> Self {
        Self { config }
    }

    pub fn allow_relay(&self, message: &WeaveMessage) -> bool {
        if self.config.suppress_reply_relay && message.kind == WeaveMessageKind::Reply {
            return false;
        }
        if matches!(
            message.kind,
            WeaveMessageKind::Control | WeaveMessageKind::System
        ) {
            return false;
        }
        true
    }
}

#[derive(Debug)]
pub struct WeaveRuntime {
    config: WeaveRuntimeConfig,
    loop_guard: LoopGuard,
    agents_by_session: HashMap<String, Vec<WeaveAgent>>,
}

impl Default for WeaveRuntime {
    fn default() -> Self {
        Self {
            config: WeaveRuntimeConfig::default(),
            loop_guard: LoopGuard::new(LoopGuardConfig::default()),
            agents_by_session: HashMap::new(),
        }
    }
}

impl WeaveRuntime {
    pub fn new(config: WeaveRuntimeConfig) -> Self {
        Self {
            config,
            loop_guard: LoopGuard::new(config.loop_guard),
            agents_by_session: HashMap::new(),
        }
    }

    pub fn config(&self) -> WeaveRuntimeConfig {
        self.config
    }

    pub fn handle_event(&mut self, event: WeaveRuntimeInput) -> Vec<WeaveRuntimeAction> {
        match event {
            WeaveRuntimeInput::SessionUpdated(_) => Vec::new(),
            WeaveRuntimeInput::AgentsUpdated { session_id, agents } => {
                self.set_agents(session_id, agents);
                Vec::new()
            }
            WeaveRuntimeInput::MessageReceived(message) => self.handle_message(message),
        }
    }

    pub fn set_agents(&mut self, session_id: String, agents: Vec<WeaveAgent>) {
        self.agents_by_session.insert(session_id, agents);
    }

    fn handle_message(&self, message: WeaveMessage) -> Vec<WeaveRuntimeAction> {
        if !self.loop_guard.allow_relay(&message) {
            return Vec::new();
        }

        let targets = self.relay_targets(&message);
        if targets.is_empty() {
            return Vec::new();
        }

        targets
            .into_iter()
            .map(|dst| {
                WeaveRuntimeAction::SendMessage(WeaveOutboundMessage {
                    session_id: message.session_id.clone(),
                    src: message.src.clone(),
                    dst: Some(dst),
                    kind: message.kind,
                    text: message.text.clone(),
                    reply_to: Some(message.id.clone()),
                })
            })
            .collect()
    }

    fn relay_targets(&self, message: &WeaveMessage) -> Vec<String> {
        if let Some(dst) = message.dst.as_ref() {
            if dst == &message.src {
                return Vec::new();
            }
            return vec![dst.clone()];
        }

        let agents = match self.agents_by_session.get(&message.session_id) {
            Some(agents) => agents,
            None => return Vec::new(),
        };

        let mut lead_ids = self.select_lead_ids(agents);
        lead_ids.retain(|id| id != &message.src);
        lead_ids
    }

    fn select_lead_ids(&self, agents: &[WeaveAgent]) -> Vec<String> {
        let mut lead_ids: Vec<String> = agents
            .iter()
            .filter(|agent| agent.is_lead())
            .map(WeaveAgent::id)
            .map(str::to_string)
            .collect();

        if self.config.lead_policy == LeadPolicy::Single && lead_ids.len() > 1 {
            lead_ids.sort();
            lead_ids.truncate(1);
        }

        lead_ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;

    fn agent(id: &str, lead: bool) -> WeaveAgent {
        WeaveAgent {
            id: id.to_string(),
            name: None,
            lead,
            status: None,
        }
    }

    fn message(
        id: &str,
        session_id: &str,
        src: &str,
        dst: Option<&str>,
        kind: WeaveMessageKind,
        text: &str,
    ) -> WeaveMessage {
        WeaveMessage {
            id: id.to_string(),
            session_id: session_id.to_string(),
            src: src.to_string(),
            dst: dst.map(str::to_string),
            kind,
            text: text.to_string(),
            reply_to: None,
        }
    }

    fn action_for(dst: &str, message: &WeaveMessage) -> WeaveRuntimeAction {
        WeaveRuntimeAction::SendMessage(WeaveOutboundMessage {
            session_id: message.session_id.clone(),
            src: message.src.clone(),
            dst: Some(dst.to_string()),
            kind: message.kind,
            text: message.text.clone(),
            reply_to: Some(message.id.clone()),
        })
    }

    #[test]
    fn drops_message_before_agent_list() {
        let mut runtime = WeaveRuntime::default();
        let message = message(
            "msg-1",
            "session-1",
            "user-1",
            None,
            WeaveMessageKind::User,
            "hello",
        );
        let actions = runtime.handle_event(WeaveRuntimeInput::MessageReceived(message));
        assert_eq!(actions, Vec::new());
    }

    #[test]
    fn suppresses_reply_by_default() {
        let mut runtime = WeaveRuntime::default();
        runtime.set_agents("session-1".to_string(), vec![agent("lead-a", true)]);
        let message = message(
            "msg-2",
            "session-1",
            "user-1",
            None,
            WeaveMessageKind::Reply,
            "ack",
        );
        let actions = runtime.handle_event(WeaveRuntimeInput::MessageReceived(message));
        assert_eq!(actions, Vec::new());
    }

    #[test]
    fn relays_reply_when_loop_guard_disabled() {
        let config = WeaveRuntimeConfig {
            lead_policy: LeadPolicy::Single,
            loop_guard: LoopGuardConfig {
                suppress_reply_relay: false,
            },
        };
        let mut runtime = WeaveRuntime::new(config);
        runtime.set_agents("session-1".to_string(), vec![agent("lead-a", true)]);
        let message = message(
            "msg-3",
            "session-1",
            "user-1",
            None,
            WeaveMessageKind::Reply,
            "ack",
        );
        let actions = runtime.handle_event(WeaveRuntimeInput::MessageReceived(message.clone()));
        assert_eq!(actions, vec![action_for("lead-a", &message)]);
    }

    #[test]
    fn relays_to_all_leads_with_multiple_policy() {
        let config = WeaveRuntimeConfig {
            lead_policy: LeadPolicy::Multiple,
            loop_guard: LoopGuardConfig::default(),
        };
        let mut runtime = WeaveRuntime::new(config);
        runtime.set_agents(
            "session-1".to_string(),
            vec![agent("lead-a", true), agent("lead-b", true)],
        );
        let message = message(
            "msg-4",
            "session-1",
            "user-1",
            None,
            WeaveMessageKind::User,
            "ping",
        );
        let actions = runtime.handle_event(WeaveRuntimeInput::MessageReceived(message.clone()));
        assert_eq!(
            actions,
            vec![
                action_for("lead-a", &message),
                action_for("lead-b", &message)
            ]
        );
    }

    #[test]
    fn retries_are_not_deduplicated() {
        let mut runtime = WeaveRuntime::default();
        runtime.set_agents("session-1".to_string(), vec![agent("lead-a", true)]);
        let message = message(
            "msg-5",
            "session-1",
            "user-1",
            None,
            WeaveMessageKind::User,
            "ping",
        );
        let expected = vec![action_for("lead-a", &message)];
        let first = runtime.handle_event(WeaveRuntimeInput::MessageReceived(message.clone()));
        let second = runtime.handle_event(WeaveRuntimeInput::MessageReceived(message));
        assert_eq!(first, expected);
        assert_eq!(second, expected);
    }
}
