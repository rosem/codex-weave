use std::collections::HashMap;
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::response::sse::Event;
use axum::response::sse::Sse;
use axum::routing::delete;
use axum::routing::get;
use axum::routing::post;
use chrono::SecondsFormat;
use chrono::Utc;
use codex_weave_runtime::WeaveAgent;
use codex_weave_runtime::WeaveMessage;
use codex_weave_runtime::WeaveMessageKind;
use codex_weave_runtime::WeaveRuntime;
use codex_weave_runtime::WeaveRuntimeAction;
use codex_weave_runtime::WeaveRuntimeInput;
use codex_weave_runtime::WeaveSession;
use codex_weave_runtime::WeaveSessionStatus;
use codex_weave_transport_uds::WeaveUdsClient;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use tokio::sync::RwLock;
use tokio_stream::wrappers::UnboundedReceiverStream;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct WeaveHttpState {
    store: Arc<RwLock<WeaveHttpStore>>,
    uds_client: Option<WeaveUdsClient>,
}

impl Default for WeaveHttpState {
    fn default() -> Self {
        Self {
            store: Arc::new(RwLock::new(WeaveHttpStore::default())),
            uds_client: None,
        }
    }
}

impl WeaveHttpState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_uds(uds_client: WeaveUdsClient) -> Self {
        Self {
            store: Arc::new(RwLock::new(WeaveHttpStore::default())),
            uds_client: Some(uds_client),
        }
    }
}

#[derive(Debug, Default)]
struct WeaveHttpStore {
    sessions: HashMap<String, WeaveSession>,
    agents_by_session: HashMap<String, Vec<WeaveAgent>>,
    agent_overrides: HashMap<String, HashMap<String, AgentOverride>>,
    agent_order: HashMap<String, Vec<String>>,
    runtime: WeaveRuntime,
    events_by_session: HashMap<String, Vec<WeaveHttpEvent>>,
}

#[derive(Debug, Clone, Default)]
struct AgentOverride {
    name: Option<String>,
    lead: Option<bool>,
    status: Option<String>,
}

#[derive(Serialize)]
struct HealthPayload {
    status: &'static str,
}

#[derive(Serialize)]
struct ErrorPayload {
    error: &'static str,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    id: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateAgentRequest {
    id: Option<String>,
    name: Option<String>,
    lead: Option<bool>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateAgentRequest {
    name: Option<String>,
    lead: Option<bool>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReorderAgentsRequest {
    agent_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SendMessageRequest {
    text: String,
    agent_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct WeaveHttpEvent {
    id: String,
    session_id: String,
    agent_id: Option<String>,
    event_type: String,
    created_at: String,
    payload: Value,
}

pub fn router(state: WeaveHttpState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/stream", get(not_implemented))
        .route("/api/sessions/{id}", get(get_session))
        .route("/api/sessions/{id}/close", post(close_session))
        .route(
            "/api/sessions/{id}/agents",
            get(list_agents).post(create_agent),
        )
        .route(
            "/api/sessions/{id}/agents/{agent_id}",
            delete(delete_agent).patch(update_agent),
        )
        .route("/api/sessions/{id}/agents/order", post(reorder_agents))
        .route("/api/sessions/{id}/messages", post(send_message))
        .route("/api/sessions/{id}/events", get(list_events))
        .route("/api/sessions/{id}/events/stream", get(stream_events))
        .with_state(state)
}

async fn health() -> Response {
    ok_response(HealthPayload { status: "ok" })
}

async fn list_sessions(State(state): State<WeaveHttpState>) -> Response {
    match fetch_sessions(&state).await {
        Ok(sessions) => ok_response(sessions),
        Err(response) => response,
    }
}

async fn create_session(
    State(state): State<WeaveHttpState>,
    Json(request): Json<CreateSessionRequest>,
) -> Response {
    if state.uds_client.is_some() && request.id.is_some() {
        return error_response(StatusCode::BAD_REQUEST, "id not supported with UDS");
    }

    let id = match request.id {
        Some(id) if !id.trim().is_empty() => id,
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "id must be non-empty"),
        None => Uuid::new_v4().to_string(),
    };

    if let Some(uds_client) = &state.uds_client {
        let name = request.name;
        let session = match uds_client.create_session(name).await {
            Ok(session) => session,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "weave coordinator unavailable",
                );
            }
        };
        let mut store = state.store.write().await;
        store.sessions.insert(session.id.clone(), session.clone());
        return created_response(session);
    }

    let mut store = state.store.write().await;
    if let Some(existing) = store.sessions.get(&id) {
        return ok_response(existing.clone());
    }

    let session = WeaveSession {
        id: id.clone(),
        name: request.name,
        status: WeaveSessionStatus::Open,
    };

    store.sessions.insert(id, session.clone());
    record_event(
        &mut store,
        session.id.clone(),
        None,
        "session.created",
        json!({ "id": session.id, "name": session.name }),
    );
    created_response(session)
}

async fn get_session(
    State(state): State<WeaveHttpState>,
    Path(session_id): Path<String>,
) -> Response {
    let sessions = match fetch_sessions(&state).await {
        Ok(sessions) => sessions,
        Err(response) => return response,
    };
    match sessions
        .into_iter()
        .find(|session| session.id == session_id)
    {
        Some(session) => ok_response(session),
        None => error_response(StatusCode::NOT_FOUND, "session not found"),
    }
}

async fn close_session(
    State(state): State<WeaveHttpState>,
    Path(session_id): Path<String>,
) -> Response {
    if let Some(uds_client) = &state.uds_client {
        if uds_client.close_session(&session_id).await.is_err() {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "weave coordinator unavailable",
            );
        }
        let mut store = state.store.write().await;
        if let Some(session) = store.sessions.get_mut(&session_id) {
            session.status = WeaveSessionStatus::Closed;
            return ok_response(session.clone());
        }
        return StatusCode::NO_CONTENT.into_response();
    }

