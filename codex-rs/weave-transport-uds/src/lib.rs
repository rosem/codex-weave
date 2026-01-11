#[cfg(unix)]
mod platform {
    use chrono::SecondsFormat;
    use codex_weave_runtime::WeaveAgent;
    use codex_weave_runtime::WeaveMessageKind;
    use codex_weave_runtime::WeaveSession;
    use codex_weave_runtime::WeaveSessionStatus;
    use serde::Deserialize;
    use serde::Serialize;
    use serde_json::Value;
    use serde_json::json;
    use std::collections::HashMap;
    use std::env;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::io::BufReader;
    use tokio::net::UnixStream;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    const WEAVE_VERSION: u8 = 1;
    const COORD_SOCKET: &str = "coord.sock";
    const SESSIONS_DIR: &str = "sessions";

    #[derive(Debug, Serialize, Deserialize)]
    struct WeaveErrorDetail {
        code: String,
        message: String,
        #[serde(default)]
        detail: Option<Value>,
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
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WeaveErrorDetail>,
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

    #[derive(Debug, Deserialize)]
    struct EventsHistoryResponse {
        records: Vec<Value>,
    }

    #[derive(Debug, Clone)]
    pub struct WeaveUdsClient {
        src: String,
        seq: Arc<AtomicU64>,
    }

    impl Default for WeaveUdsClient {
        fn default() -> Self {
            Self::new("codex-cli")
        }
    }

    impl WeaveUdsClient {
        pub fn new(src: impl Into<String>) -> Self {
            Self {
                src: src.into(),
                seq: Arc::new(AtomicU64::new(0)),
            }
        }

        fn next_seq(&self) -> u64 {
            self.seq.fetch_add(1, Ordering::Relaxed).saturating_add(1)
        }

