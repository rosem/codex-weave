use std::collections::HashMap;

pub(crate) use codex_weave_client::WeaveActionResult;
pub(crate) use codex_weave_client::WeaveAgent;
pub(crate) use codex_weave_client::WeaveAgentConnection;
pub(crate) use codex_weave_client::WeaveIncomingMessage;
pub(crate) use codex_weave_client::WeaveMessageKind;
pub(crate) use codex_weave_client::WeaveMessageMetadata;
pub(crate) use codex_weave_client::WeaveSession;
pub(crate) use codex_weave_client::WeaveTaskDone;
pub(crate) use codex_weave_client::WeaveTaskUpdate;
pub(crate) use codex_weave_client::WeaveTool;
pub(crate) use codex_weave_client::close_session;
pub(crate) use codex_weave_client::connect_agent;
pub(crate) use codex_weave_client::create_session;
pub(crate) use codex_weave_client::list_agents;
pub(crate) use codex_weave_client::list_sessions;

fn weave_mention_counts(agents: &[WeaveAgent]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for agent in agents {
        let mention = agent.mention_text();
        let count = counts.entry(mention).or_insert(0);
        *count += 1;
    }
    counts
}

fn weave_mention_token(agent: &WeaveAgent, counts: &HashMap<String, usize>) -> String {
    let mention = agent.mention_text();
    if counts.get(&mention).copied().unwrap_or(0) > 1 {
        agent.id.clone()
    } else {
        mention
    }
}

pub(crate) fn weave_mention_tokens(agents: &[WeaveAgent]) -> HashMap<String, String> {
    let counts = weave_mention_counts(agents);
    let mut tokens = HashMap::new();
    for agent in agents {
        tokens.insert(agent.id.clone(), weave_mention_token(agent, &counts));
    }
    tokens
}

pub(crate) fn weave_mention_lookup(agents: &[WeaveAgent]) -> HashMap<String, WeaveAgent> {
    let counts = weave_mention_counts(agents);
    let mut lookup = HashMap::new();
    for agent in agents {
        lookup.insert(weave_mention_token(agent, &counts), agent.clone());
    }
    lookup
}