    let mut store = state.store.write().await;
    let session_snapshot = match store.sessions.get_mut(&session_id) {
        Some(session) => {
            session.status = WeaveSessionStatus::Closed;
            session.clone()
        }
        None => return error_response(StatusCode::NOT_FOUND, "session not found"),
    };
    record_event(
        &mut store,
        session_id,
        None,
        "session.closed",
        json!({ "status": "closed" }),
    );
    ok_response(session_snapshot)
}

async fn list_agents(
    State(state): State<WeaveHttpState>,
    Path(session_id): Path<String>,
) -> Response {
    if let Some(uds_client) = &state.uds_client {
        let agents = match uds_client.list_agents(&session_id).await {
            Ok(agents) => agents,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "weave coordinator unavailable",
                );
            }
        };
        let store = state.store.read().await;
        let overrides = store.agent_overrides.get(&session_id);
        let order = store.agent_order.get(&session_id);
        let merged = apply_agent_overrides(agents, overrides, order);
        return ok_response(merged);
    }

    let store = state.store.read().await;
    if !store.sessions.contains_key(&session_id) {
        return error_response(StatusCode::NOT_FOUND, "session not found");
    }
    let agents = store
        .agents_by_session
        .get(&session_id)
        .cloned()
        .unwrap_or_default();
    ok_response(agents)
}

async fn create_agent(
    State(state): State<WeaveHttpState>,
    Path(session_id): Path<String>,
    Json(request): Json<CreateAgentRequest>,
) -> Response {
    let id = match request.id {
        Some(id) if !id.trim().is_empty() => id,
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "id must be non-empty"),
        None => Uuid::new_v4().to_string(),
    };

    if state.uds_client.is_some() {
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            "agent creation requires runtime",
        );
    }

    let mut store = state.store.write().await;
    if !store.sessions.contains_key(&session_id) {
        return error_response(StatusCode::NOT_FOUND, "session not found");
    }

    let session_key = session_id.clone();
    let (agent_snapshot, agents_snapshot) = {
        let agents = store.agents_by_session.entry(session_key).or_default();
        if let Some(existing) = agents.iter().find(|agent| agent.id == id) {
            return ok_response(existing.clone());
        }

        let agent = WeaveAgent {
            id: id.clone(),
            name: request.name,
            lead: request.lead.unwrap_or(false),
            status: request.status,
        };

        agents.push(agent.clone());
        (agent, agents.clone())
    };
    sync_runtime_agents(&mut store, &session_id, &agents_snapshot);
    record_event(
        &mut store,
        session_id.clone(),
        Some(agent_snapshot.id.clone()),
        "agent.created",
        json!({
            "id": agent_snapshot.id,
            "name": agent_snapshot.name,
            "lead": agent_snapshot.lead
        }),
    );
    created_response(agent_snapshot)
}

