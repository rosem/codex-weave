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
    },
    TaskDone {
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WeaveRelayAction {
    Message {
        dst: String,
        text: String,
        reply_policy: String,
        #[serde(default)]
        step_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        plan: Option<WeaveRelayPlan>,
    },
    Control {
        dst: String,
        #[serde(default)]
        step_id: String,
        command: WeaveRelayCommand,
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