        pub async fn list_sessions(&self) -> Result<Vec<WeaveSession>, String> {
            let socket_path = coord_socket_path(&resolve_weave_home()?);
            let request = new_envelope_with_src("session.list", self.src.clone(), None, None);
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
                        status: WeaveSessionStatus::Open,
                    }
                })
                .collect();
            Ok(sessions)
        }

        pub async fn create_session(&self, name: Option<String>) -> Result<WeaveSession, String> {
            let socket_path = coord_socket_path(&resolve_weave_home()?);
            let payload = name.as_ref().map(|name| json!({ "name": name }));
            let request = new_envelope_with_src("session.create", self.src.clone(), None, payload);
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
                status: WeaveSessionStatus::Open,
            })
        }

        pub async fn close_session(&self, session_id: &str) -> Result<(), String> {
            let weave_home = resolve_weave_home()?;
            let session_socket = session_socket_path(&weave_home, session_id);
            let socket_path = if session_socket.exists() {
                session_socket
            } else {
                coord_socket_path(&weave_home)
            };
            let request = new_envelope_with_src(
                "session.close",
                self.src.clone(),
                Some(session_id.to_string()),
                None,
            );
            let response = send_request(&socket_path, &request).await?;
            if let Some(message) = response_error(&response) {
                return Err(message);
            }
            Ok(())
        }

        pub async fn list_agents(&self, session_id: &str) -> Result<Vec<WeaveAgent>, String> {
            let weave_home = resolve_weave_home()?;
            let session_socket = session_socket_path(&weave_home, session_id);
            let socket_path = if session_socket.exists() {
                session_socket
            } else {
                coord_socket_path(&weave_home)
            };
            let request = new_envelope_with_src(
                "agent.list",
                self.src.clone(),
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
                    WeaveAgent {
                        id: entry.id,
                        name,
                        lead: false,
                        status: None,
                    }
                })
                .collect();
            Ok(agents)
        }

        pub async fn send_messages(
            &self,
            session_id: &str,
            dsts: Vec<String>,
            text: String,
            sender_name: Option<String>,
            kind: Option<WeaveMessageKind>,
        ) -> Result<(), String> {
            if dsts.is_empty() {
                return Ok(());
            }
            let weave_home = resolve_weave_home()?;
            let session_socket = session_socket_path(&weave_home, session_id);
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

            let agent_id = format!("weave-http-{}", Uuid::new_v4());
            let payload = agent_add_payload(&agent_id, sender_name.as_deref());
            let add_request = new_envelope_with_src(
                "agent.add",
                agent_id.clone(),
                Some(session_id.to_string()),
                Some(payload),
            );
            send_envelope(&mut write_half, &add_request).await?;
            let response = read_response(&mut reader, add_request.id.as_str()).await?;
            if let Some(message) = response_error(&response) {
                return Err(message);
            }

            let group_id = Uuid::new_v4().to_string();
            let sender_name = sender_name.as_deref();
            let actions = dsts
                .iter()
                .enumerate()
                .map(|(index, dst)| {
                    action_message_payload(
                        dst,
                        text.as_str(),
                        Uuid::new_v4().to_string(),
                        index,
                        kind,
                        sender_name,
                    )
                })
                .collect::<Vec<_>>();
            let payload = action_submit_payload(&group_id, actions);
            let mut request = new_envelope_with_src(
                "action.submit",
                agent_id.clone(),
                Some(session_id.to_string()),
                Some(payload),
            );
            request.seq = Some(self.next_seq());
            request.idempotency_key = Some(group_id);
            send_envelope(&mut write_half, &request).await?;
            let response = read_response(&mut reader, request.id.as_str()).await?;
            if let Some(message) = response_error(&response) {
                return Err(message);
            }

            let remove_request = new_envelope_with_src(
                "agent.remove",
                agent_id.clone(),
                Some(session_id.to_string()),
                Some(json!({ "id": agent_id })),
            );
            let _ = send_envelope(&mut write_half, &remove_request).await;
            let _ = read_response(&mut reader, remove_request.id.as_str()).await;
            Ok(())
        }

        pub async fn list_events(
            &self,
            session_id: &str,
            limit: usize,
        ) -> Result<Vec<Value>, String> {
            let weave_home = resolve_weave_home()?;
            let session_socket = session_socket_path(&weave_home, session_id);
            let socket_path = if session_socket.exists() {
                session_socket
            } else {
                coord_socket_path(&weave_home)
            };
            let payload = (limit > 0).then(|| json!({ "limit": limit }));
            let request = new_envelope_with_src(
                "events.history",
                self.src.clone(),
                Some(session_id.to_string()),
                payload,
            );
            let response = send_request(&socket_path, &request).await?;
            if let Some(message) = response_error(&response) {
                return Err(message);
            }
            let payload = response
                .payload
                .ok_or_else(|| "Weave events.history response missing payload".to_string())?;
            let response: EventsHistoryResponse = serde_json::from_value(payload)
                .map_err(|err| format!("Failed to parse Weave events.history: {err}"))?;
            Ok(response.records)
        }

        pub async fn stream_events(
            &self,
            session_id: &str,
            follow: bool,
        ) -> Result<mpsc::UnboundedReceiver<Value>, String> {
            let weave_home = resolve_weave_home()?;
            let session_socket = session_socket_path(&weave_home, session_id);
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

            let payload = json!({ "follow": follow });
            let request = new_envelope_with_src(
                "events.stream",
                self.src.clone(),
                Some(session_id.to_string()),
                Some(payload),
            );
            send_envelope(&mut write_half, &request).await?;
            let response = read_response(&mut reader, request.id.as_str()).await?;
            if let Some(message) = response_error(&response) {
                return Err(message);
            }

            let request_id = request.id.clone();
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(async move {
                let mut line = String::new();
                loop {
                    line.clear();
                    let bytes = match reader.read_line(&mut line).await {
                        Ok(bytes) => bytes,
                        Err(_) => break,
                    };
                    if bytes == 0 {
                        break;
                    }
                    let envelope: WeaveEnvelope = match serde_json::from_str(line.trim_end()) {
                        Ok(envelope) => envelope,
                        Err(_) => continue,
                    };
                    if envelope.r#type != "events.record" {
                        continue;
                    }
                    if envelope.corr.as_deref() != Some(request_id.as_str()) {
                        continue;
                    }
                    let Some(payload) = envelope.payload else {
                        continue;
                    };
                    if tx.send(payload).is_err() {
                        break;
                    }
                }
            });
            Ok(rx)
        }
    }

    fn resolve_weave_home() -> Result<PathBuf, String> {
        if let Ok(value) = env::var("WEAVE_HOME") {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err("WEAVE_HOME is set but empty".to_string());
            }
            return expand_home(trimmed);
        }
        let home =
            dirs::home_dir().ok_or_else(|| "Failed to resolve home directory".to_string())?;
        Ok(home.join(".weave"))
    }

    fn expand_home(path: &str) -> Result<PathBuf, String> {
        if path == "~" || path.starts_with("~/") {
            let home =
                dirs::home_dir().ok_or_else(|| "Failed to resolve home directory".to_string())?;
            if path == "~" {
                return Ok(home);
            }
            return Ok(home.join(&path[2..]));
        }
        Ok(PathBuf::from(path))
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
            session,
            seq: None,
            idempotency_key: None,
            corr: None,
            payload,
            status: None,
            error: None,
        }
    }

    fn now_timestamp() -> String {
        chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    async fn send_request(
        socket_path: &Path,
        request: &WeaveEnvelope,
    ) -> Result<WeaveEnvelope, String> {
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(|err| format!("Failed to connect to Weave coordinator: {err}"))?;
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        send_envelope(&mut write_half, request).await?;
        read_response(&mut reader, request.id.as_str()).await
    }

    async fn send_envelope(
        write_half: &mut tokio::io::WriteHalf<UnixStream>,
        request: &WeaveEnvelope,
    ) -> Result<(), String> {
        let payload = serde_json::to_vec(request)
            .map_err(|err| format!("Failed to serialize Weave request: {err}"))?;
        write_half
            .write_all(&payload)
            .await
            .map_err(|err| format!("Failed to send Weave request: {err}"))?;
        write_half
            .write_all(b"\n")
            .await
            .map_err(|err| format!("Failed to send Weave request newline: {err}"))?;
        Ok(())
    }

    async fn read_response(
        reader: &mut BufReader<tokio::io::ReadHalf<UnixStream>>,
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
                return Err("Weave coordinator closed connection".to_string());
            }
            let envelope: WeaveEnvelope = serde_json::from_str(&line)
                .map_err(|err| format!("Failed to parse Weave response: {err}"))?;
            if envelope.corr.as_deref() == Some(request_id) {
                return Ok(envelope);
            }
        }
    }

    fn response_error(response: &WeaveEnvelope) -> Option<String> {
        let status = response.status.as_deref().unwrap_or("ok");
        if status != "error" {
            return None;
        }
        let detail = response.error.as_ref();
        let message = detail.map_or("Weave request failed", |detail| detail.message.as_str());
        let mut info = HashMap::from([("status", status)]);
        if let Some(detail) = detail {
            info.insert("code", detail.code.as_str());
        }
        let extra = info
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ");
        Some(format!("{message} ({extra})"))
    }

    fn agent_add_payload(agent_id: &str, name: Option<&str>) -> Value {
        let trimmed = name.map(str::trim).filter(|name| !name.is_empty());
        match trimmed {
            Some(name) => json!({ "id": agent_id, "name": name }),
            None => json!({ "id": agent_id }),
        }
    }

    fn kind_label(kind: WeaveMessageKind) -> &'static str {
        match kind {
            WeaveMessageKind::User => "user",
            WeaveMessageKind::Reply => "reply",
            WeaveMessageKind::Control => "control",
            WeaveMessageKind::System => "system",
        }
    }

    fn action_message_payload(
        dst: &str,
        text: &str,
        action_id: String,
        action_index: usize,
        kind: Option<WeaveMessageKind>,
        sender_name: Option<&str>,
    ) -> Value {
        let mut payload = serde_json::Map::new();
        payload.insert("type".to_string(), json!("message"));
        payload.insert("dst".to_string(), json!(dst));
        if let Some(kind) = kind {
            payload.insert("kind".to_string(), json!(kind_label(kind)));
        }
        if let Some(sender_name) = sender_name.map(str::trim).filter(|name| !name.is_empty()) {
            payload.insert("sender_name".to_string(), json!(sender_name));
        }
        payload.insert("action_id".to_string(), json!(action_id));
        payload.insert("action_index".to_string(), json!(action_index));
        payload.insert("text".to_string(), json!(text));
        Value::Object(payload)
    }

    fn action_submit_payload(group_id: &str, actions: Vec<Value>) -> Value {
        let mut payload = serde_json::Map::new();
        payload.insert("group_id".to_string(), json!(group_id));
        payload.insert("actions".to_string(), Value::Array(actions));
        Value::Object(payload)
    }

    pub use WeaveUdsClient as Client;
}