async fn update_agent(
    State(state): State<WeaveHttpState>,
    Path((session_id, agent_id)): Path<(String, String)>,
    Json(request): Json<UpdateAgentRequest>,
) -> Response {
    if let Some(uds_client) = &state.uds_client {
        let agents = match uds_client.list_agents(&session_id).await {
            Ok(agents) => agents,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "weave coordinator unavailable",
                );
            }
        };
        let target = match agents.iter().find(|agent| agent.id == agent_id) {
            Some(agent) => agent.clone(),
            None => return error_response(StatusCode::NOT_FOUND, "agent not found"),
        };
        let updated = apply_agent_update(target, request);
        let mut store = state.store.write().await;
        let overrides = store.agent_overrides.entry(session_id).or_default();
        let entry = overrides.entry(agent_id.clone()).or_default();
        entry.name = updated.name.clone();
        entry.lead = Some(updated.lead);
        entry.status = updated.status.clone();
        return ok_response(updated);
    }

    let mut store = state.store.write().await;
    let (agent_snapshot, agents_snapshot) = {
        let agents = match store.agents_by_session.get_mut(&session_id) {
            Some(agents) => agents,
            None => return error_response(StatusCode::NOT_FOUND, "session not found"),
        };

        let agent = match agents.iter_mut().find(|agent| agent.id == agent_id) {
            Some(agent) => agent,
            None => return error_response(StatusCode::NOT_FOUND, "agent not found"),
        };

        if let Some(name) = request.name {
            agent.name = Some(name);
        }
        if let Some(lead) = request.lead {
            agent.lead = lead;
        }
        if let Some(status) = request.status {
            agent.status = Some(status);
        }

        (agent.clone(), agents.clone())
    };

    sync_runtime_agents(&mut store, &session_id, &agents_snapshot);
    record_event(
        &mut store,
        session_id.clone(),
        Some(agent_id),
        "agent.updated",
        json!({
            "id": agent_snapshot.id,
            "name": agent_snapshot.name,
            "lead": agent_snapshot.lead
        }),
    );
    ok_response(agent_snapshot)
}

async fn delete_agent(
    State(state): State<WeaveHttpState>,
    Path((session_id, agent_id)): Path<(String, String)>,
) -> Response {
    if state.uds_client.is_some() {
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            "agent deletion requires runtime",
        );
    }

    let mut store = state.store.write().await;
    let agents_snapshot = {
        let agents = match store.agents_by_session.get_mut(&session_id) {
            Some(agents) => agents,
            None => return error_response(StatusCode::NOT_FOUND, "session not found"),
        };

        let before_len = agents.len();
        agents.retain(|agent| agent.id != agent_id);
        if agents.len() == before_len {
            return error_response(StatusCode::NOT_FOUND, "agent not found");
        }
        agents.clone()
    };

    sync_runtime_agents(&mut store, &session_id, &agents_snapshot);
    record_event(
        &mut store,
        session_id,
        Some(agent_id),
        "agent.deleted",
        json!({ "ok": true }),
    );
    StatusCode::NO_CONTENT.into_response()
}

