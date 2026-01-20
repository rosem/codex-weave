use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeaveSession {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeaveMessageKind {
    User,
    Reply,
    Control,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeaveAgent {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeaveTool {
    NewSession,
    Compact,
    Interrupt,
    Review { instructions: Option<String> },
}

impl WeaveTool {
    pub fn from_command(command: &str, args: Option<&str>) -> Option<Self> {
        let command = command.trim();
        if command.is_empty() {
            return None;
        }
        let args = args.map(str::trim).filter(|value| !value.is_empty());
        let normalized = command.to_ascii_lowercase();
        match normalized.as_str() {
            "new" => Some(Self::NewSession),
            "new_session" => Some(Self::NewSession),
            "compact" => Some(Self::Compact),
            "interrupt" => Some(Self::Interrupt),
            "review" => Some(Self::Review {
                instructions: args.map(ToString::to_string),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeaveIncomingMessage {
    pub session_id: String,
    pub message_id: String,
    pub src: String,
    pub src_name: Option<String>,
    pub meta: Option<Value>,
    pub text: String,
    pub kind: WeaveMessageKind,
    pub conversation_id: String,
    pub conversation_owner: String,
    pub parent_message_id: Option<String>,
    pub task_id: Option<String>,
    pub tool: Option<WeaveTool>,
    pub action_group_id: Option<String>,
    pub action_id: Option<String>,
    pub action_index: Option<usize>,
    pub reply_to_action_id: Option<String>,
    pub action_result: Option<WeaveActionResult>,
    pub task_update: Option<WeaveTaskUpdate>,
    pub task_done: Option<WeaveTaskDone>,
    pub defer_until_ready: bool,
    pub has_conversation_metadata: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeaveActionResult {
    pub group_id: String,
    pub action_id: String,
    pub action_index: usize,
    pub status: String,
    pub detail: Option<String>,
    pub new_context_id: Option<String>,
    pub new_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeaveTaskUpdate {
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeaveTaskDone {
    pub task_id: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeaveMessageMetadata {
    pub conversation_id: String,
    pub conversation_owner: String,
    pub parent_message_id: Option<String>,
    pub task_id: Option<String>,
}

impl WeaveSession {
    pub fn display_name(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| self.id.clone())
    }
}

impl WeaveAgent {
    pub fn display_name(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| self.id.clone())
    }

    pub fn mention_text(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .filter(|name| !name.chars().any(char::is_whitespace))
            .map(ToString::to_string)
            .unwrap_or_else(|| self.id.clone())
    }
}

#[cfg(unix)]
mod platform {
    use super::WeaveActionResult;
    use super::WeaveAgent;
    use super::WeaveIncomingMessage;
    use super::WeaveSession;
    use serde::Deserialize;
    use serde::Serialize;
    use serde_json::Value;
    use serde_json::json;
    use std::collections::HashMap;
    use std::env;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::io::BufReader;
    use tokio::io::ReadHalf;
    use tokio::io::WriteHalf;
    use tokio::net::UnixStream;
    use tokio::sync::mpsc;
    use tokio::sync::oneshot;
    use tracing::warn;
    use uuid::Uuid;

    const WEAVE_VERSION: u8 = 1;
    const COORD_SOCKET: &str = "coord.sock";
    const SESSIONS_DIR: &str = "sessions";
    const REQUEST_SRC: &str = "codex-cli";

    #[derive(Debug, Serialize, Deserialize)]
    struct WeaveErrorDetail {
        code: String,
        message: String,
        #[serde(default)]
        detail: Option<Value>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct WeaveAck {
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct WeaveEnvelope {
        v: u8,
        #[serde(rename = "type")]
        r#type: String,
        id: String,
        ts: String,
        src: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        dst: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        topic: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        corr: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WeaveErrorDetail>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ack: Option<WeaveAck>,
    }

    #[derive(Debug, Deserialize)]
    struct SessionListPayload {
        sessions: Vec<SessionListEntry>,
    }

    #[derive(Debug, Deserialize)]
    struct SessionListEntry {
        id: String,
        name: String,
    }

    #[derive(Debug, Deserialize)]
    struct AgentListPayload {
        agents: Vec<AgentListEntry>,
    }

    #[derive(Debug, Deserialize)]
    struct AgentListEntry {
        id: String,
        #[serde(default)]
        name: Option<String>,
    }

    pub struct WeaveAgentConnection {
        session_id: String,
        agent_id: String,
        agent_name: Arc<RwLock<String>>,
        seq: Arc<AtomicU64>,
        outgoing_tx: mpsc::UnboundedSender<WeaveOutgoingRequest>,
        incoming_rx: Option<mpsc::UnboundedReceiver<WeaveIncomingMessage>>,
        shutdown_tx: Option<oneshot::Sender<()>>,
    }

    impl std::fmt::Debug for WeaveAgentConnection {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("WeaveAgentConnection")
                .field("session_id", &self.session_id)
                .field("agent_id", &self.agent_id)
                .finish()
        }
    }

    impl WeaveAgentConnection {
        pub fn sender(&self) -> WeaveAgentSender {
            WeaveAgentSender {
                session_id: self.session_id.clone(),
                agent_id: self.agent_id.clone(),
                seq: Arc::clone(&self.seq),
                outgoing_tx: self.outgoing_tx.clone(),
            }
        }

        pub fn set_agent_name(&mut self, name: String) {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return;
            }
            if let Ok(mut guard) = self.agent_name.write() {
                *guard = trimmed.to_string();
            }
        }

        pub fn take_incoming_rx(
            &mut self,
        ) -> Option<mpsc::UnboundedReceiver<WeaveIncomingMessage>> {
            self.incoming_rx.take()
        }

        pub fn shutdown(&mut self) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
        }
    }

    impl Drop for WeaveAgentConnection {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
        }
    }

    pub async fn list_sessions() -> Result<Vec<WeaveSession>, String> {
        let socket_path = coord_socket_path(&resolve_weave_home()?);
        let request = new_envelope("session.list", None, None);
        let response = send_request(&socket_path, &request).await?;
        if let Some(message) = response_error(&response) {
            return Err(message);
        }
        let payload = response
            .payload
            .ok_or_else(|| "Weave session list response missing payload".to_string())?;
        let list: SessionListPayload = serde_json::from_value(payload)
            .map_err(|err| format!("Failed to parse Weave session list: {err}"))?;
        let sessions = list
            .sessions
            .into_iter()
            .map(|entry| {
                let trimmed = entry.name.trim();
                WeaveSession {
                    id: entry.id,
                    name: (!trimmed.is_empty()).then_some(trimmed.to_string()),
                }
            })
            .collect();
        Ok(sessions)
    }

    pub async fn create_session(name: Option<String>) -> Result<WeaveSession, String> {
        let socket_path = coord_socket_path(&resolve_weave_home()?);
        let payload = name.as_ref().map(|name| json!({ "name": name }));
        let request = new_envelope("session.create", None, payload);
        let response = send_request(&socket_path, &request).await?;
        if let Some(message) = response_error(&response) {
            return Err(message);
        }
        let session_id = response
            .session
            .ok_or_else(|| "Weave session.create response missing session id".to_string())?;
        Ok(WeaveSession {
            id: session_id,
            name,
        })
    }

    pub async fn close_session(session_id: &str) -> Result<(), String> {
        let weave_home = resolve_weave_home()?;
        let session_socket = session_socket_path(&weave_home, session_id);
        let socket_path = if session_socket.exists() {
            session_socket
        } else {
            coord_socket_path(&weave_home)
        };
        let request = new_envelope("session.close", Some(session_id.to_string()), None);
        let response = send_request(&socket_path, &request).await?;
        if let Some(message) = response_error(&response) {
            return Err(message);
        }
        Ok(())
    }

    pub async fn list_agents(session_id: &str, src: &str) -> Result<Vec<WeaveAgent>, String> {
        let weave_home = resolve_weave_home()?;
        let session_socket = session_socket_path(&weave_home, session_id);
        let socket_path = if session_socket.exists() {
            session_socket
        } else {
            coord_socket_path(&weave_home)
        };
        let request = new_envelope_with_src(
            "agent.list",
            src.to_string(),
            Some(session_id.to_string()),
            None,
        );
        let response = send_request(&socket_path, &request).await?;
        if let Some(message) = response_error(&response) {
            return Err(message);
        }
        let payload = response
            .payload
            .ok_or_else(|| "Weave agent list response missing payload".to_string())?;
        let list: AgentListPayload = serde_json::from_value(payload)
            .map_err(|err| format!("Failed to parse Weave agent list: {err}"))?;
        let agents = list
            .agents
            .into_iter()
            .map(|entry| {
                let name = entry
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(ToString::to_string);
                WeaveAgent { id: entry.id, name }
            })
            .collect();
        Ok(agents)
    }

    pub async fn connect_agent(
        session_id: String,
        agent_id: String,
        name: Option<String>,
    ) -> Result<WeaveAgentConnection, String> {
        let weave_home = resolve_weave_home()?;
        let session_socket = session_socket_path(&weave_home, &session_id);
        let socket_path = if session_socket.exists() {
            session_socket
        } else {
            coord_socket_path(&weave_home)
        };
        let stream = UnixStream::connect(&socket_path)
            .await
            .map_err(|err| format!("Failed to connect to Weave coordinator: {err}"))?;
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let payload = agent_add_payload(&agent_id, name.as_deref());
        let request = new_envelope_with_src(
            "agent.add",
            agent_id.clone(),
            Some(session_id.clone()),
            Some(payload),
        );
        send_envelope(&mut write_half, &request).await?;
        let response = read_response(&mut reader, request.id.as_str()).await?;
        if let Some(message) = response_error(&response) {
            return Err(message);
        }
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let seq = Arc::new(AtomicU64::new(0));
        let agent_name = name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| agent_id.clone());
        let agent_name = Arc::new(RwLock::new(agent_name));
        let session_id_for_task = session_id.clone();
        let agent_id_for_task = agent_id.clone();
        let agent_name_for_task = Arc::clone(&agent_name);
        let state = AgentConnectionState {
            session_id: session_id_for_task,
            agent_id: agent_id_for_task,
            agent_name: agent_name_for_task,
            outgoing_rx,
            incoming_tx,
        };
        let _task = tokio::spawn(async move {
            hold_agent_connection(reader, write_half, shutdown_rx, state).await;
        });
        Ok(WeaveAgentConnection {
            session_id,
            agent_id,
            agent_name,
            seq,
            outgoing_tx,
            incoming_rx: Some(incoming_rx),
            shutdown_tx: Some(shutdown_tx),
        })
    }

    fn resolve_weave_home() -> Result<PathBuf, String> {
        if let Ok(value) = env::var("WEAVE_HOME") {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err("WEAVE_HOME is set but empty".to_string());
            }
            return expand_home(trimmed);
        }
        let home = resolve_home_dir()?;
        Ok(home.join(".weave"))
    }

    fn expand_home(path: &str) -> Result<PathBuf, String> {
        if path == "~" || path.starts_with("~/") {
            let home = resolve_home_dir()?;
            if path == "~" {
                return Ok(home);
            }
            return Ok(home.join(&path[2..]));
        }
        Ok(PathBuf::from(path))
    }

    fn resolve_home_dir() -> Result<PathBuf, String> {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or_else(|| "Failed to resolve home directory".to_string())
    }

    fn coord_socket_path(weave_home: &Path) -> PathBuf {
        weave_home.join(COORD_SOCKET)
    }

    fn session_socket_path(weave_home: &Path, session_id: &str) -> PathBuf {
        weave_home
            .join(SESSIONS_DIR)
            .join(session_id)
            .join(COORD_SOCKET)
    }

    fn new_envelope(
        req_type: &str,
        session: Option<String>,
        payload: Option<Value>,
    ) -> WeaveEnvelope {
        new_envelope_with_src(req_type, REQUEST_SRC.to_string(), session, payload)
    }

    fn new_envelope_with_src(
        req_type: &str,
        src: String,
        session: Option<String>,
        payload: Option<Value>,
    ) -> WeaveEnvelope {
        WeaveEnvelope {
            v: WEAVE_VERSION,
            r#type: req_type.to_string(),
            id: Uuid::new_v4().to_string(),
            ts: now_timestamp(),
            src,
            dst: None,
            topic: None,
            session,
            seq: None,
            idempotency_key: None,
            corr: None,
            payload,
            payload_ref: None,
            content_type: None,
            size: None,
            meta: None,
            status: None,
            error: None,
            ack: None,
        }
    }

    fn now_timestamp() -> String {
        OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
    }

    fn response_error(response: &WeaveEnvelope) -> Option<String> {
        if response.status.as_deref() != Some("error") {
            return None;
        }
        let fallback = "Weave request failed".to_string();
        let Some(detail) = response.error.as_ref() else {
            return Some(fallback);
        };
        let code = detail.code.trim();
        let message = detail.message.trim();
        match (code.is_empty(), message.is_empty()) {
            (false, false) => Some(format!("{code}: {message}")),
            (false, true) => Some(code.to_string()),
            (true, false) => Some(message.to_string()),
            (true, true) => Some(fallback),
        }
    }

    async fn send_request(
        socket_path: &Path,
        request: &WeaveEnvelope,
    ) -> Result<WeaveEnvelope, String> {
        if !socket_path.exists() {
            let socket_path_display = socket_path.display();
            return Err(format!(
                "Weave coordinator socket not found at {socket_path_display}. Start it with `weave-service start`."
            ));
        }
        let mut stream = UnixStream::connect(socket_path)
            .await
            .map_err(|err| format!("Failed to connect to Weave coordinator: {err}"))?;
        let payload = serde_json::to_vec(request)
            .map_err(|err| format!("Failed to serialize Weave request: {err}"))?;
        stream
            .write_all(&payload)
            .await
            .map_err(|err| format!("Failed to write Weave request: {err}"))?;
        stream
            .write_all(b"\n")
            .await
            .map_err(|err| format!("Failed to write Weave request: {err}"))?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader
                .read_line(&mut line)
                .await
                .map_err(|err| format!("Failed to read Weave response: {err}"))?;
            if bytes == 0 {
                return Err("Weave coordinator closed the connection".to_string());
            }
            let response: WeaveEnvelope = serde_json::from_str(line.trim_end())
                .map_err(|err| format!("Failed to parse Weave response: {err}"))?;
            if response.corr.as_deref() == Some(request.id.as_str()) {
                return Ok(response);
            }
        }
    }

    async fn send_envelope(
        writer: &mut WriteHalf<UnixStream>,
        request: &WeaveEnvelope,
    ) -> Result<(), String> {
        let payload = serde_json::to_vec(request)
            .map_err(|err| format!("Failed to serialize Weave request: {err}"))?;
        writer
            .write_all(&payload)
            .await
            .map_err(|err| format!("Failed to write Weave request: {err}"))?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|err| format!("Failed to write Weave request: {err}"))?;
        Ok(())
    }

    async fn read_response(
        reader: &mut BufReader<ReadHalf<UnixStream>>,
        request_id: &str,
    ) -> Result<WeaveEnvelope, String> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader
                .read_line(&mut line)
                .await
                .map_err(|err| format!("Failed to read Weave response: {err}"))?;
            if bytes == 0 {
                return Err("Weave coordinator closed the connection".to_string());
            }
            let response: WeaveEnvelope = serde_json::from_str(line.trim_end())
                .map_err(|err| format!("Failed to parse Weave response: {err}"))?;
            if response.corr.as_deref() == Some(request_id) {
                return Ok(response);
            }
        }
    }

    fn agent_add_payload(agent_id: &str, name: Option<&str>) -> Value {
        let trimmed = name.map(str::trim).filter(|name| !name.is_empty());
        match trimmed {
            Some(name) => json!({ "id": agent_id, "name": name }),
            None => json!({ "id": agent_id }),
        }
    }

    fn agent_update_payload(agent_id: &str, name: &str) -> Value {
        json!({ "id": agent_id, "name": name })
    }

    fn kind_label(kind: super::WeaveMessageKind) -> &'static str {
        match kind {
            super::WeaveMessageKind::User => "user",
            super::WeaveMessageKind::Reply => "reply",
            super::WeaveMessageKind::Control => "control",
        }
    }

    fn action_message_payload(
        dst: &str,
        text: &str,
        action_id: &str,
        action_index: usize,
        reply_to_action_id: Option<&str>,
        kind: Option<super::WeaveMessageKind>,
        reply_policy: Option<&str>,
    ) -> Value {
        let mut payload = serde_json::Map::new();
        payload.insert("type".to_string(), json!("message"));
        payload.insert("dst".to_string(), json!(dst));
        if let Some(reply_to_action_id) = reply_to_action_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            payload.insert("reply_to_action_id".to_string(), json!(reply_to_action_id));
        }
        if let Some(kind) = kind {
            payload.insert("kind".to_string(), json!(kind_label(kind)));
        }
        if let Some(reply_policy) = reply_policy
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            payload.insert("reply_policy".to_string(), json!(reply_policy));
        }
        payload.insert("text".to_string(), json!(text));
        payload.insert("action_id".to_string(), json!(action_id));
        payload.insert("action_index".to_string(), json!(action_index));
        Value::Object(payload)
    }

    fn action_submit_payload(
        group_id: &str,
        actions: Vec<Value>,
        metadata: Option<&super::WeaveMessageMetadata>,
    ) -> Value {
        let mut payload = serde_json::Map::new();
        payload.insert("group_id".to_string(), json!(group_id));
        payload.insert("actions".to_string(), Value::Array(actions));
        if let Some(metadata) = metadata {
            let mut context = serde_json::Map::new();
            let conversation_id = metadata.conversation_id.trim();
            if !conversation_id.is_empty() {
                context.insert("context_id".to_string(), json!(conversation_id));
            }
            let owner_id = metadata.conversation_owner.trim();
            if !owner_id.is_empty() {
                context.insert("owner_id".to_string(), json!(owner_id));
            }
            if let Some(parent_message_id) = metadata
                .parent_message_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                context.insert("parent_message_id".to_string(), json!(parent_message_id));
            }
            if !context.is_empty() {
                payload.insert("context".to_string(), Value::Object(context));
            }
            if let Some(task_id) = metadata
                .task_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                payload.insert("task_id".to_string(), json!(task_id));
            }
        }
        Value::Object(payload)
    }

    fn action_result_payload(result: WeaveActionResult) -> Value {
        let WeaveActionResult {
            group_id,
            action_id,
            action_index,
            status,
            detail,
            new_context_id,
            new_task_id,
        } = result;
        let mut payload = serde_json::Map::new();
        payload.insert("group_id".to_string(), json!(group_id));
        payload.insert("action_id".to_string(), json!(action_id));
        payload.insert("action_index".to_string(), json!(action_index));
        payload.insert("status".to_string(), json!(status));
        if let Some(detail) = detail {
            let trimmed = detail.trim();
            if !trimmed.is_empty() {
                payload.insert("detail".to_string(), json!(trimmed));
            }
        }
        if new_context_id.is_some() || new_task_id.is_some() {
            let mut data = serde_json::Map::new();
            if let Some(context_id) = new_context_id {
                let trimmed = context_id.trim();
                if !trimmed.is_empty() {
                    data.insert("new_context_id".to_string(), json!(trimmed));
                }
            }
            if let Some(task_id) = new_task_id {
                let trimmed = task_id.trim();
                if !trimmed.is_empty() {
                    data.insert("new_task_id".to_string(), json!(trimmed));
                }
            }
            if !data.is_empty() {
                payload.insert("data".to_string(), Value::Object(data));
            }
        }
        Value::Object(payload)
    }

    #[derive(Debug)]
    struct WeaveOutgoingRequest {
        envelope: WeaveEnvelope,
        response_tx: oneshot::Sender<Result<WeaveEnvelope, String>>,
    }

    struct AgentConnectionState {
        session_id: String,
        agent_id: String,
        agent_name: Arc<RwLock<String>>,
        outgoing_rx: mpsc::UnboundedReceiver<WeaveOutgoingRequest>,
        incoming_tx: mpsc::UnboundedSender<WeaveIncomingMessage>,
    }

    struct IncomingMessageContext {
        session_id: String,
        message_id: String,
        src: String,
        src_name: Option<String>,
        meta: Option<Value>,
    }

    #[derive(Clone, Debug)]
    pub struct WeaveAgentSender {
        session_id: String,
        agent_id: String,
        seq: Arc<AtomicU64>,
        outgoing_tx: mpsc::UnboundedSender<WeaveOutgoingRequest>,
    }

    impl WeaveAgentSender {
        fn next_seq(&self) -> u64 {
            self.seq.fetch_add(1, Ordering::Relaxed).saturating_add(1)
        }

        pub async fn send_reply_with_metadata(
            &self,
            dst: String,
            text: String,
            metadata: Option<&super::WeaveMessageMetadata>,
            reply_to_action_id: Option<&str>,
        ) -> Result<(), String> {
            let group_id = Uuid::new_v4().to_string();
            let action_id = Uuid::new_v4().to_string();
            let actions = vec![action_message_payload(
                dst.as_str(),
                text.as_str(),
                &action_id,
                0,
                reply_to_action_id,
                Some(super::WeaveMessageKind::Reply),
                Some("none"),
            )];
            let payload = action_submit_payload(&group_id, actions, metadata);
            self.send_action_submit(payload).await
        }

        pub async fn update_agent_name(&self, name: String) -> Result<(), String> {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err("Weave agent name is empty".to_string());
            }
            let payload = agent_update_payload(&self.agent_id, trimmed);
            let request = new_envelope_with_src(
                "agent.update",
                self.agent_id.clone(),
                Some(self.session_id.clone()),
                Some(payload),
            );
            let response = self.send_request(request).await?;
            if let Some(message) = response_error(&response) {
                return Err(message);
            }
            Ok(())
        }

        pub async fn send_action_submit(&self, payload: Value) -> Result<(), String> {
            let mut request = new_envelope_with_src(
                "action.submit",
                self.agent_id.clone(),
                Some(self.session_id.clone()),
                Some(payload),
            );
            request.seq = Some(self.next_seq());
            request.idempotency_key = request
                .payload
                .as_ref()
                .and_then(|payload| payload.get("group_id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let response = self.send_request(request).await?;
            if let Some(message) = response_error(&response) {
                return Err(message);
            }
            Ok(())
        }

        pub async fn send_action_result(
            &self,
            dst: String,
            result: WeaveActionResult,
        ) -> Result<(), String> {
            let payload = action_result_payload(result);
            let mut request = new_envelope_with_src(
                "action.result",
                self.agent_id.clone(),
                Some(self.session_id.clone()),
                Some(payload),
            );
            request.dst = Some(dst);
            request.seq = Some(self.next_seq());
            let response = self.send_request(request).await?;
            if let Some(message) = response_error(&response) {
                return Err(message);
            }
            Ok(())
        }

        async fn send_request(&self, request: WeaveEnvelope) -> Result<WeaveEnvelope, String> {
            let (response_tx, response_rx) = oneshot::channel();
            self.outgoing_tx
                .send(WeaveOutgoingRequest {
                    envelope: request,
                    response_tx,
                })
                .map_err(|_| "Weave agent connection closed".to_string())?;
            response_rx
                .await
                .map_err(|_| "Weave agent connection closed".to_string())?
        }
    }

    async fn hold_agent_connection(
        mut reader: BufReader<ReadHalf<UnixStream>>,
        mut writer: WriteHalf<UnixStream>,
        mut shutdown_rx: oneshot::Receiver<()>,
        state: AgentConnectionState,
    ) {
        let AgentConnectionState {
            session_id,
            agent_id,
            agent_name,
            mut outgoing_rx,
            incoming_tx,
        } = state;
        let mut line = String::new();
        let mut pending: HashMap<String, oneshot::Sender<Result<WeaveEnvelope, String>>> =
            HashMap::new();
        loop {
            line.clear();
            tokio::select! {
                _ = &mut shutdown_rx => {
                    let request = new_envelope_with_src(
                        "agent.remove",
                        agent_id.clone(),
                        Some(session_id.clone()),
                        Some(json!({ "id": agent_id.clone() })),
                    );
                    let _ = send_envelope(&mut writer, &request).await;
                    let _ = read_response(&mut reader, request.id.as_str()).await;
                    break;
                }
                request = outgoing_rx.recv() => {
                    let Some(request) = request else {
                        break;
                    };
                    let request_id = request.envelope.id.clone();
                    pending.insert(request_id.clone(), request.response_tx);
                    if let Err(err) = send_envelope(&mut writer, &request.envelope).await
                        && let Some(response_tx) = pending.remove(&request_id) {
                            let _ = response_tx.send(Err(err));
                        }
                }
                result = reader.read_line(&mut line) => {
                    match result {
                        Ok(0) => break,
                        Ok(_) => {
                            let response: WeaveEnvelope = match serde_json::from_str(line.trim_end()) {
                                Ok(response) => response,
                                Err(_) => continue,
                            };
                            if let Some(corr) = response.corr.as_deref()
                                && let Some(response_tx) = pending.remove(corr)
                            {
                                let _ = response_tx.send(Ok(response));
                                continue;
                            }
                            let current_agent_name = agent_name
                                .read()
                                .map(|guard| guard.clone())
                                .unwrap_or_else(|_| agent_id.clone());
                            if let Some(ack) = build_message_ack_request(
                                &response,
                                &agent_id,
                                current_agent_name.as_str(),
                                &session_id,
                            ) && let Err(err) = send_envelope(&mut writer, &ack).await {
                                    warn!(error = %err, "Failed to acknowledge Weave message");
                                }
                            for message in build_incoming_messages(
                                &response,
                                &agent_id,
                                current_agent_name.as_str(),
                            ) {
                                let _ = incoming_tx.send(message);
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        for (_, response_tx) in pending {
            let _ = response_tx.send(Err("Weave agent connection closed".to_string()));
        }
    }

    fn build_message_ack_request(
        envelope: &WeaveEnvelope,
        agent_id: &str,
        agent_name: &str,
        session_id: &str,
    ) -> Option<WeaveEnvelope> {
        if !should_ack_message(envelope, agent_id, agent_name) {
            return None;
        }
        let corr = envelope.corr.as_ref().unwrap_or(&envelope.id).to_string();
        let mut request = new_envelope_with_src(
            "message.ack",
            agent_id.to_string(),
            Some(session_id.to_string()),
            None,
        );
        request.corr = Some(corr);
        Some(request)
    }

    fn should_ack_message(envelope: &WeaveEnvelope, agent_id: &str, agent_name: &str) -> bool {
        if envelope.r#type != "message.send" {
            return false;
        }
        if envelope.src == agent_id || envelope.src == agent_name {
            return false;
        }
        ack_requests_auto(envelope.ack.as_ref())
            && is_direct_inbox_for_agent(envelope, agent_id, agent_name)
    }

    fn ack_requests_auto(ack: Option<&WeaveAck>) -> bool {
        matches!(ack.and_then(|ack| ack.mode.as_deref()), Some("auto"))
    }

    fn build_incoming_messages(
        envelope: &WeaveEnvelope,
        agent_id: &str,
        agent_name: &str,
    ) -> Vec<WeaveIncomingMessage> {
        if envelope.src == agent_id || envelope.src == agent_name {
            return Vec::new();
        }
        let session_id = match envelope.session.as_ref() {
            Some(session_id) => session_id.clone(),
            None => return Vec::new(),
        };
        let payload = envelope.payload.as_ref();
        let ctx = IncomingMessageContext {
            session_id,
            message_id: envelope.id.clone(),
            src: envelope.src.clone(),
            src_name: meta_origin_src_name(envelope.meta.as_ref()),
            meta: envelope.meta.clone(),
        };
        match envelope.r#type.as_str() {
            "action.submit" => payload
                .map(|payload| build_action_dispatch_messages(payload, &ctx, agent_id, agent_name))
                .unwrap_or_default(),
            _ if !is_direct_message_for_agent(envelope, agent_id, agent_name) => Vec::new(),
            "action.result" => payload
                .and_then(|payload| build_action_result_message(payload, &ctx))
                .into_iter()
                .collect(),
            "message.created" | "message.send" => build_message_payload_message(envelope, &ctx)
                .into_iter()
                .collect(),
            "task.updated" => payload
                .and_then(|payload| build_task_update_message(payload, &ctx))
                .into_iter()
                .collect(),
            "task.done" => payload
                .and_then(|payload| build_task_done_message(payload, &ctx))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn is_direct_message_for_agent(
        envelope: &WeaveEnvelope,
        agent_id: &str,
        agent_name: &str,
    ) -> bool {
        match envelope.dst.as_deref() {
            Some(dst) => dst == agent_id || dst == agent_name,
            None => match envelope.topic.as_deref() {
                None => true,
                Some(topic) => agent_inbox_target(topic)
                    .is_some_and(|target| target == agent_id || target == agent_name),
            },
        }
    }

    fn is_direct_inbox_for_agent(
        envelope: &WeaveEnvelope,
        agent_id: &str,
        agent_name: &str,
    ) -> bool {
        match envelope.dst.as_deref() {
            Some(dst) => dst == agent_id || dst == agent_name,
            None => envelope
                .topic
                .as_deref()
                .and_then(|topic| agent_inbox_target(topic))
                .is_some_and(|target| target == agent_id || target == agent_name),
        }
    }

    fn agent_inbox_target(topic: &str) -> Option<&str> {
        let mut segments = topic.split('.');
        if segments.next()? != "agent" {
            return None;
        }
        let agent = segments.next()?;
        if segments.next()? != "inbox" {
            return None;
        }
        if segments.next().is_some() {
            return None;
        }
        Some(agent)
    }

    fn payload_text(payload: Option<&Value>) -> Option<String> {
        let payload = payload?;
        if let Some(text) = payload.as_str() {
            return Some(text.to_string());
        }
        let map = payload.as_object()?;
        map.get("text")
            .and_then(Value::as_str)
            .map(std::string::ToString::to_string)
    }

    fn payload_kind(payload: Option<&Value>) -> super::WeaveMessageKind {
        let Some(payload) = payload else {
            return super::WeaveMessageKind::User;
        };
        let Some(map) = payload.as_object() else {
            return super::WeaveMessageKind::User;
        };
        let Some(kind) = map.get("kind").and_then(Value::as_str) else {
            return super::WeaveMessageKind::User;
        };
        match kind {
            "reply" => super::WeaveMessageKind::Reply,
            "control" => super::WeaveMessageKind::Control,
            _ => super::WeaveMessageKind::User,
        }
    }

    fn meta_origin_src_name(meta: Option<&Value>) -> Option<String> {
        let meta = meta?.as_object()?;
        let weave = meta.get("weave")?.as_object()?;
        let name = weave.get("origin_src_name")?.as_str()?.trim();
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }

    fn payload_conversation_id(payload: Option<&Value>) -> Option<String> {
        let payload = payload?;
        let map = payload.as_object()?;
        let id = map.get("context_id")?.as_str()?;
        let id = id.trim();
        if id.is_empty() {
            None
        } else {
            Some(id.to_string())
        }
    }

    fn payload_conversation_owner(payload: Option<&Value>) -> Option<String> {
        let payload = payload?;
        let map = payload.as_object()?;
        let owner = map.get("owner_id")?.as_str()?;
        let owner = owner.trim();
        if owner.is_empty() {
            None
        } else {
            Some(owner.to_string())
        }
    }

    fn payload_parent_message_id(payload: Option<&Value>) -> Option<String> {
        let payload = payload?;
        let map = payload.as_object()?;
        let parent = map.get("parent_message_id")?.as_str()?;
        let parent = parent.trim();
        if parent.is_empty() {
            None
        } else {
            Some(parent.to_string())
        }
    }

    fn payload_reply_to_action_id(payload: Option<&Value>) -> Option<String> {
        let payload = payload?;
        let map = payload.as_object()?;
        let reply_to_action_id = map.get("reply_to_action_id")?.as_str()?;
        let reply_to_action_id = reply_to_action_id.trim();
        if reply_to_action_id.is_empty() {
            None
        } else {
            Some(reply_to_action_id.to_string())
        }
    }

    fn payload_task_id(payload: Option<&Value>) -> Option<String> {
        let payload = payload?;
        let map = payload.as_object()?;
        let task_id = map.get("task_id")?.as_str()?;
        let task_id = task_id.trim();
        if task_id.is_empty() {
            None
        } else {
            Some(task_id.to_string())
        }
    }

    fn message_payload_text(envelope: &WeaveEnvelope, payload: Option<&Value>) -> Option<String> {
        payload_text(payload).or_else(|| {
            envelope
                .payload_ref
                .as_deref()
                .map(|payload_ref| format!("[payload_ref: {payload_ref}]"))
        })
    }

    fn build_message_payload_message(
        envelope: &WeaveEnvelope,
        ctx: &IncomingMessageContext,
    ) -> Option<WeaveIncomingMessage> {
        let payload = envelope.payload.as_ref();
        let text = message_payload_text(envelope, payload)?;
        let kind = payload_kind(payload);
        let conversation_id =
            payload_conversation_id(payload).unwrap_or_else(|| ctx.message_id.clone());
        let conversation_owner =
            payload_conversation_owner(payload).unwrap_or_else(|| ctx.src.clone());
        let parent_message_id = payload_parent_message_id(payload);
        let reply_to_action_id = payload_reply_to_action_id(payload);
        let task_id = payload_task_id(payload);
        let has_conversation_metadata = payload_conversation_id(payload).is_some()
            || payload_conversation_owner(payload).is_some()
            || parent_message_id.is_some()
            || task_id.is_some();
        Some(WeaveIncomingMessage {
            session_id: ctx.session_id.clone(),
            message_id: ctx.message_id.clone(),
            src: ctx.src.clone(),
            src_name: ctx.src_name.clone(),
            meta: ctx.meta.clone(),
            text,
            kind,
            conversation_id,
            conversation_owner,
            parent_message_id,
            task_id,
            tool: None,
            action_group_id: None,
            action_id: None,
            action_index: None,
            reply_to_action_id,
            action_result: None,
            task_update: None,
            task_done: None,
            defer_until_ready: false,
            has_conversation_metadata,
        })
    }

    fn build_action_dispatch_messages(
        payload: &Value,
        ctx: &IncomingMessageContext,
        agent_id: &str,
        agent_name: &str,
    ) -> Vec<WeaveIncomingMessage> {
        let map = match payload.as_object() {
            Some(map) => map,
            None => return Vec::new(),
        };
        let group_id = map
            .get("group_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| ctx.message_id.clone());
        let context = map.get("context").and_then(Value::as_object);
        let conversation_id = context
            .and_then(|ctx| ctx.get("context_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| ctx.message_id.clone());
        let conversation_owner = context
            .and_then(|ctx| ctx.get("owner_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| ctx.src.clone());
        let parent_message_id = context
            .and_then(|ctx| ctx.get("parent_message_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let task_id = map
            .get("task_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let has_conversation_metadata =
            context.is_some() || parent_message_id.is_some() || task_id.is_some();
        let actions = match map.get("actions").and_then(Value::as_array) {
            Some(actions) => actions,
            None => return Vec::new(),
        };
        let mut messages = Vec::new();
        let mut defer_messages = false;
        for (idx, action) in actions.iter().enumerate() {
            let Some(action_map) = action.as_object() else {
                continue;
            };
            let Some(action_dst) = action_map
                .get("dst")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if action_dst != agent_id && action_dst != agent_name {
                continue;
            }
            let action_type = action_map
                .get("type")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            let action_index = action_map
                .get("action_index")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(idx);
            let action_id = action_map
                .get("action_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let reply_to_action_id = action_map
                .get("reply_to_action_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            match action_type {
                "message" => {
                    let Some(text) = action_map.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    let text = text.to_string();
                    let kind = action_map
                        .get("kind")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|value| match value {
                            "reply" => super::WeaveMessageKind::Reply,
                            "control" => super::WeaveMessageKind::Control,
                            _ => super::WeaveMessageKind::User,
                        })
                        .unwrap_or(super::WeaveMessageKind::User);
                    let message = WeaveIncomingMessage {
                        session_id: ctx.session_id.clone(),
                        message_id: ctx.message_id.clone(),
                        src: ctx.src.clone(),
                        src_name: ctx.src_name.clone(),
                        meta: ctx.meta.clone(),
                        text,
                        kind,
                        conversation_id: conversation_id.clone(),
                        conversation_owner: conversation_owner.clone(),
                        parent_message_id: parent_message_id.clone(),
                        task_id: task_id.clone(),
                        tool: None,
                        action_group_id: Some(group_id.clone()),
                        action_id: action_id.clone(),
                        action_index: Some(action_index),
                        reply_to_action_id: reply_to_action_id.clone(),
                        action_result: None,
                        task_update: None,
                        task_done: None,
                        defer_until_ready: defer_messages,
                        has_conversation_metadata,
                    };
                    messages.push(message);
                }
                "control" => {
                    let command = action_map
                        .get("command")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("");
                    let args = action_map
                        .get("args")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty());
                    let tool = super::WeaveTool::from_command(command, args);
                    if tool.is_none() {
                        if command.is_empty() {
                            warn!(
                                src = %ctx.src,
                                action_group_id = %group_id,
                                "Weave control action missing command"
                            );
                        } else {
                            warn!(
                                src = %ctx.src,
                                action_group_id = %group_id,
                                command = %command,
                                "Unknown Weave control command"
                            );
                        }
                        continue;
                    }
                    if matches!(tool, Some(super::WeaveTool::NewSession)) {
                        defer_messages = true;
                    }
                    let message = WeaveIncomingMessage {
                        session_id: ctx.session_id.clone(),
                        message_id: ctx.message_id.clone(),
                        src: ctx.src.clone(),
                        src_name: ctx.src_name.clone(),
                        meta: ctx.meta.clone(),
                        text: String::new(),
                        kind: super::WeaveMessageKind::Control,
                        conversation_id: conversation_id.clone(),
                        conversation_owner: conversation_owner.clone(),
                        parent_message_id: parent_message_id.clone(),
                        task_id: task_id.clone(),
                        tool,
                        action_group_id: Some(group_id.clone()),
                        action_id: action_id.clone(),
                        action_index: Some(action_index),
                        reply_to_action_id: reply_to_action_id.clone(),
                        action_result: None,
                        task_update: None,
                        task_done: None,
                        defer_until_ready: false,
                        has_conversation_metadata,
                    };
                    messages.push(message);
                }
                _ => {}
            }
        }
        messages
    }

    fn build_action_result_message(
        payload: &Value,
        ctx: &IncomingMessageContext,
    ) -> Option<WeaveIncomingMessage> {
        let map = payload.as_object()?;
        let group_id = map.get("group_id")?.as_str()?.trim();
        let action_id = map.get("action_id")?.as_str()?.trim();
        let status = map.get("status")?.as_str()?.trim();
        if group_id.is_empty() || action_id.is_empty() || status.is_empty() {
            return None;
        }
        let action_index = map.get("action_index")?.as_u64()? as usize;
        let detail = map
            .get("detail")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let action_result = super::WeaveActionResult {
            group_id: group_id.to_string(),
            action_id: action_id.to_string(),
            action_index,
            status: status.to_string(),
            detail,
            new_context_id: map
                .get("data")
                .and_then(Value::as_object)
                .and_then(|data| data.get("new_context_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            new_task_id: map
                .get("data")
                .and_then(Value::as_object)
                .and_then(|data| data.get("new_task_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        };
        Some(WeaveIncomingMessage {
            session_id: ctx.session_id.clone(),
            message_id: ctx.message_id.clone(),
            src: ctx.src.clone(),
            src_name: ctx.src_name.clone(),
            meta: ctx.meta.clone(),
            text: String::new(),
            kind: super::WeaveMessageKind::Control,
            conversation_id: ctx.message_id.clone(),
            conversation_owner: ctx.src.clone(),
            parent_message_id: None,
            task_id: None,
            tool: None,
            action_group_id: Some(action_result.group_id.clone()),
            action_id: Some(action_result.action_id.clone()),
            action_index: Some(action_result.action_index),
            reply_to_action_id: None,
            action_result: Some(action_result),
            task_update: None,
            task_done: None,
            defer_until_ready: false,
            has_conversation_metadata: false,
        })
    }

    fn build_task_update_message(
        payload: &Value,
        ctx: &IncomingMessageContext,
    ) -> Option<WeaveIncomingMessage> {
        let map = payload.as_object()?;
        let task_id = map.get("task_id")?.as_str()?.trim();
        let status = map.get("status")?.as_str()?.trim();
        if task_id.is_empty() || status.is_empty() {
            return None;
        }
        let conversation_id =
            payload_conversation_id(Some(payload)).unwrap_or_else(|| ctx.message_id.clone());
        let conversation_owner =
            payload_conversation_owner(Some(payload)).unwrap_or_else(|| ctx.src.clone());
        let parent_message_id = payload_parent_message_id(Some(payload));
        let has_conversation_metadata = payload_conversation_id(Some(payload)).is_some()
            || payload_conversation_owner(Some(payload)).is_some()
            || parent_message_id.is_some();
        let task_update = super::WeaveTaskUpdate {
            task_id: task_id.to_string(),
            status: status.to_string(),
        };
        Some(WeaveIncomingMessage {
            session_id: ctx.session_id.clone(),
            message_id: ctx.message_id.clone(),
            src: ctx.src.clone(),
            src_name: None,
            meta: ctx.meta.clone(),
            text: String::new(),
            kind: super::WeaveMessageKind::Control,
            conversation_id,
            conversation_owner,
            parent_message_id,
            task_id: Some(task_id.to_string()),
            tool: None,
            action_group_id: None,
            action_id: None,
            action_index: None,
            reply_to_action_id: None,
            action_result: None,
            task_update: Some(task_update),
            task_done: None,
            defer_until_ready: false,
            has_conversation_metadata,
        })
    }

    fn build_task_done_message(
        payload: &Value,
        ctx: &IncomingMessageContext,
    ) -> Option<WeaveIncomingMessage> {
        let map = payload.as_object()?;
        let task_id = map.get("task_id")?.as_str()?.trim();
        if task_id.is_empty() {
            return None;
        }
        let conversation_id =
            payload_conversation_id(Some(payload)).unwrap_or_else(|| ctx.message_id.clone());
        let conversation_owner =
            payload_conversation_owner(Some(payload)).unwrap_or_else(|| ctx.src.clone());
        let parent_message_id = payload_parent_message_id(Some(payload));
        let has_conversation_metadata = payload_conversation_id(Some(payload)).is_some()
            || payload_conversation_owner(Some(payload)).is_some()
            || parent_message_id.is_some();
        let summary = map
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let task_done = super::WeaveTaskDone {
            task_id: task_id.to_string(),
            summary,
        };
        Some(WeaveIncomingMessage {
            session_id: ctx.session_id.clone(),
            message_id: ctx.message_id.clone(),
            src: ctx.src.clone(),
            src_name: None,
            meta: ctx.meta.clone(),
            text: String::new(),
            kind: super::WeaveMessageKind::Control,
            conversation_id,
            conversation_owner,
            parent_message_id,
            task_id: Some(task_id.to_string()),
            tool: None,
            action_group_id: None,
            action_id: None,
            action_index: None,
            reply_to_action_id: None,
            action_result: None,
            task_update: None,
            task_done: Some(task_done),
            defer_until_ready: false,
            has_conversation_metadata,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::super::WeaveIncomingMessage;
        use super::super::WeaveMessageKind;
        use super::super::WeaveTool;
        use super::WEAVE_VERSION;
        use super::WeaveAck;
        use super::WeaveEnvelope;
        use super::build_incoming_messages;
        use super::build_message_ack_request;
        use pretty_assertions::assert_eq;
        use serde_json::json;

        fn base_envelope() -> WeaveEnvelope {
            WeaveEnvelope {
                v: WEAVE_VERSION,
                r#type: "message.send".to_string(),
                id: "msg-1".to_string(),
                ts: "2024-01-01T00:00:00Z".to_string(),
                src: "src-1".to_string(),
                dst: None,
                topic: None,
                session: Some("session-1".to_string()),
                seq: None,
                idempotency_key: None,
                corr: None,
                payload: None,
                payload_ref: None,
                content_type: None,
                size: None,
                meta: None,
                status: None,
                error: None,
                ack: None,
            }
        }

        #[test]
        fn message_ack_requires_auto_inbox() {
            let mut envelope = base_envelope();
            envelope.src = "agent-2".to_string();
            envelope.topic = Some("agent.agent-1.inbox".to_string());
            envelope.ack = Some(WeaveAck {
                mode: Some("auto".to_string()),
                timeout_ms: None,
            });

            let ack = build_message_ack_request(&envelope, "agent-1", "agent-name", "session-1")
                .expect("expected ack request");
            assert_eq!(ack.r#type, "message.ack");
            assert_eq!(ack.src, "agent-1");
            assert_eq!(ack.session.as_deref(), Some("session-1"));
            assert_eq!(ack.corr.as_deref(), Some("msg-1"));

            envelope.ack = Some(WeaveAck {
                mode: Some("manual".to_string()),
                timeout_ms: None,
            });
            assert!(
                build_message_ack_request(&envelope, "agent-1", "agent-name", "session-1")
                    .is_none()
            );
        }

        #[test]
        fn action_submit_defers_messages_after_new_session() {
            let mut envelope = base_envelope();
            envelope.r#type = "action.submit".to_string();
            envelope.src = "router".to_string();
            envelope.meta = Some(json!({
                "weave": { "origin_src_name": "coordinator" }
            }));
            envelope.payload = Some(json!({
                "group_id": "group-1",
                "task_id": "task-1",
                "context": {
                    "context_id": "ctx-1",
                    "owner_id": "owner-1",
                    "parent_message_id": "parent-1"
                },
                "actions": [
                    {
                        "type": "message",
                        "dst": "agent-1",
                        "text": "before",
                        "action_id": "action-1",
                        "action_index": 0
                    },
                    {
                        "type": "control",
                        "dst": "agent-1",
                        "command": "new",
                        "action_id": "action-2",
                        "action_index": 1
                    },
                    {
                        "type": "message",
                        "dst": "agent-1",
                        "text": "after",
                        "action_id": "action-3",
                        "action_index": 2
                    }
                ]
            }));

            let messages = build_incoming_messages(&envelope, "agent-1", "agent-name");
            let expected = vec![
                WeaveIncomingMessage {
                    session_id: "session-1".to_string(),
                    message_id: "msg-1".to_string(),
                    src: "router".to_string(),
                    src_name: Some("coordinator".to_string()),
                    meta: envelope.meta.clone(),
                    text: "before".to_string(),
                    kind: WeaveMessageKind::User,
                    conversation_id: "ctx-1".to_string(),
                    conversation_owner: "owner-1".to_string(),
                    parent_message_id: Some("parent-1".to_string()),
                    task_id: Some("task-1".to_string()),
                    tool: None,
                    action_group_id: Some("group-1".to_string()),
                    action_id: Some("action-1".to_string()),
                    action_index: Some(0),
                    reply_to_action_id: None,
                    action_result: None,
                    task_update: None,
                    task_done: None,
                    defer_until_ready: false,
                    has_conversation_metadata: true,
                },
                WeaveIncomingMessage {
                    session_id: "session-1".to_string(),
                    message_id: "msg-1".to_string(),
                    src: "router".to_string(),
                    src_name: Some("coordinator".to_string()),
                    meta: envelope.meta.clone(),
                    text: String::new(),
                    kind: WeaveMessageKind::Control,
                    conversation_id: "ctx-1".to_string(),
                    conversation_owner: "owner-1".to_string(),
                    parent_message_id: Some("parent-1".to_string()),
                    task_id: Some("task-1".to_string()),
                    tool: Some(WeaveTool::NewSession),
                    action_group_id: Some("group-1".to_string()),
                    action_id: Some("action-2".to_string()),
                    action_index: Some(1),
                    reply_to_action_id: None,
                    action_result: None,
                    task_update: None,
                    task_done: None,
                    defer_until_ready: false,
                    has_conversation_metadata: true,
                },
                WeaveIncomingMessage {
                    session_id: "session-1".to_string(),
                    message_id: "msg-1".to_string(),
                    src: "router".to_string(),
                    src_name: Some("coordinator".to_string()),
                    meta: envelope.meta,
                    text: "after".to_string(),
                    kind: WeaveMessageKind::User,
                    conversation_id: "ctx-1".to_string(),
                    conversation_owner: "owner-1".to_string(),
                    parent_message_id: Some("parent-1".to_string()),
                    task_id: Some("task-1".to_string()),
                    tool: None,
                    action_group_id: Some("group-1".to_string()),
                    action_id: Some("action-3".to_string()),
                    action_index: Some(2),
                    reply_to_action_id: None,
                    action_result: None,
                    task_update: None,
                    task_done: None,
                    defer_until_ready: true,
                    has_conversation_metadata: true,
                },
            ];

            assert_eq!(messages, expected);
        }

        #[test]
        fn message_payload_ref_does_not_override_empty_text() {
            let mut envelope = base_envelope();
            envelope.src = "agent-2".to_string();
            envelope.dst = Some("agent-1".to_string());
            envelope.payload_ref = Some("blob-1".to_string());
            envelope.payload = Some(json!({ "text": "" }));

            let messages = build_incoming_messages(&envelope, "agent-1", "agent-name");
            let expected = vec![WeaveIncomingMessage {
                session_id: "session-1".to_string(),
                message_id: "msg-1".to_string(),
                src: "agent-2".to_string(),
                src_name: None,
                meta: None,
                text: String::new(),
                kind: WeaveMessageKind::User,
                conversation_id: "msg-1".to_string(),
                conversation_owner: "agent-2".to_string(),
                parent_message_id: None,
                task_id: None,
                tool: None,
                action_group_id: None,
                action_id: None,
                action_index: None,
                reply_to_action_id: None,
                action_result: None,
                task_update: None,
                task_done: None,
                defer_until_ready: false,
                has_conversation_metadata: false,
            }];

            assert_eq!(messages, expected);
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use super::WeaveActionResult;
    use super::WeaveAgent;
    use super::WeaveIncomingMessage;
    use super::WeaveSession;
    use serde_json::Value;
    use tokio::sync::mpsc;

    pub struct WeaveAgentConnection;

    #[derive(Clone, Debug)]
    pub struct WeaveAgentSender;

    impl std::fmt::Debug for WeaveAgentConnection {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("WeaveAgentConnection").finish()
        }
    }

    impl WeaveAgentConnection {
        pub fn sender(&self) -> WeaveAgentSender {
            WeaveAgentSender
        }

        pub fn set_agent_name(&mut self, _name: String) {}

        pub fn shutdown(&mut self) {}

        pub fn take_incoming_rx(
            &mut self,
        ) -> Option<mpsc::UnboundedReceiver<WeaveIncomingMessage>> {
            None
        }
    }

    impl WeaveAgentSender {
        pub async fn send_reply_with_metadata(
            &self,
            _dst: String,
            _text: String,
            _metadata: Option<&super::WeaveMessageMetadata>,
            _reply_to_action_id: Option<&str>,
        ) -> Result<(), String> {
            Err("Weave sessions are only supported on Unix platforms.".to_string())
        }

        pub async fn update_agent_name(&self, _name: String) -> Result<(), String> {
            Err("Weave sessions are only supported on Unix platforms.".to_string())
        }

        pub async fn send_action_submit(&self, _payload: Value) -> Result<(), String> {
            Err("Weave sessions are only supported on Unix platforms.".to_string())
        }

        pub async fn send_action_result(
            &self,
            _dst: String,
            _result: WeaveActionResult,
        ) -> Result<(), String> {
            Err("Weave sessions are only supported on Unix platforms.".to_string())
        }
    }

    pub async fn list_sessions() -> Result<Vec<WeaveSession>, String> {
        Err("Weave sessions are only supported on Unix platforms.".to_string())
    }

    pub async fn create_session(_name: Option<String>) -> Result<WeaveSession, String> {
        Err("Weave sessions are only supported on Unix platforms.".to_string())
    }

    pub async fn close_session(_session_id: &str) -> Result<(), String> {
        Err("Weave sessions are only supported on Unix platforms.".to_string())
    }

    pub async fn list_agents(_session_id: &str, _src: &str) -> Result<Vec<WeaveAgent>, String> {
        Err("Weave sessions are only supported on Unix platforms.".to_string())
    }

    pub async fn connect_agent(
        _session_id: String,
        _agent_id: String,
        _name: Option<String>,
    ) -> Result<WeaveAgentConnection, String> {
        Err("Weave sessions are only supported on Unix platforms.".to_string())
    }
}

pub use platform::WeaveAgentConnection;
pub use platform::WeaveAgentSender;
pub use platform::close_session;
pub use platform::connect_agent;
pub use platform::create_session;
pub use platform::list_agents;
pub use platform::list_sessions;
