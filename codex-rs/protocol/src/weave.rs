use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json;
use ts_rs::TS;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WeaveRelayOutput {
    RelayActions {
        actions: Vec<WeaveRelayAction>,
        #[serde(default)]
        done: Option<WeaveRelayDoneRequest>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WeaveRelayToolArgs {
    pub relay_id: String,
    pub actions: Vec<WeaveRelayAction>,
    #[serde(default)]
    pub done: Option<WeaveRelayDoneRequest>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct WeaveRelayToolResult {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WeaveRelayRequestEvent {
    /// Responses API call id for the associated tool call, if available.
    pub call_id: String,
    /// Turn ID that this request belongs to.
    /// Uses `#[serde(default)]` for backwards compatibility.
    #[serde(default)]
    pub turn_id: String,
    pub relay_id: String,
    pub actions: Vec<WeaveRelayAction>,
    #[serde(default)]
    pub done: Option<WeaveRelayDoneRequest>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
#[serde(untagged)]
pub enum WeaveRelayDoneRequest {
    Flag(bool),
    Summary {
        #[serde(default)]
        summary: Option<String>,
    },
}

impl WeaveRelayDoneRequest {
    pub fn is_requested(&self) -> bool {
        match self {
            Self::Flag(value) => *value,
            Self::Summary { .. } => true,
        }
    }

    pub fn summary(&self) -> Option<&str> {
        match self {
            Self::Summary { summary } => {
                summary.as_deref().map(str::trim).filter(|s| !s.is_empty())
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WeaveRelayAction {
    Message {
        dst: String,
        text: String,
        #[serde(default)]
        expects_reply: Option<bool>,
        #[serde(default)]
        plan_step_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        plan: Option<WeaveRelayPlan>,
    },
    Control {
        dst: String,
        #[serde(default)]
        plan_step_id: String,
        command: WeaveRelayCommand,
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        args: Option<String>,
    },
    Wait {
        targets: Vec<String>,
        #[serde(default)]
        plan_step_id: String,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum WeaveRelayCommand {
    New,
    Compact,
    Interrupt,
    Review,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WeaveRelayPlan {
    pub steps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn parse_weave_relay_output(text: &str) -> Option<WeaveRelayOutput> {
    let candidate = strip_weave_json_fence(text);
    let candidate = candidate.trim();
    if !candidate.starts_with('{') || !candidate.ends_with('}') {
        return None;
    }
    let output: WeaveRelayOutput = serde_json::from_str(candidate).ok()?;
    Some(output)
}

fn strip_weave_json_fence(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let without_ticks = trimmed.trim_start_matches("```");
    let without_lang = without_ticks
        .strip_prefix("json")
        .or_else(|| without_ticks.strip_prefix("JSON"))
        .unwrap_or(without_ticks);
    let without_lang = without_lang.trim_start();
    without_lang.trim_end_matches("```").trim().to_string()
}