async fn reorder_agents(
    State(state): State<WeaveHttpState>,
    Path(session_id): Path<String>,
    Json(request): Json<ReorderAgentsRequest>,
) -> Response {
    if let Some(uds_client) = &state.uds_client {
        let agents = match uds_client.list_agents(&session_id).await {
            Ok(agents) => agents,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "weave coordinator unavailable",
                );
            }
        };
        let existing_ids: HashSet<String> = agents
            .iter()
            .map(WeaveAgent::id)
            .map(str::to_string)
            .collect();
        let requested_ids: HashSet<String> = request.agent_ids.iter().cloned().collect();

        if existing_ids != requested_ids {
            return error_response(
                StatusCode::BAD_REQUEST,
                "agent_ids must include each existing agent once",
            );
        }

        let mut store = state.store.write().await;
        store
            .agent_order
            .insert(session_id.clone(), request.agent_ids.clone());
        let overrides = store.agent_overrides.get(&session_id);
        let merged = apply_agent_overrides(agents, overrides, Some(&request.agent_ids));
        return ok_response(merged);
    }

    let mut store = state.store.write().await;
    let order = request.agent_ids;
    let agents_snapshot = {
        let agents = match store.agents_by_session.get_mut(&session_id) {
            Some(agents) => agents,
            None => return error_response(StatusCode::NOT_FOUND, "session not found"),
        };

        let existing_ids: HashSet<String> = agents
            .iter()
            .map(WeaveAgent::id)
            .map(str::to_string)
            .collect();
        let requested_ids: HashSet<String> = order.iter().cloned().collect();

        if existing_ids != requested_ids {
            return error_response(
                StatusCode::BAD_REQUEST,
                "agent_ids must include each existing agent once",
            );
        }

        let mut reordered = Vec::with_capacity(agents.len());
        for agent_id in &order {
            if let Some(pos) = agents.iter().position(|agent| agent.id == *agent_id) {
                reordered.push(agents[pos].clone());
            }
        }

        *agents = reordered;
        agents.clone()
    };
    sync_runtime_agents(&mut store, &session_id, &agents_snapshot);
    record_event(
        &mut store,
        session_id,
        None,
        "agent.reordered",
        json!({ "agent_ids": order }),
    );
    ok_response(agents_snapshot)
}

async fn send_message(
    State(state): State<WeaveHttpState>,
    Path(session_id): Path<String>,
    Json(request): Json<SendMessageRequest>,
) -> Response {
    if request.text.trim().is_empty() || request.agent_ids.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "missing text or agent_ids");
    }

    if let Some(uds_client) = &state.uds_client {
        let agents = match uds_client.list_agents(&session_id).await {
            Ok(agents) => agents,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "weave coordinator unavailable",
                );
            }
        };

        let text = request.text;
        let requested_ids = request.agent_ids;
        let actions = {
            let mut store = state.store.write().await;
            let overrides = store.agent_overrides.get(&session_id);
            let order = store.agent_order.get(&session_id);
            let merged_agents = apply_agent_overrides(agents, overrides, order);
            sync_runtime_agents(&mut store, &session_id, &merged_agents);

            let agent_ids: HashSet<String> = merged_agents
                .iter()
                .map(WeaveAgent::id)
                .map(str::to_string)
                .collect();
            let target_ids: Vec<String> = requested_ids
                .into_iter()
                .filter(|agent_id| agent_ids.contains(agent_id))
                .collect();

            if target_ids.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "agent_ids not found");
            }

            let mut actions = Vec::new();
            for target_id in target_ids {
                let message = WeaveMessage {
                    id: Uuid::new_v4().to_string(),
                    session_id: session_id.clone(),
                    src: "user".to_string(),
                    dst: Some(target_id),
                    kind: WeaveMessageKind::User,
                    text: text.clone(),
                    reply_to: None,
                };
                actions.extend(
                    store
                        .runtime
                        .handle_event(WeaveRuntimeInput::MessageReceived(message)),
                );
            }
            actions
        };

        let mut batches: Vec<(WeaveMessageKind, String, Vec<String>)> = Vec::new();
        for action in actions {
            let WeaveRuntimeAction::SendMessage(message) = action;
            let Some(dst) = message.dst else {
                continue;
            };
            if let Some((_, _, dsts)) = batches
                .iter_mut()
                .find(|(kind, text, _)| *kind == message.kind && text == &message.text)
            {
                dsts.push(dst);
            } else {
                batches.push((message.kind, message.text, vec![dst]));
            }
        }

        let sender_name = "weave-http".to_string();
        for (kind, text, dsts) in batches {
            if uds_client
                .send_messages(
                    &session_id,
                    dsts,
                    text,
                    Some(sender_name.clone()),
                    Some(kind),
                )
                .await
                .is_err()
            {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "weave coordinator unavailable",
                );
            }
        }
        return StatusCode::ACCEPTED.into_response();
    }

    let mut store = state.store.write().await;
    if !store.sessions.contains_key(&session_id) {
        return error_response(StatusCode::NOT_FOUND, "session not found");
    }

    let agents = store
        .agents_by_session
        .get(&session_id)
        .cloned()
        .unwrap_or_default();
    let agent_ids: HashSet<String> = agents
        .iter()
        .map(WeaveAgent::id)
        .map(str::to_string)
        .collect();

    let text = request.text;
    let target_ids: Vec<String> = request
        .agent_ids
        .into_iter()
        .filter(|agent_id| agent_ids.contains(agent_id))
        .collect();

    if target_ids.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "agent_ids not found");
    }

    record_event(
        &mut store,
        session_id.clone(),
        None,
        "user.input",
        json!({ "text": text, "agent_ids": target_ids }),
    );

    for target_id in target_ids {
        let message = WeaveMessage {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.clone(),
            src: "user".to_string(),
            dst: Some(target_id),
            kind: WeaveMessageKind::User,
            text: text.clone(),
            reply_to: None,
        };
        let actions = store
            .runtime
            .handle_event(WeaveRuntimeInput::MessageReceived(message.clone()));
        record_event(
            &mut store,
            session_id.clone(),
            message.dst.clone(),
            "runtime.message",
            json!({ "id": message.id, "text": message.text }),
        );
        record_runtime_actions(&mut store, &session_id, actions);
    }

    StatusCode::ACCEPTED.into_response()
}