#[cfg(not(unix))]
mod platform {
    use codex_weave_runtime::WeaveAgent;
    use codex_weave_runtime::WeaveMessageKind;
    use codex_weave_runtime::WeaveSession;
    use serde_json::Value;
    use tokio::sync::mpsc;

    #[derive(Debug, Clone, Default)]
    pub struct WeaveUdsClient;

    impl WeaveUdsClient {
        pub fn new(_src: impl Into<String>) -> Self {
            Self
        }

        pub async fn list_sessions(&self) -> Result<Vec<WeaveSession>, String> {
            Err("Weave UDS transport is only available on unix".to_string())
        }

        pub async fn create_session(&self, _name: Option<String>) -> Result<WeaveSession, String> {
            Err("Weave UDS transport is only available on unix".to_string())
        }

        pub async fn close_session(&self, _session_id: &str) -> Result<(), String> {
            Err("Weave UDS transport is only available on unix".to_string())
        }

        pub async fn list_agents(&self, _session_id: &str) -> Result<Vec<WeaveAgent>, String> {
            Err("Weave UDS transport is only available on unix".to_string())
        }

        pub async fn send_messages(
            &self,
            _session_id: &str,
            _dsts: Vec<String>,
            _text: String,
            _sender_name: Option<String>,
            _kind: Option<WeaveMessageKind>,
        ) -> Result<(), String> {
            Err("Weave UDS transport is only available on unix".to_string())
        }

        pub async fn list_events(
            &self,
            _session_id: &str,
            _limit: usize,
        ) -> Result<Vec<Value>, String> {
            Err("Weave UDS transport is only available on unix".to_string())
        }

        pub async fn stream_events(
            &self,
            _session_id: &str,
            _follow: bool,
        ) -> Result<mpsc::UnboundedReceiver<Value>, String> {
            Err("Weave UDS transport is only available on unix".to_string())
        }
    }

    pub use WeaveUdsClient as Client;
}

pub use platform::Client as WeaveUdsClient;