async fn list_events(
    State(state): State<WeaveHttpState>,
    Path(session_id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Response {
    if let Some(uds_client) = &state.uds_client {
        let limit = query.limit.unwrap_or(500);
        let records = match uds_client.list_events(&session_id, limit).await {
            Ok(records) => records,
            Err(_) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "weave coordinator unavailable",
                );
            }
        };
        let events = records
            .into_iter()
            .map(|record| http_event_from_record(&session_id, record))
            .collect::<Vec<_>>();
        return ok_response(events);
    }

    let store = state.store.read().await;
    let events = store
        .events_by_session
        .get(&session_id)
        .cloned()
        .unwrap_or_default();
    let limit = query.limit.unwrap_or(500);
    let limited = if events.len() > limit {
        events.into_iter().take(limit).collect()
    } else {
        events
    };
    ok_response(limited)
}

async fn stream_events(
    State(state): State<WeaveHttpState>,
    Path(session_id): Path<String>,
) -> Sse<BoxStream<'static, Result<Event, Infallible>>> {
    if let Some(uds_client) = &state.uds_client {
        let receiver = match uds_client.stream_events(&session_id, true).await {
            Ok(receiver) => receiver,
            Err(_) => {
                let stream = tokio_stream::iter(vec![Ok(
                    Event::default().data("weave coordinator unavailable")
                )])
                .boxed();
                return Sse::new(stream);
            }
        };
        let session_id = session_id.clone();
        let stream = UnboundedReceiverStream::new(receiver)
            .map(move |record| {
                let event = http_event_from_record(&session_id, record);
                Ok(Event::default().data(serialize_event(&event)))
            })
            .boxed();
        return Sse::new(stream);
    }

    let store = state.store.read().await;
    let events = store
        .events_by_session
        .get(&session_id)
        .cloned()
        .unwrap_or_default();
    let stream = tokio_stream::iter(
        events
            .into_iter()
            .map(|event| Ok(Event::default().data(serialize_event(&event)))),
    )
    .boxed();
    Sse::new(stream)
}

async fn not_implemented() -> Response {
    error_response(StatusCode::NOT_IMPLEMENTED, "not implemented")
}

fn ok_response<T>(payload: T) -> Response
where
    T: Serialize,
{
    (StatusCode::OK, Json(payload)).into_response()
}

fn created_response<T>(payload: T) -> Response
where
    T: Serialize,
{
    (StatusCode::CREATED, Json(payload)).into_response()
}

fn error_response(status: StatusCode, message: &'static str) -> Response {
    (status, Json(ErrorPayload { error: message })).into_response()
}

async fn fetch_sessions(state: &WeaveHttpState) -> Result<Vec<WeaveSession>, Response> {
    if let Some(uds_client) = &state.uds_client {
        let sessions = uds_client.list_sessions().await.map_err(|_| {
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "weave coordinator unavailable",
            )
        })?;
        let mut store = state.store.write().await;
        store.sessions = sessions
            .iter()
            .cloned()
            .map(|session| (session.id.clone(), session))
            .collect();
        let mut sorted = sessions;
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        return Ok(sorted);
    }

    let store = state.store.read().await;
    let mut sessions: Vec<WeaveSession> = store.sessions.values().cloned().collect();
    sessions.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(sessions)
}

fn apply_agent_update(mut agent: WeaveAgent, request: UpdateAgentRequest) -> WeaveAgent {
    if let Some(name) = request.name {
        agent.name = Some(name);
    }
    if let Some(lead) = request.lead {
        agent.lead = lead;
    }
    if let Some(status) = request.status {
        agent.status = Some(status);
    }
    agent
}

fn apply_agent_overrides(
    agents: Vec<WeaveAgent>,
    overrides: Option<&HashMap<String, AgentOverride>>,
    order: Option<&Vec<String>>,
) -> Vec<WeaveAgent> {
    if overrides.is_none() && order.is_none() {
        return agents;
    }

    let merged: Vec<WeaveAgent> = agents
        .into_iter()
        .map(|mut agent| {
            if let Some(overrides) = overrides
                && let Some(override_entry) = overrides.get(&agent.id)
            {
                if let Some(name) = override_entry.name.clone() {
                    agent.name = Some(name);
                }
                if let Some(lead) = override_entry.lead {
                    agent.lead = lead;
                }
                if let Some(status) = override_entry.status.clone() {
                    agent.status = Some(status);
                }
            }
            agent
        })
        .collect();

    let order = match order {
        Some(order) => order,
        None => return merged,
    };

    let mut by_id: HashMap<String, WeaveAgent> = merged
        .into_iter()
        .map(|agent| (agent.id.clone(), agent))
        .collect();
    let requested_ids: HashSet<String> = order.iter().cloned().collect();
    let existing_ids: HashSet<String> = by_id.keys().cloned().collect();

    if requested_ids != existing_ids {
        return by_id.into_values().collect();
    }

    let mut reordered = Vec::with_capacity(order.len());
    for agent_id in order {
        if let Some(agent) = by_id.remove(agent_id) {
            reordered.push(agent);
        }
    }
    reordered
}

fn sync_runtime_agents(store: &mut WeaveHttpStore, session_id: &str, agents: &[WeaveAgent]) {
    store
        .runtime
        .set_agents(session_id.to_string(), agents.to_vec());
}

fn record_event(
    store: &mut WeaveHttpStore,
    session_id: String,
    agent_id: Option<String>,
    event_type: &'static str,
    payload: Value,
) {
    let event = WeaveHttpEvent {
        id: Uuid::new_v4().to_string(),
        session_id: session_id.clone(),
        agent_id,
        event_type: event_type.to_string(),
        created_at: now_timestamp(),
        payload,
    };
    store
        .events_by_session
        .entry(session_id)
        .or_default()
        .push(event);
}

fn record_runtime_actions(
    store: &mut WeaveHttpStore,
    session_id: &str,
    actions: Vec<WeaveRuntimeAction>,
) {
    for action in actions {
        match action {
            WeaveRuntimeAction::SendMessage(message) => {
                record_event(
                    store,
                    session_id.to_string(),
                    message.dst.clone(),
                    "runtime.send",
                    json!({
                        "text": message.text,
                        "dst": message.dst,
                        "kind": message.kind,
                    }),
                );
            }
        }
    }
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn http_event_from_record(session_id: &str, record: Value) -> WeaveHttpEvent {
    let record_id = record
        .get("seq")
        .and_then(Value::as_i64)
        .map(|seq| format!("record-{seq}"))
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let created_at = record
        .get("ts")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_timestamp);
    let envelope = record.get("envelope");
    let event_type = envelope
        .and_then(|envelope| envelope.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("events.record")
        .to_string();
    let agent_id = envelope
        .and_then(|envelope| {
            envelope
                .get("dst")
                .and_then(Value::as_str)
                .or_else(|| envelope.get("src").and_then(Value::as_str))
        })
        .map(str::to_string);
    WeaveHttpEvent {
        id: record_id,
        session_id: session_id.to_string(),
        agent_id,
        event_type,
        created_at,
        payload: record,
    }
}

fn serialize_event(event: &WeaveHttpEvent) -> String {
    serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string())
}
