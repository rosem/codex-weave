use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_app_server_protocol::AuthMode;
use codex_backend_client::Client as BackendClient;
use codex_core::config::Config;
use codex_core::config::ConstraintResult;
use codex_core::config::types::Notifications;
use codex_core::features::FEATURES;
use codex_core::features::Feature;
use codex_core::git_info::current_branch_name;
use codex_core::git_info::local_git_branches;
use codex_core::models_manager::manager::ModelsManager;
use codex_core::project_doc::DEFAULT_PROJECT_DOC_FILENAME;
use codex_core::protocol::AgentMessageDeltaEvent;
use codex_core::protocol::AgentMessageEvent;
use codex_core::protocol::AgentReasoningDeltaEvent;
use codex_core::protocol::AgentReasoningEvent;
use codex_core::protocol::AgentReasoningRawContentDeltaEvent;
use codex_core::protocol::AgentReasoningRawContentEvent;
use codex_core::protocol::ApplyPatchApprovalRequestEvent;
use codex_core::protocol::BackgroundEventEvent;
use codex_core::protocol::CreditsSnapshot;
use codex_core::protocol::DeprecationNoticeEvent;
use codex_core::protocol::ErrorEvent;
use codex_core::protocol::Event;
use codex_core::protocol::EventMsg;
use codex_core::protocol::ExecApprovalRequestEvent;
use codex_core::protocol::ExecCommandBeginEvent;
use codex_core::protocol::ExecCommandEndEvent;
use codex_core::protocol::ExecCommandSource;
use codex_core::protocol::ExitedReviewModeEvent;
use codex_core::protocol::ListCustomPromptsResponseEvent;
use codex_core::protocol::ListSkillsResponseEvent;
use codex_core::protocol::McpListToolsResponseEvent;
use codex_core::protocol::McpStartupCompleteEvent;
use codex_core::protocol::McpStartupStatus;
use codex_core::protocol::McpStartupUpdateEvent;
use codex_core::protocol::McpToolCallBeginEvent;
use codex_core::protocol::McpToolCallEndEvent;
use codex_core::protocol::Op;
use codex_core::protocol::PatchApplyBeginEvent;
use codex_core::protocol::RateLimitSnapshot;
use codex_core::protocol::ReviewRequest;
use codex_core::protocol::ReviewTarget;
use codex_core::protocol::SkillsListEntry;
use codex_core::protocol::StreamErrorEvent;
use codex_core::protocol::TerminalInteractionEvent;
use codex_core::protocol::TokenUsage;
use codex_core::protocol::TokenUsageInfo;
use codex_core::protocol::TurnAbortReason;
use codex_core::protocol::TurnCompleteEvent;
use codex_core::protocol::TurnDiffEvent;
use codex_core::protocol::UndoCompletedEvent;
use codex_core::protocol::UndoStartedEvent;
use codex_core::protocol::UserMessageEvent;
use codex_core::protocol::ViewImageToolCallEvent;
use codex_core::protocol::WarningEvent;
use codex_core::protocol::WebSearchBeginEvent;
use codex_core::protocol::WebSearchEndEvent;
use codex_core::skills::model::SkillMetadata;
use codex_protocol::ThreadId;
use codex_protocol::account::PlanType;
use codex_protocol::approvals::ElicitationRequestEvent;
use codex_protocol::parse_command::ParsedCommand;
use codex_protocol::user_input::UserInput;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use rand::Rng;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use tracing::debug;
use uuid::Uuid;

use crate::app_event::AppEvent;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxEnableMode;
use crate::app_event::WindowsSandboxFallbackReason;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::BetaFeatureItem;
use crate::bottom_pane::BottomPane;
use crate::bottom_pane::BottomPaneParams;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ExperimentalFeaturesView;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::clipboard_paste::paste_image_to_temp_png;
use crate::diff_render::display_path_for;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::ExecCell;
use crate::exec_cell::new_active_exec_command;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::get_git_diff::get_git_diff;
use crate::history_cell;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::McpToolCallCell;
use crate::history_cell::PlainHistoryCell;
use crate::markdown::append_markdown;
use crate::render::Insets;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableExt;
use crate::render::renderable::RenderableItem;
use crate::slash_command::SlashCommand;
use crate::status::RateLimitSnapshotDisplay;
use crate::text_formatting::truncate_text;
use crate::tui::FrameRequester;
use crate::weave_client;
use crate::weave_client::WeaveActionResult;
use crate::weave_client::WeaveAgent;
use crate::weave_client::WeaveAgentConnection;
use crate::weave_client::WeaveIncomingMessage;
use crate::weave_client::WeaveMessageKind;
use crate::weave_client::WeaveMessageMetadata;
use crate::weave_client::WeaveSession;
use crate::weave_client::WeaveTaskDone;
use crate::weave_client::WeaveTaskUpdate;
use crate::weave_client::WeaveTool;
mod interrupts;
use self::interrupts::InterruptManager;
mod agent;
use self::agent::spawn_agent;
use self::agent::spawn_agent_from_existing;
mod session_header;
use self::session_header::SessionHeader;
use crate::streaming::controller::StreamController;
use std::path::Path;

use chrono::Local;
use codex_common::approval_presets::ApprovalPreset;
use codex_common::approval_presets::builtin_approval_presets;
use codex_core::AuthManager;
use codex_core::CodexAuth;
use codex_core::ThreadManager;
use codex_core::protocol::AskForApproval;
use codex_core::protocol::SandboxPolicy;
use codex_file_search::FileMatch;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_protocol::weave::WeaveRelayAction;
use codex_protocol::weave::WeaveRelayCommand;
use codex_protocol::weave::WeaveRelayOutput;
use codex_protocol::weave::WeaveRelayPlan;
use codex_protocol::weave::parse_weave_relay_output;
use serde_json::Value;
use serde_json::json;
use strum::IntoEnumIterator;

const USER_SHELL_COMMAND_HELP_TITLE: &str = "Prefix a command with ! to run it locally";
const USER_SHELL_COMMAND_HELP_HINT: &str = "Example: !ls";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
// Track information about an in-flight exec command.
struct RunningCommand {
    command: Vec<String>,
    parsed_cmd: Vec<ParsedCommand>,
    source: ExecCommandSource,
}

struct UnifiedExecProcessSummary {
    key: String,
    command_display: String,
}

struct UnifiedExecWaitState {
    command_display: String,
}

impl UnifiedExecWaitState {
    fn new(command_display: String) -> Self {
        Self { command_display }
    }

    fn is_duplicate(&self, command_display: &str) -> bool {
        self.command_display == command_display
    }
}

struct WeaveToolContext<'a> {
    sender: &'a str,
    sender_id: &'a str,
    text: &'a str,
    conversation_id: &'a str,
    conversation_owner: &'a str,
    has_conversation_metadata: bool,
    action_group_id: Option<&'a str>,
    action_id: Option<&'a str>,
    action_index: Option<usize>,
}

fn is_unified_exec_source(source: ExecCommandSource) -> bool {
    matches!(
        source,
        ExecCommandSource::UnifiedExecStartup | ExecCommandSource::UnifiedExecInteraction
    )
}

fn is_standard_tool_call(parsed_cmd: &[ParsedCommand]) -> bool {
    !parsed_cmd.is_empty()
        && parsed_cmd
            .iter()
            .all(|parsed| !matches!(parsed, ParsedCommand::Unknown { .. }))
}

const RATE_LIMIT_WARNING_THRESHOLDS: [f64; 3] = [75.0, 90.0, 95.0];
const NUDGE_MODEL_SLUG: &str = "gpt-5.1-codex-mini";
const RATE_LIMIT_SWITCH_PROMPT_THRESHOLD: f64 = 90.0;

#[derive(Default)]
struct RateLimitWarningState {
    secondary_index: usize,
    primary_index: usize,
}

impl RateLimitWarningState {
    fn take_warnings(
        &mut self,
        secondary_used_percent: Option<f64>,
        secondary_window_minutes: Option<i64>,
        primary_used_percent: Option<f64>,
        primary_window_minutes: Option<i64>,
    ) -> Vec<String> {
        let reached_secondary_cap =
            matches!(secondary_used_percent, Some(percent) if percent == 100.0);
        let reached_primary_cap = matches!(primary_used_percent, Some(percent) if percent == 100.0);
        if reached_secondary_cap || reached_primary_cap {
            return Vec::new();
        }

        let mut warnings = Vec::new();

        if let Some(secondary_used_percent) = secondary_used_percent {
            let mut highest_secondary: Option<f64> = None;
            while self.secondary_index < RATE_LIMIT_WARNING_THRESHOLDS.len()
                && secondary_used_percent >= RATE_LIMIT_WARNING_THRESHOLDS[self.secondary_index]
            {
                highest_secondary = Some(RATE_LIMIT_WARNING_THRESHOLDS[self.secondary_index]);
                self.secondary_index += 1;
            }
            if let Some(threshold) = highest_secondary {
                let limit_label = secondary_window_minutes
                    .map(get_limits_duration)
                    .unwrap_or_else(|| "weekly".to_string());
                let remaining_percent = 100.0 - threshold;
                warnings.push(format!(
                    "Heads up, you have less than {remaining_percent:.0}% of your {limit_label} limit left. Run /status for a breakdown."
                ));
            }
        }

        if let Some(primary_used_percent) = primary_used_percent {
            let mut highest_primary: Option<f64> = None;
            while self.primary_index < RATE_LIMIT_WARNING_THRESHOLDS.len()
                && primary_used_percent >= RATE_LIMIT_WARNING_THRESHOLDS[self.primary_index]
            {
                highest_primary = Some(RATE_LIMIT_WARNING_THRESHOLDS[self.primary_index]);
                self.primary_index += 1;
            }
            if let Some(threshold) = highest_primary {
                let limit_label = primary_window_minutes
                    .map(get_limits_duration)
                    .unwrap_or_else(|| "5h".to_string());
                let remaining_percent = 100.0 - threshold;
                warnings.push(format!(
                    "Heads up, you have less than {remaining_percent:.0}% of your {limit_label} limit left. Run /status for a breakdown."
                ));
            }
        }

        warnings
    }
}

pub(crate) fn get_limits_duration(windows_minutes: i64) -> String {
    const MINUTES_PER_HOUR: i64 = 60;
    const MINUTES_PER_DAY: i64 = 24 * MINUTES_PER_HOUR;
    const MINUTES_PER_WEEK: i64 = 7 * MINUTES_PER_DAY;
    const MINUTES_PER_MONTH: i64 = 30 * MINUTES_PER_DAY;
    const ROUNDING_BIAS_MINUTES: i64 = 3;

    let windows_minutes = windows_minutes.max(0);

    if windows_minutes <= MINUTES_PER_DAY.saturating_add(ROUNDING_BIAS_MINUTES) {
        let adjusted = windows_minutes.saturating_add(ROUNDING_BIAS_MINUTES);
        let hours = std::cmp::max(1, adjusted / MINUTES_PER_HOUR);
        format!("{hours}h")
    } else if windows_minutes <= MINUTES_PER_WEEK.saturating_add(ROUNDING_BIAS_MINUTES) {
        "weekly".to_string()
    } else if windows_minutes <= MINUTES_PER_MONTH.saturating_add(ROUNDING_BIAS_MINUTES) {
        "monthly".to_string()
    } else {
        "annual".to_string()
    }
}

/// Common initialization parameters shared by all `ChatWidget` constructors.
pub(crate) struct ChatWidgetInit {
    pub(crate) config: Config,
    pub(crate) frame_requester: FrameRequester,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) initial_prompt: Option<String>,
    pub(crate) initial_images: Vec<PathBuf>,
    pub(crate) enhanced_keys_supported: bool,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) models_manager: Arc<ModelsManager>,
    pub(crate) feedback: codex_feedback::CodexFeedback,
    pub(crate) is_first_run: bool,
    pub(crate) model: String,
}

#[derive(Default)]
enum RateLimitSwitchPromptState {
    #[default]
    Idle,
    Pending,
    Shown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ExternalEditorState {
    #[default]
    Closed,
    Requested,
    Active,
}

pub(crate) struct ChatWidget {
    app_event_tx: AppEventSender,
    codex_op_tx: UnboundedSender<Op>,
    bottom_pane: BottomPane,
    active_cell: Option<Box<dyn HistoryCell>>,
    config: Config,
    model: String,
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    session_header: SessionHeader,
    initial_user_message: Option<UserMessage>,
    token_info: Option<TokenUsageInfo>,
    rate_limit_snapshot: Option<RateLimitSnapshotDisplay>,
    plan_type: Option<PlanType>,
    rate_limit_warnings: RateLimitWarningState,
    rate_limit_switch_prompt: RateLimitSwitchPromptState,
    rate_limit_poller: Option<JoinHandle<()>>,
    // Stream lifecycle controller
    stream_controller: Option<StreamController>,
    running_commands: HashMap<String, RunningCommand>,
    suppressed_exec_calls: HashSet<String>,
    last_unified_wait: Option<UnifiedExecWaitState>,
    task_complete_pending: bool,
    unified_exec_processes: Vec<UnifiedExecProcessSummary>,
    mcp_startup_status: Option<HashMap<String, McpStartupStatus>>,
    // Queue of interruptive UI events deferred during an active write cycle
    interrupts: InterruptManager,
    // Skip sending an interrupt back when the interrupt originated remotely.
    suppress_weave_interrupt: bool,
    // Accumulates the current reasoning block text to extract a header
    reasoning_buffer: String,
    // Accumulates full reasoning content for transcript-only recording
    full_reasoning_buffer: String,
    // Current status header shown in the status indicator.
    current_status_header: String,
    // Previous status header to restore after a transient stream retry.
    retry_status_header: Option<String>,
    thread_id: Option<ThreadId>,
    selected_weave_session_id: Option<String>,
    selected_weave_session_name: Option<String>,
    weave_agent_id: String,
    weave_agent_name: String,
    weave_agent_connection: Option<WeaveAgentConnection>,
    weave_agents: Option<Vec<WeaveAgent>>,
    pending_weave_relay: Option<WeaveRelayTargets>,
    active_weave_relay: Option<WeaveRelayTargets>,
    active_weave_plan: Option<WeaveRelayPlan>,
    pending_weave_plan: bool,
    weave_relay_buffer: String,
    relay_output_consumed: bool,
    weave_inbound_task_ids: HashMap<String, String>,
    weave_target_states: HashMap<String, WeaveRelayTargetState>,
    pending_weave_action_messages: VecDeque<WeaveIncomingMessage>,
    pending_weave_new_session_result: Option<WeavePendingActionResult>,
    pending_weave_new_session_context: Option<WeavePendingNewSessionContext>,
    pending_weave_new_session: bool,
    frame_requester: FrameRequester,
    // Whether to include the initial welcome banner on session configured
    show_welcome_banner: bool,
    // When resuming an existing session (selected via resume picker), avoid an
    // immediate redraw on SessionConfigured to prevent a gratuitous UI flicker.
    suppress_session_configured_redraw: bool,
    // User messages queued while a turn is in progress
    queued_user_messages: VecDeque<UserMessage>,
    // Track which Weave agent should receive the next model reply.
    active_weave_reply_target: Option<WeaveReplyTarget>,
    // Track which sender owns the current control-initiated task.
    active_weave_control_context: Option<WeaveControlContext>,
    // Pending notification to show when unfocused on next Draw
    pending_notification: Option<Notification>,
    // Simple review mode flag; used to adjust layout and banners.
    is_review_mode: bool,
    // Snapshot of token usage to restore after review mode exits.
    pre_review_token_info: Option<Option<TokenUsageInfo>>,
    // Whether to add a final message separator after the last message
    needs_final_message_separator: bool,

    last_rendered_width: std::cell::Cell<Option<usize>>,
    // Feedback sink for /feedback
    feedback: codex_feedback::CodexFeedback,
    // Current session rollout path (if known)
    current_rollout_path: Option<PathBuf>,
    external_editor_state: ExternalEditorState,
}

#[derive(Debug)]
pub(crate) struct WeaveStateSnapshot {
    pub(crate) session_id: Option<String>,
    pub(crate) session_name: Option<String>,
    pub(crate) agent_id: String,
    pub(crate) agent_name: String,
    pub(crate) connection: Option<WeaveAgentConnection>,
    pub(crate) pending_new_session_result: Option<WeavePendingActionResult>,
    pub(crate) pending_new_session_context: Option<WeavePendingNewSessionContext>,
    pub(crate) pending_action_messages: VecDeque<WeaveIncomingMessage>,
}

#[derive(Clone, Debug)]
struct WeaveReplyTarget {
    session_id: String,
    agent_id: String,
    display_name: String,
    conversation_id: String,
    conversation_owner: String,
    parent_message_id: Option<String>,
    task_id: Option<String>,
    action_group_id: Option<String>,
    action_id: Option<String>,
    action_index: Option<usize>,
}

#[derive(Clone, Debug)]
struct WeaveRelayTargets {
    owner_id: String,
    target_ids: HashSet<String>,
}

#[derive(Clone, Debug)]
struct WeaveRelayTargetState {
    context_id: String,
    task_id: String,
    pending_actions: HashMap<String, WeavePendingAction>,
    last_action_id: Option<String>,
}

#[derive(Clone, Debug)]
struct WeavePendingAction {
    group_id: String,
    expects_reply: bool,
}

#[derive(Clone, Debug)]
struct WeaveRelayGroupPayload {
    group_id: String,
    actions: Vec<Value>,
    metadata: WeaveMessageMetadata,
}

#[derive(Clone, Debug)]
struct WeaveControlContext {
    sender_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct WeavePendingActionResult {
    sender_id: String,
    group_id: String,
    action_id: String,
    action_index: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct WeavePendingNewSessionContext {
    group_id: String,
    conversation_id: String,
    conversation_owner: String,
    task_id: String,
}

struct WeaveActionResultContext<'a> {
    status: &'a str,
    detail: Option<&'a str>,
    new_context_id: Option<&'a str>,
    new_task_id: Option<&'a str>,
}

impl WeaveRelayTargets {
    fn new(owner_id: String, agents: &[WeaveAgent]) -> Self {
        let target_ids = agents.iter().map(|agent| agent.id.clone()).collect();
        Self {
            owner_id,
            target_ids,
        }
    }

    fn allows(&self, agent: &WeaveAgent) -> bool {
        self.target_ids.contains(&agent.id)
    }

    fn allows_id(&self, agent_id: &str) -> bool {
        self.target_ids.contains(agent_id)
    }

    fn matches(&self, other: &Self) -> bool {
        self.owner_id == other.owner_id && self.target_ids == other.target_ids
    }
}

impl WeaveRelayTargetState {
    fn new(context_id: String, task_id: String) -> Self {
        Self {
            context_id,
            task_id,
            pending_actions: HashMap::new(),
            last_action_id: None,
        }
    }

    fn has_pending_reply(&self) -> bool {
        self.pending_actions
            .values()
            .any(|pending| pending.expects_reply)
    }
}

#[derive(Clone, Debug)]
struct WeaveUserMessage {
    reply_target: WeaveReplyTarget,
    auto_reply: bool,
    relay_targets: Option<WeaveRelayTargets>,
    conversation_id: String,
    conversation_owner: String,
    task_id: Option<String>,
    kind: WeaveMessageKind,
}

#[derive(Clone, Debug)]
enum UserMessageSource {
    Local,
    Weave(Box<WeaveUserMessage>),
}

struct UserMessage {
    text: String,
    image_paths: Vec<PathBuf>,
    source: UserMessageSource,
}

impl UserMessage {
    fn from_weave(text: String, weave: WeaveUserMessage) -> Self {
        Self {
            text,
            image_paths: Vec::new(),
            source: UserMessageSource::Weave(Box::new(weave)),
        }
    }
}

impl From<String> for UserMessage {
    fn from(text: String) -> Self {
        Self {
            text,
            image_paths: Vec::new(),
            source: UserMessageSource::Local,
        }
    }
}

impl From<&str> for UserMessage {
    fn from(text: &str) -> Self {
        Self {
            text: text.to_string(),
            image_paths: Vec::new(),
            source: UserMessageSource::Local,
        }
    }
}

fn create_initial_user_message(text: String, image_paths: Vec<PathBuf>) -> Option<UserMessage> {
    if text.is_empty() && image_paths.is_empty() {
        None
    } else {
        Some(UserMessage {
            text,
            image_paths,
            source: UserMessageSource::Local,
        })
    }
}

struct WeavePromptContext<'a> {
    self_name: &'a str,
    sender_name: &'a str,
    agents: Option<&'a [WeaveAgent]>,
    message: &'a str,
    is_owner: bool,
    task_targets: Option<&'a str>,
    pending_replies: Option<&'a str>,
    conversation_id: &'a str,
    conversation_owner: &'a str,
    task_id: Option<&'a str>,
    kind: WeaveMessageKind,
}

fn format_weave_prompt(ctx: WeavePromptContext<'_>) -> String {
    let WeavePromptContext {
        self_name,
        sender_name,
        agents,
        message,
        is_owner,
        task_targets,
        pending_replies,
        conversation_id,
        conversation_owner,
        task_id,
        kind,
    } = ctx;
    let mut lines = Vec::new();
    lines.push("Weave context:".to_string());
    lines.push(format!("agent: {self_name}"));
    lines.push(format!("sender: {sender_name}"));
    lines.push(format!("conversation: {conversation_id}"));
    lines.push(format!("conversation owner: {conversation_owner}"));
    if let Some(task_id) = task_id {
        lines.push(format!("task: {task_id}"));
    }
    if let Some(agents) = agents {
        let agent_list = agents
            .iter()
            .map(weave_agent_label)
            .collect::<Vec<_>>()
            .join(", ");
        if !agent_list.is_empty() {
            lines.push(format!("agents: {agent_list}"));
        }
    }
    lines.push("This message came from another agent.".to_string());
    let is_reply = matches!(kind, WeaveMessageKind::Reply);
    if is_owner {
        lines.push("You are the conversation owner.".to_string());
        if let Some(targets) = task_targets {
            lines.push("Weave task: active.".to_string());
            lines.push(format!("Weave task targets: {targets}"));
            if let Some(pending) = pending_replies {
                lines.push(format!("Weave task pending replies: {pending}"));
                if pending == "none" {
                    lines.push(
                        "If the task is complete, respond with type \"task_done\".".to_string(),
                    );
                } else {
                    lines.push(
                        "Wait for pending replies unless you need clarification.".to_string(),
                    );
                }
            }
            lines.push(
                "Continue coordinating with the target agent(s) until the task is complete."
                    .to_string(),
            );
            lines.push(
                "While the task is in progress, respond ONLY with a single-line JSON object."
                    .to_string(),
            );
            lines.push(
                "Use type \"relay_actions\" with an `actions` array to send messages or control commands."
                    .to_string(),
            );
            lines.push("Action types: \"message\", \"control\".".to_string());
            lines.push(
                "When the task is complete, respond with type \"task_done\" and a brief summary for the local user.".to_string(),
            );
            lines.push(
                "If you already provided a plan for this task, do not repeat it.".to_string(),
            );
        } else if is_reply {
            lines.push(
                "This is a reply from another agent. Do not respond unless the local user asks."
                    .to_string(),
            );
        } else {
            lines.push("Treat it as a direct request for you.".to_string());
            lines
                .push("Reply to the sender with the answer. Do not ask them to reply.".to_string());
        }
    } else if is_reply {
        lines.push(
            "This is a reply from another agent. Do not respond unless the local user asks."
                .to_string(),
        );
    } else {
        lines.push("Treat it as a direct request for you.".to_string());
        lines.push("Reply to the sender with the answer. Do not ask them to reply.".to_string());
    }
    lines.push("--- INPUT ---".to_string());
    lines.push(message.to_string());
    lines.join("\n")
}

impl ChatWidget {
    fn default_weave_agent_name(agent_id: &str) -> String {
        let short = agent_id.split('-').next().unwrap_or(agent_id);
        format!("codex-{short}")
    }

    pub(crate) fn take_weave_state(&mut self) -> WeaveStateSnapshot {
        let session_name = match (
            self.selected_weave_session_id.as_deref(),
            self.selected_weave_session_name.as_deref(),
        ) {
            (Some(id), Some(name)) if name != id => Some(name.to_string()),
            _ => None,
        };
        WeaveStateSnapshot {
            session_id: self.selected_weave_session_id.clone(),
            session_name,
            agent_id: self.weave_agent_id.clone(),
            agent_name: self.weave_agent_name.clone(),
            connection: self.weave_agent_connection.take(),
            pending_new_session_result: self.pending_weave_new_session_result.take(),
            pending_new_session_context: self.pending_weave_new_session_context.take(),
            pending_action_messages: std::mem::take(&mut self.pending_weave_action_messages),
        }
    }

    pub(crate) fn restore_weave_state(&mut self, snapshot: WeaveStateSnapshot) {
        self.weave_agent_id = snapshot.agent_id;
        self.weave_agent_name = snapshot.agent_name;
        self.pending_weave_new_session_result = snapshot.pending_new_session_result;
        self.pending_weave_new_session_context = snapshot.pending_new_session_context;
        self.pending_weave_action_messages = snapshot.pending_action_messages;
        self.bottom_pane
            .set_weave_agent_identity(self.weave_agent_id.clone());
        if let Some(connection) = snapshot.connection {
            self.weave_agent_connection = Some(connection);
            self.selected_weave_session_id = snapshot.session_id;
            self.selected_weave_session_name = snapshot.session_name;
            self.weave_agents = None;
            self.bottom_pane.set_weave_agents(None);
            self.refresh_weave_session_label();
            self.request_weave_agent_list();
            self.maybe_send_pending_weave_new_session_result();
            return;
        }
        if let Some(session_id) = snapshot.session_id {
            let session = WeaveSession {
                id: session_id,
                name: snapshot.session_name,
            };
            self.set_weave_session_selection(Some(session));
        } else {
            self.set_weave_session_selection(None);
        }
    }

    fn flush_answer_stream_with_separator(&mut self) {
        if let Some(mut controller) = self.stream_controller.take()
            && let Some(cell) = controller.finalize()
        {
            self.add_boxed_history(cell);
        }
    }

    /// Update the status indicator header and details.
    ///
    /// Passing `None` clears any existing details.
    fn set_status(&mut self, header: String, details: Option<String>) {
        self.current_status_header = header.clone();
        self.bottom_pane.update_status(header, details);
    }

    /// Convenience wrapper around [`Self::set_status`];
    /// updates the status indicator header and clears any existing details.
    fn set_status_header(&mut self, header: String) {
        self.set_status(header, None);
    }

    fn restore_retry_status_header_if_present(&mut self) {
        if let Some(header) = self.retry_status_header.take() {
            self.set_status_header(header);
        }
    }

    fn maybe_send_pending_weave_new_session_result(&mut self) {
        let Some(pending) = self.pending_weave_new_session_result.take() else {
            return;
        };
        let Some(context) = self.pending_weave_new_session_context.clone() else {
            self.pending_weave_new_session_result = Some(pending);
            return;
        };
        if self.thread_id.is_none() {
            self.pending_weave_new_session_result = Some(pending);
            return;
        }
        if self.weave_agent_connection.is_none() {
            self.pending_weave_new_session_result = Some(pending);
            return;
        }
        self.weave_target_states.insert(
            pending.sender_id.clone(),
            WeaveRelayTargetState::new(context.conversation_id.clone(), context.task_id.clone()),
        );
        let action_result = Self::build_weave_action_result(
            &pending.group_id,
            &pending.action_id,
            pending.action_index,
            "completed",
            None,
            Some(context.conversation_id.as_str()),
            Some(context.task_id.as_str()),
        );
        self.send_weave_action_result(&pending.sender_id, action_result);
        if self.pending_weave_action_messages.is_empty() {
            self.pending_weave_new_session_context = None;
        }
    }

    fn should_defer_weave_new_session(
        &self,
        conversation_id: &str,
        conversation_owner: &str,
        sender_id: &str,
    ) -> bool {
        self.active_weave_reply_target
            .as_ref()
            .is_some_and(|target| {
                target.agent_id == sender_id
                    && target.conversation_id == conversation_id
                    && target.conversation_owner == conversation_owner
            })
            || self
                .active_weave_control_context
                .as_ref()
                .is_some_and(|context| context.sender_id == sender_id)
    }

    fn start_weave_new_session(&mut self) {
        self.active_weave_reply_target = None;
        self.active_weave_control_context = None;
        self.pending_weave_relay = None;
        self.active_weave_relay = None;
        self.active_weave_plan = None;
        self.pending_weave_plan = false;
        self.weave_target_states.clear();
        self.weave_relay_buffer.clear();
        if !self.queued_user_messages.is_empty() {
            self.queued_user_messages.clear();
            self.refresh_queued_user_messages();
        }
        self.app_event_tx.send(AppEvent::NewSession);
        self.request_redraw();
    }

    fn maybe_start_pending_weave_new_session(&mut self) -> bool {
        if !self.pending_weave_new_session {
            return false;
        }
        self.pending_weave_new_session = false;
        self.start_weave_new_session();
        true
    }

    // --- Small event handlers ---
    fn on_session_configured(&mut self, event: codex_core::protocol::SessionConfiguredEvent) {
        self.bottom_pane
            .set_history_metadata(event.history_log_id, event.history_entry_count);
        self.set_skills(None);
        self.thread_id = Some(event.session_id);
        self.current_rollout_path = Some(event.rollout_path.clone());
        let initial_messages = event.initial_messages.clone();
        let model_for_header = event.model.clone();
        self.session_header.set_model(&model_for_header);
        self.add_to_history(history_cell::new_session_info(
            &self.config,
            &model_for_header,
            event,
            self.show_welcome_banner,
        ));
        self.maybe_send_pending_weave_new_session_result();
        self.flush_pending_weave_action_messages();
        if let Some(messages) = initial_messages {
            self.replay_initial_messages(messages);
        }
        // Ask codex-core to enumerate custom prompts for this session.
        self.submit_op(Op::ListCustomPrompts);
        self.submit_op(Op::ListSkills {
            cwds: Vec::new(),
            force_reload: false,
        });
        if let Some(user_message) = self.initial_user_message.take() {
            self.submit_user_message(user_message);
        }
        if !self.suppress_session_configured_redraw {
            self.request_redraw();
        }
    }

    fn set_skills(&mut self, skills: Option<Vec<SkillMetadata>>) {
        self.bottom_pane.set_skills(skills);
    }

    fn set_skills_from_response(&mut self, response: &ListSkillsResponseEvent) {
        let skills = skills_for_cwd(&self.config.cwd, &response.skills);
        self.set_skills(Some(skills));
    }

    pub(crate) fn open_feedback_note(
        &mut self,
        category: crate::app_event::FeedbackCategory,
        include_logs: bool,
    ) {
        // Build a fresh snapshot at the time of opening the note overlay.
        let snapshot = self.feedback.snapshot(self.thread_id);
        let rollout = if include_logs {
            self.current_rollout_path.clone()
        } else {
            None
        };
        let view = crate::bottom_pane::FeedbackNoteView::new(
            category,
            snapshot,
            rollout,
            self.app_event_tx.clone(),
            include_logs,
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn open_feedback_consent(&mut self, category: crate::app_event::FeedbackCategory) {
        let params = crate::bottom_pane::feedback_upload_consent_params(
            self.app_event_tx.clone(),
            category,
            self.current_rollout_path.clone(),
        );
        self.bottom_pane.show_selection_view(params);
        self.request_redraw();
    }

    fn on_agent_message(&mut self, mut message: String) {
        if let Some(output) = self.take_weave_relay_output(&message) {
            if self.handle_weave_relay_output(&output) {
                self.relay_output_consumed = true;
                self.handle_stream_finished();
                self.request_redraw();
                return;
            }
            message = output;
        } else if parse_weave_relay_output(&message).is_some() {
            self.add_to_history(history_cell::new_error_event(
                "Unexpected relay output; start a weave task to use relay_actions.".to_string(),
            ));
            self.handle_stream_finished();
            self.request_redraw();
            return;
        }
        let reply_message = self
            .active_weave_reply_target
            .as_ref()
            .map(|_| message.clone());
        // If we have a stream_controller, then the final agent message is redundant and will be a
        // duplicate of what has already been streamed.
        if self.stream_controller.is_none() {
            self.handle_streaming_delta(message);
        }
        self.flush_answer_stream_with_separator();
        self.handle_stream_finished();
        if let Some(reply_target) = self.active_weave_reply_target.take()
            && let Some(message) = reply_message
        {
            self.send_weave_reply(reply_target, message);
        }
        self.request_redraw();
    }

    fn on_agent_message_delta(&mut self, delta: String) {
        if self.pending_weave_relay.is_some() {
            self.weave_relay_buffer.push_str(&delta);
            return;
        }
        self.handle_streaming_delta(delta);
    }

    fn on_agent_reasoning_delta(&mut self, delta: String) {
        // For reasoning deltas, do not stream to history. Accumulate the
        // current reasoning block and extract the first bold element
        // (between **/**) as the chunk header. Show this header as status.
        self.reasoning_buffer.push_str(&delta);

        if let Some(header) = extract_first_bold(&self.reasoning_buffer) {
            // Update the shimmer header to the extracted reasoning chunk header.
            self.set_status_header(header);
        } else {
            // Fallback while we don't yet have a bold header: leave existing header as-is.
        }
        self.request_redraw();
    }

    fn on_agent_reasoning_final(&mut self) {
        // At the end of a reasoning block, record transcript-only content.
        self.full_reasoning_buffer.push_str(&self.reasoning_buffer);
        if !self.full_reasoning_buffer.is_empty() {
            let cell =
                history_cell::new_reasoning_summary_block(self.full_reasoning_buffer.clone());
            self.add_boxed_history(cell);
        }
        self.reasoning_buffer.clear();
        self.full_reasoning_buffer.clear();
        self.request_redraw();
    }

    fn on_reasoning_section_break(&mut self) {
        // Start a new reasoning block for header extraction and accumulate transcript.
        self.full_reasoning_buffer.push_str(&self.reasoning_buffer);
        self.full_reasoning_buffer.push_str("\n\n");
        self.reasoning_buffer.clear();
    }

    // Raw reasoning uses the same flow as summarized reasoning

    fn on_task_started(&mut self) {
        self.bottom_pane.clear_ctrl_c_quit_hint();
        self.bottom_pane.set_task_running(true);
        self.retry_status_header = None;
        self.bottom_pane.set_interrupt_hint_visible(true);
        self.set_status_header(String::from("Working"));
        self.relay_output_consumed = false;
        self.full_reasoning_buffer.clear();
        self.reasoning_buffer.clear();
        self.request_redraw();
    }

    fn on_task_complete(&mut self, last_agent_message: Option<String>) {
        let mut last_agent_message = last_agent_message;
        if self.pending_weave_relay.is_some()
            && (!self.weave_relay_buffer.trim().is_empty()
                || last_agent_message
                    .as_ref()
                    .is_some_and(|message| !message.trim().is_empty()))
        {
            let output =
                self.take_weave_relay_output(last_agent_message.as_deref().unwrap_or_default());
            if let Some(output) = output
                && self.handle_weave_relay_output(&output)
            {
                self.relay_output_consumed = true;
                last_agent_message = None;
            }
        } else if !self.relay_output_consumed
            && last_agent_message
                .as_ref()
                .is_some_and(|message| !message.trim().is_empty())
            && parse_weave_relay_output(last_agent_message.as_deref().unwrap_or_default()).is_some()
        {
            self.add_to_history(history_cell::new_error_event(
                "Unexpected relay output; start a weave task to use relay_actions.".to_string(),
            ));
            last_agent_message = None;
        }
        self.relay_output_consumed = false;
        // If a stream is currently active, finalize it.
        self.flush_answer_stream_with_separator();
        self.flush_wait_cell();
        let reply_message = last_agent_message
            .as_ref()
            .filter(|message| !message.trim().is_empty())
            .map(ToString::to_string);
        if let Some(reply_target) = self.active_weave_reply_target.take()
            && let Some(message) = reply_message
        {
            self.send_weave_reply(reply_target, message);
        }
        self.active_weave_control_context = None;
        // Mark task stopped and request redraw now that all content is in history.
        self.bottom_pane.set_task_running(false);
        self.running_commands.clear();
        self.suppressed_exec_calls.clear();
        self.last_unified_wait = None;
        self.request_redraw();

        // If there is a queued user message, send exactly one now to begin the next turn.
        let started_new_session = self.maybe_start_pending_weave_new_session();
        if !started_new_session {
            self.maybe_send_next_queued_input();
        }
        // Emit a notification when the turn completes (suppressed if focused).
        self.notify(Notification::AgentTurnComplete {
            response: last_agent_message.unwrap_or_default(),
        });

        self.maybe_show_pending_rate_limit_prompt();
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        match info {
            Some(info) => self.apply_token_info(info),
            None => {
                self.bottom_pane.set_context_window(None, None);
                self.token_info = None;
            }
        }
    }

    fn apply_token_info(&mut self, info: TokenUsageInfo) {
        let percent = self.context_remaining_percent(&info);
        let used_tokens = self.context_used_tokens(&info, percent.is_some());
        self.bottom_pane.set_context_window(percent, used_tokens);
        self.token_info = Some(info);
    }

    fn context_remaining_percent(&self, info: &TokenUsageInfo) -> Option<i64> {
        info.model_context_window.map(|window| {
            info.last_token_usage
                .percent_of_context_window_remaining(window)
        })
    }

    fn context_used_tokens(&self, info: &TokenUsageInfo, percent_known: bool) -> Option<i64> {
        if percent_known {
            return None;
        }

        Some(info.total_token_usage.tokens_in_context_window())
    }

    fn restore_pre_review_token_info(&mut self) {
        if let Some(saved) = self.pre_review_token_info.take() {
            match saved {
                Some(info) => self.apply_token_info(info),
                None => {
                    self.bottom_pane.set_context_window(None, None);
                    self.token_info = None;
                }
            }
        }
    }

    pub(crate) fn on_rate_limit_snapshot(&mut self, snapshot: Option<RateLimitSnapshot>) {
        if let Some(mut snapshot) = snapshot {
            if snapshot.credits.is_none() {
                snapshot.credits = self
                    .rate_limit_snapshot
                    .as_ref()
                    .and_then(|display| display.credits.as_ref())
                    .map(|credits| CreditsSnapshot {
                        has_credits: credits.has_credits,
                        unlimited: credits.unlimited,
                        balance: credits.balance.clone(),
                    });
            }

            self.plan_type = snapshot.plan_type.or(self.plan_type);

            let warnings = self.rate_limit_warnings.take_warnings(
                snapshot
                    .secondary
                    .as_ref()
                    .map(|window| window.used_percent),
                snapshot
                    .secondary
                    .as_ref()
                    .and_then(|window| window.window_minutes),
                snapshot.primary.as_ref().map(|window| window.used_percent),
                snapshot
                    .primary
                    .as_ref()
                    .and_then(|window| window.window_minutes),
            );

            let high_usage = snapshot
                .secondary
                .as_ref()
                .map(|w| w.used_percent >= RATE_LIMIT_SWITCH_PROMPT_THRESHOLD)
                .unwrap_or(false)
                || snapshot
                    .primary
                    .as_ref()
                    .map(|w| w.used_percent >= RATE_LIMIT_SWITCH_PROMPT_THRESHOLD)
                    .unwrap_or(false);

            if high_usage
                && !self.rate_limit_switch_prompt_hidden()
                && self.model != NUDGE_MODEL_SLUG
                && !matches!(
                    self.rate_limit_switch_prompt,
                    RateLimitSwitchPromptState::Shown
                )
            {
                self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Pending;
            }

            let display = crate::status::rate_limit_snapshot_display(&snapshot, Local::now());
            self.rate_limit_snapshot = Some(display);

            if !warnings.is_empty() {
                for warning in warnings {
                    self.add_to_history(history_cell::new_warning_event(warning));
                }
                self.request_redraw();
            }
        } else {
            self.rate_limit_snapshot = None;
        }
    }
    /// Finalize any active exec as failed and stop/clear running UI state.
    fn finalize_turn(&mut self) {
        // Ensure any spinner is replaced by a red ✗ and flushed into history.
        self.finalize_active_cell_as_failed();
        // Reset running state and clear streaming buffers.
        self.bottom_pane.set_task_running(false);
        self.running_commands.clear();
        self.suppressed_exec_calls.clear();
        self.last_unified_wait = None;
        self.active_weave_reply_target = None;
        self.stream_controller = None;
        self.maybe_show_pending_rate_limit_prompt();
    }

    fn on_error(&mut self, message: String) {
        self.finalize_turn();
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();

        // After an error ends the turn, try sending the next queued input.
        self.maybe_send_next_queued_input();
    }

    fn on_warning(&mut self, message: impl Into<String>) {
        self.add_to_history(history_cell::new_warning_event(message.into()));
        self.request_redraw();
    }

    fn on_mcp_startup_update(&mut self, ev: McpStartupUpdateEvent) {
        let mut status = self.mcp_startup_status.take().unwrap_or_default();
        if let McpStartupStatus::Failed { error } = &ev.status {
            self.on_warning(error);
        }
        status.insert(ev.server, ev.status);
        self.mcp_startup_status = Some(status);
        self.bottom_pane.set_task_running(true);
        if let Some(current) = &self.mcp_startup_status {
            let total = current.len();
            let mut starting: Vec<_> = current
                .iter()
                .filter_map(|(name, state)| {
                    if matches!(state, McpStartupStatus::Starting) {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect();
            starting.sort();
            if let Some(first) = starting.first() {
                let completed = total.saturating_sub(starting.len());
                let max_to_show = 3;
                let mut to_show: Vec<String> = starting
                    .iter()
                    .take(max_to_show)
                    .map(ToString::to_string)
                    .collect();
                if starting.len() > max_to_show {
                    to_show.push("…".to_string());
                }
                let header = if total > 1 {
                    format!(
                        "Starting MCP servers ({completed}/{total}): {}",
                        to_show.join(", ")
                    )
                } else {
                    format!("Booting MCP server: {first}")
                };
                self.set_status_header(header);
            }
        }
        self.request_redraw();
    }

    fn on_mcp_startup_complete(&mut self, ev: McpStartupCompleteEvent) {
        let mut parts = Vec::new();
        if !ev.failed.is_empty() {
            let failed_servers: Vec<_> = ev.failed.iter().map(|f| f.server.clone()).collect();
            parts.push(format!("failed: {}", failed_servers.join(", ")));
        }
        if !ev.cancelled.is_empty() {
            self.on_warning(format!(
                "MCP startup interrupted. The following servers were not initialized: {}",
                ev.cancelled.join(", ")
            ));
        }
        if !parts.is_empty() {
            self.on_warning(format!("MCP startup incomplete ({})", parts.join("; ")));
        }

        self.mcp_startup_status = None;
        self.bottom_pane.set_task_running(false);
        self.maybe_send_next_queued_input();
        self.request_redraw();
    }

    /// Handle a turn aborted due to user interrupt (Esc).
    /// When there are queued user messages, restore them into the composer
    /// separated by newlines rather than auto‑submitting the next one.
    fn on_interrupted_turn(&mut self, reason: TurnAbortReason) {
        let mut cancel_target = if reason == TurnAbortReason::Interrupted {
            self.active_weave_reply_target.clone()
        } else {
            None
        };
        let cancel_context = if reason == TurnAbortReason::Interrupted {
            self.active_weave_relay.clone()
        } else {
            None
        };
        // Finalize, log a gentle prompt, and clear running state.
        self.finalize_turn();
        self.unified_exec_processes.clear();
        self.sync_unified_exec_footer();
        self.active_weave_control_context = None;

        if reason != TurnAbortReason::ReviewEnded {
            self.add_to_history(history_cell::new_error_event(
                "Conversation interrupted - tell the model what to do differently. Something went wrong? Hit `/feedback` to report the issue.".to_owned(),
            ));
        }

        // If any messages were queued during the task, restore them into the composer.
        if !self.queued_user_messages.is_empty() {
            let queued_text = self
                .queued_user_messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>()
                .join("\n");
            let existing_text = self.bottom_pane.composer_text();
            let combined = if existing_text.is_empty() {
                queued_text
            } else if queued_text.is_empty() {
                existing_text
            } else {
                format!("{queued_text}\n{existing_text}")
            };
            self.bottom_pane.set_composer_text(combined);
            // Clear the queue and update the status indicator list.
            self.queued_user_messages.clear();
            self.refresh_queued_user_messages();
        }

        if let Some(targets) = cancel_context {
            self.cancel_active_weave_task_on_interrupt(targets);
            cancel_target = None;
        }

        if self.suppress_weave_interrupt {
            self.suppress_weave_interrupt = false;
        } else if let Some(target) = cancel_target {
            self.send_weave_interrupt(target);
        }

        self.maybe_start_pending_weave_new_session();
        self.request_redraw();
    }

    fn on_plan_update(&mut self, update: UpdatePlanArgs) {
        self.add_to_history(history_cell::new_plan_update(update));
    }

    fn on_exec_approval_request(&mut self, id: String, ev: ExecApprovalRequestEvent) {
        let id2 = id.clone();
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_exec_approval(id, ev),
            |s| s.handle_exec_approval_now(id2, ev2),
        );
    }

    fn on_apply_patch_approval_request(&mut self, id: String, ev: ApplyPatchApprovalRequestEvent) {
        let id2 = id.clone();
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_apply_patch_approval(id, ev),
            |s| s.handle_apply_patch_approval_now(id2, ev2),
        );
    }

    fn on_elicitation_request(&mut self, ev: ElicitationRequestEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_elicitation(ev),
            |s| s.handle_elicitation_request_now(ev2),
        );
    }

    fn on_exec_command_begin(&mut self, ev: ExecCommandBeginEvent) {
        self.flush_answer_stream_with_separator();
        if is_unified_exec_source(ev.source) {
            self.track_unified_exec_process_begin(&ev);
            if !is_standard_tool_call(&ev.parsed_cmd) {
                return;
            }
        }
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_exec_begin(ev), |s| s.handle_exec_begin_now(ev2));
    }

    fn on_exec_command_output_delta(
        &mut self,
        _ev: codex_core::protocol::ExecCommandOutputDeltaEvent,
    ) {
        // TODO: Handle streaming exec output if/when implemented
    }

    fn on_terminal_interaction(&mut self, ev: TerminalInteractionEvent) {
        self.flush_answer_stream_with_separator();
        let command_display = self
            .unified_exec_processes
            .iter()
            .find(|process| process.key == ev.process_id)
            .map(|process| process.command_display.clone());
        if ev.stdin.is_empty() {
            // Empty stdin means we are still waiting on background output; keep a live shimmer cell.
            if let Some(wait_cell) = self.active_cell.as_mut().and_then(|cell| {
                cell.as_any_mut()
                    .downcast_mut::<history_cell::UnifiedExecWaitCell>()
            }) && wait_cell.matches(command_display.as_deref())
            {
                // Same process still waiting; update command display if it shows up late.
                wait_cell.update_command_display(command_display);
                self.request_redraw();
                return;
            }
            let has_non_wait_active = matches!(
                self.active_cell.as_ref(),
                Some(active)
                    if active
                        .as_any()
                        .downcast_ref::<history_cell::UnifiedExecWaitCell>()
                        .is_none()
            );
            if has_non_wait_active {
                // Do not preempt non-wait active cells with a wait entry.
                return;
            }
            self.flush_wait_cell();
            self.active_cell = Some(Box::new(history_cell::new_unified_exec_wait_live(
                command_display,
                self.config.animations,
            )));
            self.request_redraw();
        } else {
            if let Some(wait_cell) = self.active_cell.as_ref().and_then(|cell| {
                cell.as_any()
                    .downcast_ref::<history_cell::UnifiedExecWaitCell>()
            }) {
                // Convert the live wait cell into a static "(waited)" entry before logging stdin.
                let waited_command = wait_cell.command_display().or(command_display.clone());
                self.active_cell = None;
                self.add_to_history(history_cell::new_unified_exec_interaction(
                    waited_command,
                    String::new(),
                ));
            }
            self.add_to_history(history_cell::new_unified_exec_interaction(
                command_display,
                ev.stdin,
            ));
        }
    }

    fn on_patch_apply_begin(&mut self, event: PatchApplyBeginEvent) {
        self.add_to_history(history_cell::new_patch_event(
            event.changes,
            &self.config.cwd,
        ));
    }

    fn on_view_image_tool_call(&mut self, event: ViewImageToolCallEvent) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_view_image_tool_call(
            event.path,
            &self.config.cwd,
        ));
        self.request_redraw();
    }

    fn on_patch_apply_end(&mut self, event: codex_core::protocol::PatchApplyEndEvent) {
        let ev2 = event.clone();
        self.defer_or_handle(
            |q| q.push_patch_end(event),
            |s| s.handle_patch_apply_end_now(ev2),
        );
    }

    fn on_exec_command_end(&mut self, ev: ExecCommandEndEvent) {
        if is_unified_exec_source(ev.source) {
            self.track_unified_exec_process_end(&ev);
            if !self.bottom_pane.is_task_running() {
                return;
            }
        }
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_exec_end(ev), |s| s.handle_exec_end_now(ev2));
    }

    fn track_unified_exec_process_begin(&mut self, ev: &ExecCommandBeginEvent) {
        if ev.source != ExecCommandSource::UnifiedExecStartup {
            return;
        }
        let key = ev.process_id.clone().unwrap_or(ev.call_id.to_string());
        let command_display = strip_bash_lc_and_escape(&ev.command);
        if let Some(existing) = self
            .unified_exec_processes
            .iter_mut()
            .find(|process| process.key == key)
        {
            existing.command_display = command_display;
        } else {
            self.unified_exec_processes.push(UnifiedExecProcessSummary {
                key,
                command_display,
            });
        }
        self.sync_unified_exec_footer();
    }

    fn track_unified_exec_process_end(&mut self, ev: &ExecCommandEndEvent) {
        let key = ev.process_id.clone().unwrap_or(ev.call_id.to_string());
        let before = self.unified_exec_processes.len();
        self.unified_exec_processes
            .retain(|process| process.key != key);
        if self.unified_exec_processes.len() != before {
            self.sync_unified_exec_footer();
        }
    }

    fn sync_unified_exec_footer(&mut self) {
        let processes = self
            .unified_exec_processes
            .iter()
            .map(|process| process.command_display.clone())
            .collect();
        self.bottom_pane.set_unified_exec_processes(processes);
    }

    fn on_mcp_tool_call_begin(&mut self, ev: McpToolCallBeginEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_mcp_begin(ev), |s| s.handle_mcp_begin_now(ev2));
    }

    fn on_mcp_tool_call_end(&mut self, ev: McpToolCallEndEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_mcp_end(ev), |s| s.handle_mcp_end_now(ev2));
    }

    fn on_web_search_begin(&mut self, _ev: WebSearchBeginEvent) {
        self.flush_answer_stream_with_separator();
    }

    fn on_web_search_end(&mut self, ev: WebSearchEndEvent) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_web_search_call(ev.query));
    }

    fn on_get_history_entry_response(
        &mut self,
        event: codex_core::protocol::GetHistoryEntryResponseEvent,
    ) {
        let codex_core::protocol::GetHistoryEntryResponseEvent {
            offset,
            log_id,
            entry,
        } = event;
        self.bottom_pane
            .on_history_entry_response(log_id, offset, entry.map(|e| e.text));
    }

    fn on_shutdown_complete(&mut self) {
        self.request_exit();
    }

    fn on_turn_diff(&mut self, unified_diff: String) {
        debug!("TurnDiffEvent: {unified_diff}");
    }

    fn on_deprecation_notice(&mut self, event: DeprecationNoticeEvent) {
        let DeprecationNoticeEvent { summary, details } = event;
        self.add_to_history(history_cell::new_deprecation_notice(summary, details));
        self.request_redraw();
    }

    fn on_background_event(&mut self, message: String) {
        debug!("BackgroundEvent: {message}");
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane.set_interrupt_hint_visible(true);
        self.set_status_header(message);
    }

    fn on_undo_started(&mut self, event: UndoStartedEvent) {
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane.set_interrupt_hint_visible(false);
        let message = event
            .message
            .unwrap_or_else(|| "Undo in progress...".to_string());
        self.set_status_header(message);
    }

    fn on_undo_completed(&mut self, event: UndoCompletedEvent) {
        let UndoCompletedEvent { success, message } = event;
        self.bottom_pane.hide_status_indicator();
        let message = message.unwrap_or_else(|| {
            if success {
                "Undo completed successfully.".to_string()
            } else {
                "Undo failed.".to_string()
            }
        });
        if success {
            self.add_info_message(message, None);
        } else {
            self.add_error_message(message);
        }
    }

    fn on_stream_error(&mut self, message: String, additional_details: Option<String>) {
        if self.retry_status_header.is_none() {
            self.retry_status_header = Some(self.current_status_header.clone());
        }
        self.set_status(message, additional_details);
    }

    /// Periodic tick to commit at most one queued line to history with a small delay,
    /// animating the output.
    pub(crate) fn on_commit_tick(&mut self) {
        if let Some(controller) = self.stream_controller.as_mut() {
            let (cell, is_idle) = controller.on_commit_tick();
            if let Some(cell) = cell {
                self.bottom_pane.hide_status_indicator();
                self.add_boxed_history(cell);
            }
            if is_idle {
                self.app_event_tx.send(AppEvent::StopCommitAnimation);
            }
        }
    }

    fn flush_interrupt_queue(&mut self) {
        let mut mgr = std::mem::take(&mut self.interrupts);
        mgr.flush_all(self);
        self.interrupts = mgr;
    }

    #[inline]
    fn defer_or_handle(
        &mut self,
        push: impl FnOnce(&mut InterruptManager),
        handle: impl FnOnce(&mut Self),
    ) {
        // Preserve deterministic FIFO across queued interrupts: once anything
        // is queued due to an active write cycle, continue queueing until the
        // queue is flushed to avoid reordering (e.g., ExecEnd before ExecBegin).
        if self.stream_controller.is_some() || !self.interrupts.is_empty() {
            push(&mut self.interrupts);
        } else {
            handle(self);
        }
    }

    fn handle_stream_finished(&mut self) {
        if self.task_complete_pending {
            self.bottom_pane.hide_status_indicator();
            self.task_complete_pending = false;
        }
        // A completed stream indicates non-exec content was just inserted.
        self.flush_interrupt_queue();
    }

    #[inline]
    fn handle_streaming_delta(&mut self, delta: String) {
        // Before streaming agent content, flush any active exec cell group.
        self.flush_active_cell();

        if self.stream_controller.is_none() {
            if self.needs_final_message_separator {
                let elapsed_seconds = self
                    .bottom_pane
                    .status_widget()
                    .map(super::status_indicator_widget::StatusIndicatorWidget::elapsed_seconds);
                self.add_to_history(history_cell::FinalMessageSeparator::new(elapsed_seconds));
                self.needs_final_message_separator = false;
            }
            self.stream_controller = Some(StreamController::new(
                self.last_rendered_width.get().map(|w| w.saturating_sub(2)),
            ));
        }
        if let Some(controller) = self.stream_controller.as_mut()
            && controller.push(&delta)
        {
            self.app_event_tx.send(AppEvent::StartCommitAnimation);
        }
        self.request_redraw();
    }

    pub(crate) fn handle_exec_end_now(&mut self, ev: ExecCommandEndEvent) {
        let running = self.running_commands.remove(&ev.call_id);
        if self.suppressed_exec_calls.remove(&ev.call_id) {
            return;
        }
        let (command, parsed, source) = match running {
            Some(rc) => (rc.command, rc.parsed_cmd, rc.source),
            None => (ev.command.clone(), ev.parsed_cmd.clone(), ev.source),
        };
        let is_unified_exec_interaction =
            matches!(source, ExecCommandSource::UnifiedExecInteraction);

        let needs_new = self
            .active_cell
            .as_ref()
            .map(|cell| cell.as_any().downcast_ref::<ExecCell>().is_none())
            .unwrap_or(true);
        if needs_new {
            self.flush_active_cell();
            self.active_cell = Some(Box::new(new_active_exec_command(
                ev.call_id.clone(),
                command,
                parsed,
                source,
                ev.interaction_input.clone(),
                self.config.animations,
            )));
        }

        if let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|c| c.as_any_mut().downcast_mut::<ExecCell>())
        {
            let output = if is_unified_exec_interaction {
                CommandOutput {
                    exit_code: ev.exit_code,
                    formatted_output: String::new(),
                    aggregated_output: String::new(),
                }
            } else {
                CommandOutput {
                    exit_code: ev.exit_code,
                    formatted_output: ev.formatted_output.clone(),
                    aggregated_output: ev.aggregated_output.clone(),
                }
            };
            cell.complete_call(&ev.call_id, output, ev.duration);
            if cell.should_flush() {
                self.flush_active_cell();
            }
        }
    }

    pub(crate) fn handle_patch_apply_end_now(
        &mut self,
        event: codex_core::protocol::PatchApplyEndEvent,
    ) {
        // If the patch was successful, just let the "Edited" block stand.
        // Otherwise, add a failure block.
        if !event.success {
            self.add_to_history(history_cell::new_patch_apply_failure(event.stderr));
        }
    }

    pub(crate) fn handle_exec_approval_now(&mut self, id: String, ev: ExecApprovalRequestEvent) {
        self.flush_answer_stream_with_separator();
        let command = shlex::try_join(ev.command.iter().map(String::as_str))
            .unwrap_or_else(|_| ev.command.join(" "));
        self.notify(Notification::ExecApprovalRequested { command });

        let request = ApprovalRequest::Exec {
            id,
            command: ev.command,
            reason: ev.reason,
            proposed_execpolicy_amendment: ev.proposed_execpolicy_amendment,
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub(crate) fn handle_apply_patch_approval_now(
        &mut self,
        id: String,
        ev: ApplyPatchApprovalRequestEvent,
    ) {
        self.flush_answer_stream_with_separator();

        let request = ApprovalRequest::ApplyPatch {
            id,
            reason: ev.reason,
            changes: ev.changes.clone(),
            cwd: self.config.cwd.clone(),
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
        self.notify(Notification::EditApprovalRequested {
            cwd: self.config.cwd.clone(),
            changes: ev.changes.keys().cloned().collect(),
        });
    }

    pub(crate) fn handle_elicitation_request_now(&mut self, ev: ElicitationRequestEvent) {
        self.flush_answer_stream_with_separator();

        self.notify(Notification::ElicitationRequested {
            server_name: ev.server_name.clone(),
        });

        let request = ApprovalRequest::McpElicitation {
            server_name: ev.server_name,
            request_id: ev.id,
            message: ev.message,
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub(crate) fn handle_exec_begin_now(&mut self, ev: ExecCommandBeginEvent) {
        // Ensure the status indicator is visible while the command runs.
        self.running_commands.insert(
            ev.call_id.clone(),
            RunningCommand {
                command: ev.command.clone(),
                parsed_cmd: ev.parsed_cmd.clone(),
                source: ev.source,
            },
        );
        let is_wait_interaction = matches!(ev.source, ExecCommandSource::UnifiedExecInteraction)
            && ev
                .interaction_input
                .as_deref()
                .map(str::is_empty)
                .unwrap_or(true);
        let command_display = ev.command.join(" ");
        let should_suppress_unified_wait = is_wait_interaction
            && self
                .last_unified_wait
                .as_ref()
                .is_some_and(|wait| wait.is_duplicate(&command_display));
        if is_wait_interaction {
            self.last_unified_wait = Some(UnifiedExecWaitState::new(command_display));
        } else {
            self.last_unified_wait = None;
        }
        if should_suppress_unified_wait {
            self.suppressed_exec_calls.insert(ev.call_id);
            return;
        }
        let interaction_input = ev.interaction_input.clone();
        if let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|c| c.as_any_mut().downcast_mut::<ExecCell>())
            && let Some(new_exec) = cell.with_added_call(
                ev.call_id.clone(),
                ev.command.clone(),
                ev.parsed_cmd.clone(),
                ev.source,
                interaction_input.clone(),
            )
        {
            *cell = new_exec;
        } else {
            self.flush_active_cell();

            self.active_cell = Some(Box::new(new_active_exec_command(
                ev.call_id.clone(),
                ev.command.clone(),
                ev.parsed_cmd,
                ev.source,
                interaction_input,
                self.config.animations,
            )));
        }

        self.request_redraw();
    }

    pub(crate) fn handle_mcp_begin_now(&mut self, ev: McpToolCallBeginEvent) {
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.active_cell = Some(Box::new(history_cell::new_active_mcp_tool_call(
            ev.call_id,
            ev.invocation,
            self.config.animations,
        )));
        self.request_redraw();
    }
    pub(crate) fn handle_mcp_end_now(&mut self, ev: McpToolCallEndEvent) {
        self.flush_answer_stream_with_separator();

        let McpToolCallEndEvent {
            call_id,
            invocation,
            duration,
            result,
        } = ev;

        let extra_cell = match self
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<McpToolCallCell>())
        {
            Some(cell) if cell.call_id() == call_id => cell.complete(duration, result),
            _ => {
                self.flush_active_cell();
                let mut cell = history_cell::new_active_mcp_tool_call(
                    call_id,
                    invocation,
                    self.config.animations,
                );
                let extra_cell = cell.complete(duration, result);
                self.active_cell = Some(Box::new(cell));
                extra_cell
            }
        };

        self.flush_active_cell();
        if let Some(extra) = extra_cell {
            self.add_boxed_history(extra);
        }
    }

    pub(crate) fn new(common: ChatWidgetInit, thread_manager: Arc<ThreadManager>) -> Self {
        let ChatWidgetInit {
            config,
            frame_requester,
            app_event_tx,
            initial_prompt,
            initial_images,
            enhanced_keys_supported,
            auth_manager,
            models_manager,
            feedback,
            is_first_run,
            model,
        } = common;
        let mut config = config;
        config.model = Some(model.clone());
        let mut rng = rand::rng();
        let placeholder = EXAMPLE_PROMPTS[rng.random_range(0..EXAMPLE_PROMPTS.len())].to_string();
        let codex_op_tx = spawn_agent(config.clone(), app_event_tx.clone(), thread_manager);

        let weave_agent_id = Uuid::new_v4().to_string();
        let weave_agent_name = Self::default_weave_agent_name(&weave_agent_id);

        let mut widget = Self {
            app_event_tx: app_event_tx.clone(),
            frame_requester: frame_requester.clone(),
            codex_op_tx,
            bottom_pane: BottomPane::new(BottomPaneParams {
                frame_requester,
                app_event_tx,
                has_input_focus: true,
                enhanced_keys_supported,
                placeholder_text: placeholder,
                disable_paste_burst: config.disable_paste_burst,
                animations_enabled: config.animations,
                skills: None,
            }),
            active_cell: None,
            config,
            model: model.clone(),
            auth_manager,
            models_manager,
            session_header: SessionHeader::new(model),
            initial_user_message: create_initial_user_message(
                initial_prompt.unwrap_or_default(),
                initial_images,
            ),
            token_info: None,
            rate_limit_snapshot: None,
            plan_type: None,
            rate_limit_warnings: RateLimitWarningState::default(),
            rate_limit_switch_prompt: RateLimitSwitchPromptState::default(),
            rate_limit_poller: None,
            stream_controller: None,
            running_commands: HashMap::new(),
            suppressed_exec_calls: HashSet::new(),
            last_unified_wait: None,
            task_complete_pending: false,
            unified_exec_processes: Vec::new(),
            mcp_startup_status: None,
            interrupts: InterruptManager::new(),
            suppress_weave_interrupt: false,
            reasoning_buffer: String::new(),
            full_reasoning_buffer: String::new(),
            current_status_header: String::from("Working"),
            retry_status_header: None,
            thread_id: None,
            selected_weave_session_id: None,
            selected_weave_session_name: None,
            weave_agent_id,
            weave_agent_name,
            weave_agent_connection: None,
            weave_agents: None,
            pending_weave_relay: None,
            active_weave_relay: None,
            active_weave_plan: None,
            pending_weave_plan: false,
            weave_relay_buffer: String::new(),
            relay_output_consumed: false,
            weave_inbound_task_ids: HashMap::new(),
            weave_target_states: HashMap::new(),
            pending_weave_action_messages: VecDeque::new(),
            pending_weave_new_session_result: None,
            pending_weave_new_session_context: None,
            pending_weave_new_session: false,
            queued_user_messages: VecDeque::new(),
            active_weave_reply_target: None,
            active_weave_control_context: None,
            show_welcome_banner: is_first_run,
            suppress_session_configured_redraw: false,
            pending_notification: None,
            is_review_mode: false,
            pre_review_token_info: None,
            needs_final_message_separator: false,
            last_rendered_width: std::cell::Cell::new(None),
            feedback,
            current_rollout_path: None,
            external_editor_state: ExternalEditorState::Closed,
        };

        widget
            .bottom_pane
            .set_weave_agent_identity(widget.weave_agent_id.clone());
        widget.prefetch_rate_limits();

        widget
    }

    /// Create a ChatWidget attached to an existing conversation (e.g., a fork).
    pub(crate) fn new_from_existing(
        common: ChatWidgetInit,
        conversation: std::sync::Arc<codex_core::CodexThread>,
        session_configured: codex_core::protocol::SessionConfiguredEvent,
    ) -> Self {
        let ChatWidgetInit {
            config,
            frame_requester,
            app_event_tx,
            initial_prompt,
            initial_images,
            enhanced_keys_supported,
            auth_manager,
            models_manager,
            feedback,
            model,
            ..
        } = common;
        let mut rng = rand::rng();
        let placeholder = EXAMPLE_PROMPTS[rng.random_range(0..EXAMPLE_PROMPTS.len())].to_string();

        let codex_op_tx =
            spawn_agent_from_existing(conversation, session_configured, app_event_tx.clone());

        let weave_agent_id = Uuid::new_v4().to_string();
        let weave_agent_name = Self::default_weave_agent_name(&weave_agent_id);

        let mut widget = Self {
            app_event_tx: app_event_tx.clone(),
            frame_requester: frame_requester.clone(),
            codex_op_tx,
            bottom_pane: BottomPane::new(BottomPaneParams {
                frame_requester,
                app_event_tx,
                has_input_focus: true,
                enhanced_keys_supported,
                placeholder_text: placeholder,
                disable_paste_burst: config.disable_paste_burst,
                animations_enabled: config.animations,
                skills: None,
            }),
            active_cell: None,
            config,
            model: model.clone(),
            auth_manager,
            models_manager,
            session_header: SessionHeader::new(model),
            initial_user_message: create_initial_user_message(
                initial_prompt.unwrap_or_default(),
                initial_images,
            ),
            token_info: None,
            rate_limit_snapshot: None,
            plan_type: None,
            rate_limit_warnings: RateLimitWarningState::default(),
            rate_limit_switch_prompt: RateLimitSwitchPromptState::default(),
            rate_limit_poller: None,
            stream_controller: None,
            running_commands: HashMap::new(),
            suppressed_exec_calls: HashSet::new(),
            last_unified_wait: None,
            task_complete_pending: false,
            unified_exec_processes: Vec::new(),
            mcp_startup_status: None,
            interrupts: InterruptManager::new(),
            suppress_weave_interrupt: false,
            reasoning_buffer: String::new(),
            full_reasoning_buffer: String::new(),
            current_status_header: String::from("Working"),
            retry_status_header: None,
            thread_id: None,
            selected_weave_session_id: None,
            selected_weave_session_name: None,
            weave_agent_id,
            weave_agent_name,
            weave_agent_connection: None,
            weave_agents: None,
            pending_weave_relay: None,
            active_weave_relay: None,
            active_weave_plan: None,
            pending_weave_plan: false,
            weave_relay_buffer: String::new(),
            relay_output_consumed: false,
            weave_inbound_task_ids: HashMap::new(),
            weave_target_states: HashMap::new(),
            pending_weave_action_messages: VecDeque::new(),
            pending_weave_new_session_result: None,
            pending_weave_new_session_context: None,
            pending_weave_new_session: false,
            queued_user_messages: VecDeque::new(),
            active_weave_reply_target: None,
            active_weave_control_context: None,
            show_welcome_banner: false,
            suppress_session_configured_redraw: true,
            pending_notification: None,
            is_review_mode: false,
            pre_review_token_info: None,
            needs_final_message_separator: false,
            last_rendered_width: std::cell::Cell::new(None),
            feedback,
            current_rollout_path: None,
            external_editor_state: ExternalEditorState::Closed,
        };

        widget
            .bottom_pane
            .set_weave_agent_identity(widget.weave_agent_id.clone());
        widget.prefetch_rate_limits();

        widget
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'c') => {
                self.on_ctrl_c();
                return;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && c.eq_ignore_ascii_case(&'v') =>
            {
                match paste_image_to_temp_png() {
                    Ok((path, info)) => {
                        self.attach_image(
                            path,
                            info.width,
                            info.height,
                            info.encoded_format.label(),
                        );
                    }
                    Err(err) => {
                        tracing::warn!("failed to paste image: {err}");
                        self.add_to_history(history_cell::new_error_event(format!(
                            "Failed to paste image: {err}",
                        )));
                    }
                }
                return;
            }
            other if other.kind == KeyEventKind::Press => {
                self.bottom_pane.clear_ctrl_c_quit_hint();
            }
            _ => {}
        }

        match key_event {
            KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::ALT,
                kind: KeyEventKind::Press,
                ..
            } if !self.queued_user_messages.is_empty() => {
                // Prefer the most recently queued item.
                if let Some(user_message) = self.queued_user_messages.pop_back() {
                    self.bottom_pane.set_composer_text(user_message.text);
                    self.refresh_queued_user_messages();
                    self.request_redraw();
                }
            }
            _ => {
                match self.bottom_pane.handle_key_event(key_event) {
                    InputResult::Submitted(text) => {
                        // If a task is running, queue the user input to be sent after the turn completes.
                        let user_message = UserMessage {
                            text,
                            image_paths: self.bottom_pane.take_recent_submission_images(),
                            source: UserMessageSource::Local,
                        };
                        self.queue_user_message(user_message);
                    }
                    InputResult::Command(cmd) => {
                        self.dispatch_command(cmd);
                    }
                    InputResult::CommandWithArgs(cmd, args) => {
                        self.dispatch_command_with_args(cmd, args);
                    }
                    InputResult::None => {}
                }
            }
        }
    }

    pub(crate) fn attach_image(
        &mut self,
        path: PathBuf,
        width: u32,
        height: u32,
        format_label: &str,
    ) {
        tracing::info!(
            "attach_image path={path:?} width={width} height={height} format={format_label}",
        );
        self.bottom_pane
            .attach_image(path, width, height, format_label);
        self.request_redraw();
    }

    pub(crate) fn composer_text_with_pending(&self) -> String {
        self.bottom_pane.composer_text_with_pending()
    }

    pub(crate) fn apply_external_edit(&mut self, text: String) {
        self.bottom_pane.apply_external_edit(text);
        self.request_redraw();
    }

    pub(crate) fn external_editor_state(&self) -> ExternalEditorState {
        self.external_editor_state
    }

    pub(crate) fn set_external_editor_state(&mut self, state: ExternalEditorState) {
        self.external_editor_state = state;
    }

    pub(crate) fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.bottom_pane.set_footer_hint_override(items);
    }

    pub(crate) fn can_launch_external_editor(&self) -> bool {
        self.bottom_pane.can_launch_external_editor()
    }

    fn dispatch_command(&mut self, cmd: SlashCommand) {
        if !cmd.available_during_task() && self.bottom_pane.is_task_running() {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.request_redraw();
            return;
        }
        match cmd {
            SlashCommand::Feedback => {
                if !self.config.feedback_enabled {
                    let params = crate::bottom_pane::feedback_disabled_params();
                    self.bottom_pane.show_selection_view(params);
                    self.request_redraw();
                    return;
                }
                // Step 1: pick a category (UI built in feedback_view)
                let params =
                    crate::bottom_pane::feedback_selection_params(self.app_event_tx.clone());
                self.bottom_pane.show_selection_view(params);
                self.request_redraw();
            }
            SlashCommand::New => {
                self.app_event_tx.send(AppEvent::NewSession);
            }
            SlashCommand::Resume => {
                self.app_event_tx.send(AppEvent::OpenResumePicker);
            }
            SlashCommand::Weave => {
                self.request_weave_session_menu();
            }
            SlashCommand::Init => {
                let init_target = self.config.cwd.join(DEFAULT_PROJECT_DOC_FILENAME);
                if init_target.exists() {
                    let message = format!(
                        "{DEFAULT_PROJECT_DOC_FILENAME} already exists here. Skipping /init to avoid overwriting it."
                    );
                    self.add_info_message(message, None);
                    return;
                }
                const INIT_PROMPT: &str = include_str!("../prompt_for_init_command.md");
                self.submit_user_message(INIT_PROMPT.to_string().into());
            }
            SlashCommand::Compact => {
                self.clear_token_usage();
                self.app_event_tx.send(AppEvent::CodexOp(Op::Compact));
            }
            SlashCommand::Review => {
                self.open_review_popup();
            }
            SlashCommand::Model => {
                self.open_model_popup();
            }
            SlashCommand::Approvals => {
                self.open_approvals_popup();
            }
            SlashCommand::ElevateSandbox => {
                #[cfg(target_os = "windows")]
                {
                    let windows_degraded_sandbox_enabled = codex_core::get_platform_sandbox()
                        .is_some()
                        && !codex_core::is_windows_elevated_sandbox_enabled();
                    if !windows_degraded_sandbox_enabled
                        || !codex_core::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                    {
                        // This command should not be visible/recognized outside degraded mode,
                        // but guard anyway in case something dispatches it directly.
                        return;
                    }

                    let Some(preset) = builtin_approval_presets()
                        .into_iter()
                        .find(|preset| preset.id == "auto")
                    else {
                        // Avoid panicking in interactive UI; treat this as a recoverable
                        // internal error.
                        self.add_error_message(
                            "Internal error: missing the 'auto' approval preset.".to_string(),
                        );
                        return;
                    };

                    if let Err(err) = self.config.approval_policy.can_set(&preset.approval) {
                        self.add_error_message(err.to_string());
                        return;
                    }

                    self.app_event_tx
                        .send(AppEvent::BeginWindowsSandboxElevatedSetup { preset });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    // Not supported; on non-Windows this command should never be reachable.
                };
            }
            SlashCommand::Experimental => {
                self.open_experimental_popup();
            }
            SlashCommand::Quit | SlashCommand::Exit => {
                self.request_exit();
            }
            SlashCommand::Logout => {
                if let Err(e) = codex_core::auth::logout(
                    &self.config.codex_home,
                    self.config.cli_auth_credentials_store_mode,
                ) {
                    tracing::error!("failed to logout: {e}");
                }
                self.request_exit();
            }
            // SlashCommand::Undo => {
            //     self.app_event_tx.send(AppEvent::CodexOp(Op::Undo));
            // }
            SlashCommand::Diff => {
                self.add_diff_in_progress();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let text = match get_git_diff().await {
                        Ok((is_git_repo, diff_text)) => {
                            if is_git_repo {
                                diff_text
                            } else {
                                "`/diff` — _not inside a git repository_".to_string()
                            }
                        }
                        Err(e) => format!("Failed to compute diff: {e}"),
                    };
                    tx.send(AppEvent::DiffResult(text));
                });
            }
            SlashCommand::Mention => {
                self.insert_str("@");
            }
            SlashCommand::Skills => {
                self.insert_str("$");
            }
            SlashCommand::Status => {
                self.add_status_output();
            }
            SlashCommand::Ps => {
                self.add_ps_output();
            }
            SlashCommand::Mcp => {
                self.add_mcp_output();
            }
            SlashCommand::Rollout => {
                if let Some(path) = self.rollout_path() {
                    self.add_info_message(
                        format!("Current rollout path: {}", path.display()),
                        None,
                    );
                } else {
                    self.add_info_message("Rollout path is not available yet.".to_string(), None);
                }
            }
            SlashCommand::TestApproval => {
                use codex_core::protocol::EventMsg;
                use std::collections::HashMap;

                use codex_core::protocol::ApplyPatchApprovalRequestEvent;
                use codex_core::protocol::FileChange;

                self.app_event_tx.send(AppEvent::CodexEvent(Event {
                    id: "1".to_string(),
                    // msg: EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
                    //     call_id: "1".to_string(),
                    //     command: vec!["git".into(), "apply".into()],
                    //     cwd: self.config.cwd.clone(),
                    //     reason: Some("test".to_string()),
                    // }),
                    msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                        call_id: "1".to_string(),
                        turn_id: "turn-1".to_string(),
                        changes: HashMap::from([
                            (
                                PathBuf::from("/tmp/test.txt"),
                                FileChange::Add {
                                    content: "test".to_string(),
                                },
                            ),
                            (
                                PathBuf::from("/tmp/test2.txt"),
                                FileChange::Update {
                                    unified_diff: "+test\n-test2".to_string(),
                                    move_path: None,
                                },
                            ),
                        ]),
                        reason: None,
                        grant_root: Some(PathBuf::from("/tmp")),
                    }),
                }));
            }
        }
    }

    fn dispatch_command_with_args(&mut self, cmd: SlashCommand, args: String) {
        if !cmd.available_during_task() && self.bottom_pane.is_task_running() {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.request_redraw();
            return;
        }

        let trimmed = args.trim();
        match cmd {
            SlashCommand::Review if !trimmed.is_empty() => {
                self.submit_op(Op::Review {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Custom {
                            instructions: trimmed.to_string(),
                        },
                        user_facing_hint: None,
                    },
                });
            }
            _ => self.dispatch_command(cmd),
        }
    }

    pub(crate) fn handle_paste(&mut self, text: String) {
        self.bottom_pane.handle_paste(text);
    }

    // Returns true if caller should skip rendering this frame (a future frame is scheduled).
    pub(crate) fn handle_paste_burst_tick(&mut self, frame_requester: FrameRequester) -> bool {
        if self.bottom_pane.flush_paste_burst_if_due() {
            // A paste just flushed; request an immediate redraw and skip this frame.
            self.request_redraw();
            true
        } else if self.bottom_pane.is_in_paste_burst() {
            // While capturing a burst, schedule a follow-up tick and skip this frame
            // to avoid redundant renders between ticks.
            frame_requester.schedule_frame_in(
                crate::bottom_pane::ChatComposer::recommended_paste_flush_delay(),
            );
            true
        } else {
            false
        }
    }

    fn flush_active_cell(&mut self) {
        self.flush_wait_cell();
        if let Some(active) = self.active_cell.take() {
            self.needs_final_message_separator = true;
            self.app_event_tx.send(AppEvent::InsertHistoryCell(active));
        }
    }

    // Only flush a live wait cell here; other active cells must finalize via their end events.
    fn flush_wait_cell(&mut self) {
        // Wait cells are transient: convert them into "(waited)" history entries if present.
        // Leave non-wait active cells intact so their end events can finalize them.
        let Some(active) = self.active_cell.take() else {
            return;
        };
        let Some(wait_cell) = active
            .as_any()
            .downcast_ref::<history_cell::UnifiedExecWaitCell>()
        else {
            self.active_cell = Some(active);
            return;
        };
        self.needs_final_message_separator = true;
        let cell =
            history_cell::new_unified_exec_interaction(wait_cell.command_display(), String::new());
        self.app_event_tx
            .send(AppEvent::InsertHistoryCell(Box::new(cell)));
    }

    pub(crate) fn add_to_history(&mut self, cell: impl HistoryCell + 'static) {
        self.add_boxed_history(Box::new(cell));
    }

    fn add_boxed_history(&mut self, cell: Box<dyn HistoryCell>) {
        if !cell.display_lines(u16::MAX).is_empty() {
            // Only break exec grouping if the cell renders visible lines.
            self.flush_active_cell();
            self.needs_final_message_separator = true;
        }
        self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
    }

    fn add_agent_message(&mut self, message: &str) {
        let message = message.trim();
        if message.is_empty() {
            return;
        }
        let mut rendered = Vec::new();
        append_markdown(message, None, &mut rendered);
        if rendered.is_empty() {
            return;
        }
        self.add_to_history(AgentMessageCell::new(rendered, true));
    }

    fn send_weave_reply(&mut self, reply_target: WeaveReplyTarget, message: String) {
        if message.trim().is_empty() {
            return;
        }
        let WeaveReplyTarget {
            session_id,
            agent_id,
            display_name,
            conversation_id,
            conversation_owner,
            parent_message_id,
            task_id,
            action_group_id,
            action_id,
            action_index,
        } = reply_target;
        let Some(connection) = self.weave_agent_connection.as_ref() else {
            self.add_to_history(history_cell::new_error_event(format!(
                "Failed to send Weave reply to {display_name}: session not connected."
            )));
            return;
        };
        if self.selected_weave_session_id.as_deref() != Some(session_id.as_str()) {
            self.add_to_history(history_cell::new_error_event(format!(
                "Failed to send Weave reply to {display_name}: session changed."
            )));
            return;
        }
        let sender = connection.sender();
        let metadata = WeaveMessageMetadata {
            conversation_id,
            conversation_owner,
            parent_message_id,
            task_id,
        };
        let reply_to_action_id = action_id.clone();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = sender
                .send_reply_with_metadata(
                    agent_id.clone(),
                    message,
                    Some(&metadata),
                    reply_to_action_id.as_deref(),
                )
                .await;
            match result {
                Ok(()) => {
                    if let (Some(group_id), Some(action_id), Some(action_index)) = (
                        action_group_id.as_deref(),
                        action_id.as_deref(),
                        action_index,
                    ) {
                        let action_result = Self::build_weave_action_result(
                            group_id,
                            action_id,
                            action_index,
                            "completed",
                            None,
                            None,
                            None,
                        );
                        let _ = sender.send_action_result(agent_id, action_result).await;
                    }
                }
                Err(err) => {
                    if let (Some(group_id), Some(action_id), Some(action_index)) = (
                        action_group_id.as_deref(),
                        action_id.as_deref(),
                        action_index,
                    ) {
                        let action_result = Self::build_weave_action_result(
                            group_id,
                            action_id,
                            action_index,
                            "error",
                            Some(err.as_str()),
                            None,
                            None,
                        );
                        let _ = sender
                            .send_action_result(agent_id.clone(), action_result)
                            .await;
                    }
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!(
                            "Failed to send Weave reply to {display_name}: {err}"
                        )),
                    )));
                }
            }
        });
    }

    fn send_weave_interrupt(&mut self, reply_target: WeaveReplyTarget) {
        let WeaveReplyTarget {
            session_id,
            agent_id,
            display_name,
            conversation_id,
            conversation_owner,
            parent_message_id,
            task_id,
            action_group_id: _,
            action_id: _,
            action_index: _,
        } = reply_target;
        let Some(connection) = self.weave_agent_connection.as_ref() else {
            self.add_to_history(history_cell::new_error_event(format!(
                "Failed to send Weave interrupt to {display_name}: session not connected."
            )));
            return;
        };
        if self.selected_weave_session_id.as_deref() != Some(session_id.as_str()) {
            self.add_to_history(history_cell::new_error_event(format!(
                "Failed to send Weave interrupt to {display_name}: session changed."
            )));
            return;
        }
        let sender = connection.sender();
        let metadata = WeaveMessageMetadata {
            conversation_id,
            conversation_owner,
            parent_message_id,
            task_id,
        };
        let group_id = new_weave_action_group_id();
        let action_id = new_weave_action_id();
        let payload = Self::action_submit_payload(
            group_id.as_str(),
            vec![Self::weave_action_control_payload(
                agent_id.as_str(),
                &WeaveTool::Interrupt,
                action_id.as_str(),
                0,
            )],
            Some(&metadata),
        );
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            if let Err(err) = sender.send_action_submit(payload).await {
                app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_error_event(format!(
                        "Failed to send Weave interrupt to {display_name}: {err}"
                    )),
                )));
            }
        });
    }

    fn send_weave_action_result(&mut self, dst: &str, result: WeaveActionResult) {
        let Some(connection) = self.weave_agent_connection.as_ref() else {
            return;
        };
        let sender = connection.sender();
        let app_event_tx = self.app_event_tx.clone();
        let dst = dst.to_string();
        tokio::spawn(async move {
            if let Err(err) = sender.send_action_result(dst, result).await {
                app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_error_event(format!(
                        "Failed to send Weave action result: {err}"
                    )),
                )));
            }
        });
    }

    fn send_weave_action_result_for_context(
        &mut self,
        sender_id: &str,
        action_group_id: Option<&str>,
        action_id: Option<&str>,
        action_index: Option<usize>,
        result: WeaveActionResultContext<'_>,
    ) {
        if let (Some(group_id), Some(action_id), Some(action_index)) =
            (action_group_id, action_id, action_index)
        {
            let action_result = Self::build_weave_action_result(
                group_id,
                action_id,
                action_index,
                result.status,
                result.detail,
                result.new_context_id,
                result.new_task_id,
            );
            self.send_weave_action_result(sender_id, action_result);
        }
    }

    fn build_weave_action_result(
        group_id: &str,
        action_id: &str,
        action_index: usize,
        status: &str,
        detail: Option<&str>,
        new_context_id: Option<&str>,
        new_task_id: Option<&str>,
    ) -> WeaveActionResult {
        let detail = detail
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let new_context_id = new_context_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let new_task_id = new_task_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        WeaveActionResult {
            group_id: group_id.to_string(),
            action_id: action_id.to_string(),
            action_index,
            status: status.to_string(),
            detail,
            new_context_id,
            new_task_id,
        }
    }

    fn action_submit_payload(
        group_id: &str,
        actions: Vec<Value>,
        metadata: Option<&WeaveMessageMetadata>,
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

    fn weave_message_kind_label(kind: WeaveMessageKind) -> &'static str {
        match kind {
            WeaveMessageKind::User => "user",
            WeaveMessageKind::Reply => "reply",
            WeaveMessageKind::Control => "control",
        }
    }

    fn weave_action_message_payload(
        dst: &str,
        text: &str,
        action_id: &str,
        action_index: usize,
        kind: WeaveMessageKind,
        reply_to_action_id: Option<&str>,
    ) -> Value {
        let mut payload = serde_json::Map::new();
        payload.insert("type".to_string(), json!("message"));
        payload.insert("dst".to_string(), json!(dst));
        payload.insert(
            "kind".to_string(),
            json!(Self::weave_message_kind_label(kind)),
        );
        if let Some(reply_to_action_id) = reply_to_action_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            payload.insert("reply_to_action_id".to_string(), json!(reply_to_action_id));
        }
        payload.insert("text".to_string(), json!(text));
        payload.insert("action_id".to_string(), json!(action_id));
        payload.insert("action_index".to_string(), json!(action_index));
        Value::Object(payload)
    }

    fn weave_action_control_payload(
        dst: &str,
        tool: &WeaveTool,
        action_id: &str,
        action_index: usize,
    ) -> Value {
        let (command, _) = weave_control_action_parts(tool);
        let mut payload = serde_json::Map::new();
        payload.insert("type".to_string(), json!("control"));
        payload.insert("dst".to_string(), json!(dst));
        payload.insert("command".to_string(), json!(command));
        if let WeaveTool::Review {
            instructions: Some(instructions),
        } = tool
        {
            let instructions = instructions.trim();
            if !instructions.is_empty() {
                payload.insert("args".to_string(), json!(instructions));
            }
        }
        payload.insert("action_id".to_string(), json!(action_id));
        payload.insert("action_index".to_string(), json!(action_index));
        Value::Object(payload)
    }

    fn resolve_weave_outbound_metadata(
        &mut self,
        agent: &WeaveAgent,
        _tool: &WeaveTool,
    ) -> WeaveMessageMetadata {
        let state = self
            .weave_target_states
            .entry(agent.id.clone())
            .or_insert_with(|| {
                WeaveRelayTargetState::new(new_weave_conversation_id(), new_weave_task_id())
            });
        let conversation_id = state.context_id.clone();
        let task_id = state.task_id.clone();
        let conversation_owner = self.weave_agent_id.clone();
        WeaveMessageMetadata {
            conversation_id,
            conversation_owner,
            parent_message_id: None,
            task_id: Some(task_id),
        }
    }

    fn resolve_weave_interrupt_metadata(&self, agent: &WeaveAgent) -> Option<WeaveMessageMetadata> {
        if let Some(target) = self
            .active_weave_reply_target
            .as_ref()
            .filter(|target| target.agent_id == agent.id)
        {
            return Some(WeaveMessageMetadata {
                conversation_id: target.conversation_id.clone(),
                conversation_owner: target.conversation_owner.clone(),
                parent_message_id: None,
                task_id: target.task_id.clone(),
            });
        }
        let state = self.weave_target_states.get(&agent.id)?;
        Some(WeaveMessageMetadata {
            conversation_id: state.context_id.clone(),
            conversation_owner: self.weave_agent_id.clone(),
            parent_message_id: None,
            task_id: Some(state.task_id.clone()),
        })
    }

    fn resolve_weave_control_metadata(
        &mut self,
        agent: &WeaveAgent,
        tool: &WeaveTool,
    ) -> Option<WeaveMessageMetadata> {
        match tool {
            WeaveTool::Interrupt => self.resolve_weave_interrupt_metadata(agent),
            _ => Some(self.resolve_weave_outbound_metadata(agent, tool)),
        }
    }

    fn spawn_weave_control_actions(&mut self, actions: Vec<WeaveControlAction>) -> bool {
        self.spawn_weave_control_actions_with_context(actions, true)
    }

    fn spawn_weave_control_actions_with_context(
        &mut self,
        actions: Vec<WeaveControlAction>,
        clear_task_state: bool,
    ) -> bool {
        if actions.is_empty() {
            return false;
        }
        let cancel_context = self.active_weave_relay.clone();
        let should_clear_task = clear_task_state
            && cancel_context.as_ref().is_some_and(|context| {
                actions.iter().any(|action| {
                    matches!(action.tool, WeaveTool::Interrupt | WeaveTool::NewSession)
                        && action.targets.iter().any(|agent| context.allows(agent))
                })
            });
        if should_clear_task {
            self.active_weave_relay = None;
            self.active_weave_plan = None;
            self.pending_weave_plan = false;
            self.pending_weave_relay = None;
            self.weave_relay_buffer.clear();
            if let Some(context) = cancel_context.as_ref() {
                self.clear_pending_weave_actions_for_targets(context);
            }
            self.refresh_weave_session_label();
            self.maybe_send_next_queued_input();
        }
        let Some(connection) = self.weave_agent_connection.as_ref() else {
            self.add_to_history(history_cell::new_error_event(
                "Weave session isn't connected.".to_string(),
            ));
            return true;
        };
        self.app_event_tx.send(AppEvent::ScrollTranscriptToBottom);
        let sender = connection.sender();
        let mut prepared = Vec::new();
        for action in actions {
            let WeaveControlAction { targets, tool } = action;
            for agent in targets {
                let metadata = self.resolve_weave_control_metadata(&agent, &tool);
                prepared.push((agent, tool.clone(), metadata));
            }
        }
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            for (agent, tool, metadata) in prepared {
                let (_, label) = weave_control_action_parts(&tool);
                let name = agent.display_name();
                let group_id = new_weave_action_group_id();
                let action_id = new_weave_action_id();
                let payload = Self::action_submit_payload(
                    group_id.as_str(),
                    vec![Self::weave_action_control_payload(
                        agent.id.as_str(),
                        &tool,
                        action_id.as_str(),
                        0,
                    )],
                    metadata.as_ref(),
                );
                let result = sender.send_action_submit(payload).await;
                match result {
                    Ok(()) => {
                        app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                            history_cell::new_info_event(
                                format!("Weave control: sent {label} to {name}."),
                                None,
                            ),
                        )));
                    }
                    Err(err) => {
                        app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                            history_cell::new_error_event(format!(
                                "Failed to send Weave control to {name}: {err}"
                            )),
                        )));
                    }
                }
            }
        });
        true
    }

    fn is_weave_task_message(&self, text: &str) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.starts_with('!') {
            return false;
        }
        let Some(agents) = self.weave_agents.as_ref() else {
            return false;
        };
        let (_, cleaned_text) =
            parse_weave_control_actions(text, agents, Some(self.weave_agent_id.as_str()));
        let targets = find_weave_mentions(
            cleaned_text.as_str(),
            agents,
            Some(self.weave_agent_id.as_str()),
        );
        !targets.is_empty()
    }

    fn matches_active_weave_relay(
        &self,
        agent_id: &str,
        conversation_id: &str,
        task_id: Option<&str>,
    ) -> bool {
        let Some(active_relay) = self.active_weave_relay.as_ref() else {
            return false;
        };
        if !active_relay.allows_id(agent_id) {
            return false;
        }
        let Some(state) = self.weave_target_states.get(agent_id) else {
            return false;
        };
        if state.context_id != conversation_id {
            return false;
        }
        if let Some(task_id) = task_id
            && state.task_id != task_id
        {
            return false;
        }
        true
    }

    fn should_queue_weave_task(&self, user_message: &UserMessage) -> bool {
        let Some(active_relay) = self.active_weave_relay.as_ref() else {
            return false;
        };
        if active_relay.target_ids.is_empty() {
            return false;
        }
        match &user_message.source {
            UserMessageSource::Local => self.is_weave_task_message(&user_message.text),
            UserMessageSource::Weave(message) => {
                if message.conversation_owner == self.weave_agent_id
                    && self.matches_active_weave_relay(
                        message.reply_target.agent_id.as_str(),
                        message.conversation_id.as_str(),
                        message.task_id.as_deref(),
                    )
                {
                    return false;
                }
                true
            }
        }
    }

    fn take_weave_relay_output(&mut self, fallback: &str) -> Option<String> {
        self.pending_weave_relay.as_ref()?;
        if !self.weave_relay_buffer.trim().is_empty() {
            return Some(std::mem::take(&mut self.weave_relay_buffer));
        }
        Some(fallback.to_string())
    }

    fn handle_weave_relay_output(&mut self, output: &str) -> bool {
        let Some(targets) = self.pending_weave_relay.clone() else {
            return false;
        };
        self.weave_relay_buffer.clear();
        let Some(output) = parse_weave_relay_output(output) else {
            self.add_to_history(history_cell::new_error_event(
                "Invalid relay output; respond with a single-line JSON object.".to_string(),
            ));
            return true;
        };
        self.pending_weave_relay = None;

        match output {
            WeaveRelayOutput::TaskDone { summary } => {
                let summary = summary
                    .as_deref()
                    .map(str::trim)
                    .filter(|summary| !summary.is_empty())
                    .map(ToString::to_string);
                self.finish_weave_task(&targets, summary);
                true
            }
            WeaveRelayOutput::RelayActions { actions } => {
                if actions.is_empty() {
                    self.add_to_history(history_cell::new_error_event(
                        "Weave relay returned no actions; respond with relay_actions or task_done."
                            .to_string(),
                    ));
                    self.clear_weave_task_state(&targets);
                    true
                } else {
                    self.apply_weave_relay_actions(&targets, actions)
                }
            }
        }
    }

    fn apply_weave_relay_actions(
        &mut self,
        targets: &WeaveRelayTargets,
        actions: Vec<WeaveRelayAction>,
    ) -> bool {
        let Some(agents) = self.weave_agents.as_ref() else {
            self.add_to_history(history_cell::new_error_event(
                "Weave agents unavailable for relay.".to_string(),
            ));
            return true;
        };
        let agents = agents.clone();
        let mut handled = false;
        let mut outbound_messages: Vec<(String, Vec<(String, bool)>)> = Vec::new();
        let mut outgoing: HashMap<String, WeaveRelayGroupPayload> = HashMap::new();
        let mut action_index = 0usize;

        let mut next_action_index = || {
            let current = action_index;
            action_index += 1;
            current
        };

        for action in actions {
            match action {
                WeaveRelayAction::Control { dst, command } => {
                    let dst = dst.trim().to_string();
                    if dst.is_empty() {
                        self.add_to_history(history_cell::new_error_event(
                            "Weave relay target is empty.".to_string(),
                        ));
                        handled = true;
                        continue;
                    }
                    let tool = weave_tool_from_relay_command(command);
                    let resolved = resolve_weave_relay_targets(&agents, std::slice::from_ref(&dst))
                        .into_iter()
                        .filter(|agent| targets.allows(agent))
                        .collect::<Vec<_>>();
                    if resolved.is_empty() {
                        self.add_to_history(history_cell::new_error_event(
                            "Weave relay did not match any targets.".to_string(),
                        ));
                        handled = true;
                        continue;
                    }
                    for agent in resolved {
                        let action_id = new_weave_action_id();
                        let action_index = next_action_index();
                        let group = outgoing.entry(agent.id.clone()).or_insert_with(|| {
                            let (context_id, task_id) =
                                self.ensure_weave_target_state(agent.id.as_str());
                            WeaveRelayGroupPayload {
                                group_id: new_weave_action_group_id(),
                                actions: Vec::new(),
                                metadata: WeaveMessageMetadata {
                                    conversation_id: context_id,
                                    conversation_owner: targets.owner_id.clone(),
                                    parent_message_id: None,
                                    task_id: Some(task_id),
                                },
                            }
                        });
                        group.actions.push(Self::weave_action_control_payload(
                            agent.id.as_str(),
                            &tool,
                            action_id.as_str(),
                            action_index,
                        ));
                        self.register_pending_weave_action(
                            agent.id.as_str(),
                            action_id.as_str(),
                            group.group_id.as_str(),
                            false,
                        );
                    }
                    handled = true;
                }
                WeaveRelayAction::Message { dst, text, plan } => {
                    if let Some(plan) = plan {
                        self.apply_weave_plan(plan);
                    }
                    let dst = dst.trim().to_string();
                    if dst.is_empty() {
                        self.add_to_history(history_cell::new_error_event(
                            "Weave relay target is empty.".to_string(),
                        ));
                        handled = true;
                        continue;
                    }
                    let resolved = resolve_weave_relay_targets(&agents, std::slice::from_ref(&dst))
                        .into_iter()
                        .filter(|agent| targets.allows(agent))
                        .collect::<Vec<_>>();
                    if resolved.is_empty() {
                        self.add_to_history(history_cell::new_error_event(
                            "Weave relay did not match any targets.".to_string(),
                        ));
                        handled = true;
                        continue;
                    }
                    let cleaned_text = text.trim();
                    if cleaned_text.is_empty() {
                        self.add_to_history(history_cell::new_error_event(
                            "Weave relay message is empty.".to_string(),
                        ));
                        handled = true;
                        continue;
                    }
                    let (control_tools, cleaned_text) =
                        extract_weave_relay_control_tokens(cleaned_text);
                    for tool in control_tools {
                        for agent in &resolved {
                            let action_id = new_weave_action_id();
                            let action_index = next_action_index();
                            let group = outgoing.entry(agent.id.clone()).or_insert_with(|| {
                                let (context_id, task_id) =
                                    self.ensure_weave_target_state(agent.id.as_str());
                                WeaveRelayGroupPayload {
                                    group_id: new_weave_action_group_id(),
                                    actions: Vec::new(),
                                    metadata: WeaveMessageMetadata {
                                        conversation_id: context_id,
                                        conversation_owner: targets.owner_id.clone(),
                                        parent_message_id: None,
                                        task_id: Some(task_id),
                                    },
                                }
                            });
                            group.actions.push(Self::weave_action_control_payload(
                                agent.id.as_str(),
                                &tool,
                                action_id.as_str(),
                                action_index,
                            ));
                            self.register_pending_weave_action(
                                agent.id.as_str(),
                                action_id.as_str(),
                                group.group_id.as_str(),
                                false,
                            );
                        }
                    }
                    let cleaned_text = cleaned_text.trim();
                    if cleaned_text.is_empty() {
                        handled = true;
                        continue;
                    }
                    let mut successes = Vec::new();
                    for agent in &resolved {
                        let action_id = new_weave_action_id();
                        let action_index = next_action_index();
                        let reply_to_action_id = self
                            .weave_target_states
                            .get(&agent.id)
                            .and_then(|state| state.last_action_id.as_deref())
                            .map(ToString::to_string);
                        let kind = if reply_to_action_id.is_some() {
                            WeaveMessageKind::Reply
                        } else {
                            WeaveMessageKind::User
                        };
                        let group = outgoing.entry(agent.id.clone()).or_insert_with(|| {
                            let (context_id, task_id) =
                                self.ensure_weave_target_state(agent.id.as_str());
                            WeaveRelayGroupPayload {
                                group_id: new_weave_action_group_id(),
                                actions: Vec::new(),
                                metadata: WeaveMessageMetadata {
                                    conversation_id: context_id,
                                    conversation_owner: targets.owner_id.clone(),
                                    parent_message_id: None,
                                    task_id: Some(task_id),
                                },
                            }
                        });
                        group.actions.push(Self::weave_action_message_payload(
                            agent.id.as_str(),
                            cleaned_text,
                            action_id.as_str(),
                            action_index,
                            kind,
                            reply_to_action_id.as_deref(),
                        ));
                        self.register_pending_weave_action(
                            agent.id.as_str(),
                            action_id.as_str(),
                            group.group_id.as_str(),
                            true,
                        );
                        successes.push((agent.display_name(), targets.owner_id == agent.id));
                    }
                    outbound_messages.push((cleaned_text.to_string(), successes));
                    handled = true;
                }
            }
        }

        if outgoing.is_empty() {
            return handled;
        }
        if self.weave_agent_connection.is_none() {
            self.add_to_history(history_cell::new_error_event(
                "Weave session isn't connected.".to_string(),
            ));
            return true;
        };
        self.app_event_tx.send(AppEvent::ScrollTranscriptToBottom);
        let Some(connection) = self.weave_agent_connection.as_ref() else {
            return true;
        };
        let sender = connection.sender();
        let app_event_tx = self.app_event_tx.clone();
        let sender_name = self.weave_agent_name.clone();
        let sender_is_owner = targets.owner_id == self.weave_agent_id;
        let payloads = outgoing.into_values().collect::<Vec<_>>();
        tokio::spawn(async move {
            let mut all_ok = true;
            for group in payloads {
                let payload = Self::action_submit_payload(
                    group.group_id.as_str(),
                    group.actions,
                    Some(&group.metadata),
                );
                if let Err(err) = sender.send_action_submit(payload).await {
                    all_ok = false;
                    app_event_tx.send(AppEvent::WeaveActionSubmitFailed {
                        group_id: group.group_id.clone(),
                    });
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!(
                            "Failed to send Weave relay actions: {err}"
                        )),
                    )));
                }
            }
            if all_ok {
                for (message, successes) in outbound_messages {
                    if successes.is_empty() {
                        continue;
                    }
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_weave_outbound(
                            sender_name.clone(),
                            successes,
                            message,
                            sender_is_owner,
                        ),
                    )));
                }
            }
        });
        true
    }

    fn apply_weave_plan(&mut self, plan: WeaveRelayPlan) {
        if !self.pending_weave_plan {
            return;
        }
        let steps = plan
            .steps
            .into_iter()
            .map(|step| step.trim().to_string())
            .filter(|step| !step.is_empty())
            .collect::<Vec<_>>();
        if steps.is_empty() {
            return;
        }
        let plan = WeaveRelayPlan {
            steps,
            note: plan
                .note
                .as_deref()
                .map(str::trim)
                .filter(|note| !note.is_empty())
                .map(ToString::to_string),
        };
        let plan_items = plan
            .steps
            .iter()
            .enumerate()
            .map(|(idx, step)| PlanItemArg {
                step: step.clone(),
                status: if idx == 0 {
                    StepStatus::InProgress
                } else {
                    StepStatus::Pending
                },
            })
            .collect();
        let update = UpdatePlanArgs {
            explanation: plan.note.clone(),
            plan: plan_items,
        };
        self.add_to_history(history_cell::new_plan_update(update));
        self.active_weave_plan = Some(plan);
        self.pending_weave_plan = false;
    }

    fn finish_weave_task(&mut self, targets: &WeaveRelayTargets, summary: Option<String>) {
        if self
            .active_weave_relay
            .as_ref()
            .is_some_and(|task| task.matches(targets))
        {
            if let Some(plan) = self.active_weave_plan.take() {
                let plan_items = plan
                    .steps
                    .into_iter()
                    .map(|step| PlanItemArg {
                        step,
                        status: StepStatus::Completed,
                    })
                    .collect();
                let update = UpdatePlanArgs {
                    explanation: plan.note,
                    plan: plan_items,
                };
                self.add_to_history(history_cell::new_plan_update(update));
            }
            self.active_weave_relay = None;
            self.refresh_weave_session_label();
            self.clear_pending_weave_actions_for_targets(targets);
        }
        self.pending_weave_plan = false;
        if let Some(summary) = summary {
            self.add_agent_message(&summary);
        }
    }

    fn clear_weave_task_state(&mut self, targets: &WeaveRelayTargets) {
        if self
            .active_weave_relay
            .as_ref()
            .is_some_and(|task| task.matches(targets))
        {
            self.active_weave_relay = None;
            self.active_weave_plan = None;
            self.pending_weave_plan = false;
            self.refresh_weave_session_label();
            self.clear_pending_weave_actions_for_targets(targets);
        }
    }

    fn interrupt_weave_task(
        &mut self,
        conversation_id: &str,
        conversation_owner: &str,
        sender_id: &str,
    ) -> bool {
        let Some(active_relay) = self.active_weave_relay.clone() else {
            return false;
        };
        let matches_sender = conversation_owner == sender_id && active_relay.allows_id(sender_id);
        let matches_task = conversation_owner == active_relay.owner_id
            && active_relay.target_ids.iter().any(|target_id| {
                self.weave_target_states
                    .get(target_id)
                    .is_some_and(|state| state.context_id == conversation_id)
            });
        if !matches_task && !matches_sender {
            return false;
        }
        self.active_weave_relay = None;
        self.active_weave_plan = None;
        self.pending_weave_plan = false;
        self.pending_weave_relay = None;
        self.weave_relay_buffer.clear();
        self.clear_pending_weave_actions_for_targets(&active_relay);
        self.refresh_weave_session_label();
        true
    }

    fn clear_weave_reply_state(&mut self, conversation_id: &str, conversation_owner: &str) {
        if self
            .active_weave_reply_target
            .as_ref()
            .is_some_and(|target| {
                target.conversation_id == conversation_id
                    && target.conversation_owner == conversation_owner
            })
        {
            self.active_weave_reply_target = None;
        }
        let mut changed = false;
        self.queued_user_messages.retain(|message| {
            let keep = match &message.source {
                UserMessageSource::Weave(weave) => {
                    weave.conversation_id != conversation_id
                        || weave.conversation_owner != conversation_owner
                }
                UserMessageSource::Local => true,
            };
            if !keep {
                changed = true;
            }
            keep
        });
        if changed {
            self.refresh_queued_user_messages();
        }
    }

    fn clear_weave_reply_state_for_sender(&mut self, sender_id: &str) -> bool {
        let mut changed = false;
        if self
            .active_weave_reply_target
            .as_ref()
            .is_some_and(|target| {
                target.conversation_owner == sender_id || target.agent_id == sender_id
            })
        {
            self.active_weave_reply_target = None;
            changed = true;
        }
        self.queued_user_messages.retain(|message| {
            let keep = match &message.source {
                UserMessageSource::Weave(weave) => {
                    weave.conversation_owner != sender_id
                        && weave.reply_target.agent_id != sender_id
                }
                UserMessageSource::Local => true,
            };
            if !keep {
                changed = true;
            }
            keep
        });
        if changed {
            self.refresh_queued_user_messages();
        }
        changed
    }

    fn cancel_active_weave_task_on_interrupt(&mut self, targets: WeaveRelayTargets) {
        let conversation_owner = targets.owner_id.clone();
        self.active_weave_relay = None;
        self.active_weave_plan = None;
        self.pending_weave_plan = false;
        self.pending_weave_relay = None;
        self.weave_relay_buffer.clear();
        self.clear_pending_weave_actions_for_targets(&targets);
        self.refresh_weave_session_label();
        let context_ids = targets
            .target_ids
            .iter()
            .filter_map(|target_id| {
                self.weave_target_states
                    .get(target_id)
                    .map(|state| state.context_id.clone())
            })
            .collect::<Vec<_>>();
        for context_id in context_ids {
            self.clear_weave_reply_state(&context_id, &conversation_owner);
        }

        let Some(session_id) = self.selected_weave_session_id.clone() else {
            return;
        };
        let Some(agents) = self.weave_agents.as_ref() else {
            return;
        };
        let targets = agents
            .iter()
            .filter(|agent| targets.allows(agent))
            .cloned()
            .collect::<Vec<_>>();
        for agent in targets {
            let Some(state) = self.weave_target_states.get(&agent.id) else {
                continue;
            };
            let reply_target = WeaveReplyTarget {
                session_id: session_id.clone(),
                agent_id: agent.id.clone(),
                display_name: agent.display_name(),
                conversation_id: state.context_id.clone(),
                conversation_owner: conversation_owner.clone(),
                parent_message_id: None,
                task_id: Some(state.task_id.clone()),
                action_group_id: None,
                action_id: None,
                action_index: None,
            };
            self.send_weave_interrupt(reply_target);
        }
    }

    fn should_interrupt_on_interrupt(
        &self,
        conversation_id: &str,
        conversation_owner: &str,
        sender_id: &str,
    ) -> bool {
        if conversation_owner == sender_id
            && self
                .active_weave_control_context
                .as_ref()
                .is_some_and(|context| context.sender_id == sender_id)
        {
            return true;
        }
        if !(self.bottom_pane.is_task_running() || self.is_review_mode) {
            return false;
        }
        if conversation_owner != sender_id {
            return false;
        }
        self.active_weave_reply_target
            .as_ref()
            .is_some_and(|target| {
                target.conversation_id == conversation_id
                    && target.conversation_owner == conversation_owner
            })
    }

    fn should_interrupt_for_sender_interrupt(&self, sender_id: &str) -> bool {
        if self
            .active_weave_control_context
            .as_ref()
            .is_some_and(|context| context.sender_id == sender_id)
        {
            return true;
        }
        if !(self.bottom_pane.is_task_running() || self.is_review_mode) {
            return false;
        }
        if self
            .active_weave_reply_target
            .as_ref()
            .is_some_and(|target| {
                target.conversation_owner == sender_id || target.agent_id == sender_id
            })
        {
            return true;
        }
        false
    }

    fn queue_user_message(&mut self, user_message: UserMessage) {
        if self.should_queue_weave_task(&user_message) {
            self.queued_user_messages.push_back(user_message);
            self.refresh_queued_user_messages();
            return;
        }
        if let UserMessageSource::Weave(message) = &user_message.source {
            if self.should_defer_weave_reply(message) {
                self.queued_user_messages.push_back(user_message);
                self.refresh_queued_user_messages();
                return;
            }
            self.queued_user_messages.push_back(user_message);
            self.refresh_queued_user_messages();
            self.maybe_send_next_queued_input();
            return;
        }
        if self.bottom_pane.is_task_running() {
            self.queued_user_messages.push_back(user_message);
            self.refresh_queued_user_messages();
        } else {
            self.submit_user_message(user_message);
        }
    }

    fn should_defer_weave_reply(&self, message: &WeaveUserMessage) -> bool {
        if message.relay_targets.is_none() || message.conversation_owner != self.weave_agent_id {
            return false;
        }
        let agent_id = message.reply_target.agent_id.as_str();
        self.weave_target_states
            .get(agent_id)
            .is_some_and(WeaveRelayTargetState::has_pending_reply)
    }

    fn submit_user_message(&mut self, user_message: UserMessage) {
        let UserMessage {
            text,
            image_paths,
            source,
        } = user_message;
        let mut raw_text = text;
        let (is_local, reply_target, auto_reply, relay_targets, mut input_text) = match source {
            UserMessageSource::Local => (true, None, false, None, raw_text.clone()),
            UserMessageSource::Weave(message) => {
                let WeaveUserMessage {
                    reply_target,
                    auto_reply,
                    relay_targets,
                    conversation_id,
                    conversation_owner,
                    kind,
                    task_id,
                } = *message;
                let task_targets = relay_targets
                    .as_ref()
                    .map(|targets| weave_task_target_labels(targets, self.weave_agents.as_deref()));
                let pending_replies = relay_targets.as_ref().map(|_| {
                    weave_pending_reply_labels(
                        &self.weave_target_states,
                        self.weave_agents.as_deref(),
                    )
                });
                let is_owner = conversation_owner == self.weave_agent_id;
                let input_text = format_weave_prompt(WeavePromptContext {
                    self_name: &self.weave_agent_name,
                    sender_name: &reply_target.display_name,
                    agents: self.weave_agents.as_deref(),
                    message: &raw_text,
                    is_owner,
                    task_targets: task_targets.as_deref(),
                    pending_replies: pending_replies.as_deref(),
                    conversation_id: &conversation_id,
                    conversation_owner: &conversation_owner,
                    task_id: task_id.as_deref(),
                    kind,
                });
                (
                    false,
                    Some(reply_target),
                    auto_reply,
                    relay_targets,
                    input_text,
                )
            }
        };
        if is_local && let Some(agents) = self.weave_agents.as_ref() {
            let (control_actions, cleaned_text) = parse_weave_control_actions(
                raw_text.as_str(),
                agents,
                Some(self.weave_agent_id.as_str()),
            );
            if !control_actions.is_empty() {
                self.spawn_weave_control_actions(control_actions);
                raw_text = cleaned_text;
                input_text = raw_text.clone();
            }
        }
        if raw_text.is_empty() && image_paths.is_empty() {
            self.active_weave_reply_target = None;
            return;
        }

        self.pending_weave_relay = None;
        self.weave_relay_buffer.clear();
        if is_local {
            if let Some(agents) = self.weave_agents.as_ref() {
                let targets =
                    find_weave_mentions(&raw_text, agents, Some(self.weave_agent_id.as_str()));
                if !targets.is_empty() {
                    let relay_targets =
                        WeaveRelayTargets::new(self.weave_agent_id.clone(), &targets);
                    self.pending_weave_relay = Some(relay_targets.clone());
                    self.active_weave_relay = Some(relay_targets.clone());
                    self.reset_weave_relay_targets(&relay_targets);
                    self.active_weave_plan = None;
                    self.refresh_weave_session_label();
                    let wants_plan = should_request_weave_plan(&raw_text);
                    self.pending_weave_plan = wants_plan;
                    let relay_prompt = build_weave_relay_prompt(&targets, wants_plan);
                    input_text = format!("{raw_text}\n\n{relay_prompt}");
                } else {
                    if let Some(active) = self.active_weave_relay.take() {
                        self.clear_pending_weave_actions_for_targets(&active);
                    }
                    self.active_weave_plan = None;
                    self.pending_weave_plan = false;
                    self.refresh_weave_session_label();
                }
            } else if self.is_weave_task_message(&raw_text) {
                self.add_to_history(history_cell::new_error_event(
                    "Weave session isn't connected.".to_string(),
                ));
                return;
            }
        } else if let Some(relay_targets) = relay_targets.as_ref() {
            self.pending_weave_relay = Some(relay_targets.clone());
        }

        // Special-case: "!cmd" executes a local shell command instead of sending to the model.
        if is_local && let Some(stripped) = raw_text.strip_prefix('!') {
            let cmd = stripped.trim();
            if cmd.is_empty() {
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_info_event(
                        USER_SHELL_COMMAND_HELP_TITLE.to_string(),
                        Some(USER_SHELL_COMMAND_HELP_HINT.to_string()),
                    ),
                )));
                self.active_weave_reply_target = None;
                return;
            }
            self.submit_op(Op::RunUserShellCommand {
                command: cmd.to_string(),
            });
            self.active_weave_reply_target = None;
            return;
        }

        self.active_weave_reply_target = if auto_reply && relay_targets.is_none() {
            reply_target
        } else {
            None
        };

        let mut items: Vec<UserInput> = Vec::new();

        if !input_text.is_empty() {
            items.push(UserInput::Text { text: input_text });
        }

        for path in image_paths {
            items.push(UserInput::LocalImage { path });
        }

        if let Some(skills) = self.bottom_pane.skills() {
            let skill_mentions = find_skill_mentions(&raw_text, skills);
            for skill in skill_mentions {
                items.push(UserInput::Skill {
                    name: skill.name.clone(),
                    path: skill.path.clone(),
                });
            }
        }

        self.codex_op_tx
            .send(Op::UserInput {
                items,
                final_output_json_schema: None,
            })
            .unwrap_or_else(|e| {
                tracing::error!("failed to send message: {e}");
            });

        // Persist the text to cross-session message history.
        if is_local && !raw_text.is_empty() {
            self.codex_op_tx
                .send(Op::AddToHistory {
                    text: raw_text.clone(),
                })
                .unwrap_or_else(|e| {
                    tracing::error!("failed to send AddHistory op: {e}");
                });
        }

        // Only show the text portion in conversation history.
        if is_local && !raw_text.is_empty() {
            self.add_to_history(history_cell::new_user_prompt(raw_text));
        }
        self.needs_final_message_separator = false;
    }

    /// Replay a subset of initial events into the UI to seed the transcript when
    /// resuming an existing session. This approximates the live event flow and
    /// is intentionally conservative: only safe-to-replay items are rendered to
    /// avoid triggering side effects. Event ids are passed as `None` to
    /// distinguish replayed events from live ones.
    fn replay_initial_messages(&mut self, events: Vec<EventMsg>) {
        for msg in events {
            if matches!(msg, EventMsg::SessionConfigured(_)) {
                continue;
            }
            // `id: None` indicates a synthetic/fake id coming from replay.
            self.dispatch_event_msg(None, msg, true);
        }
    }

    pub(crate) fn handle_codex_event(&mut self, event: Event) {
        let Event { id, msg } = event;
        self.dispatch_event_msg(Some(id), msg, false);
    }

    /// Dispatch a protocol `EventMsg` to the appropriate handler.
    ///
    /// `id` is `Some` for live events and `None` for replayed events from
    /// `replay_initial_messages()`. Callers should treat `None` as a "fake" id
    /// that must not be used to correlate follow-up actions.
    fn dispatch_event_msg(&mut self, id: Option<String>, msg: EventMsg, from_replay: bool) {
        let is_stream_error = matches!(&msg, EventMsg::StreamError(_));
        if !is_stream_error {
            self.restore_retry_status_header_if_present();
        }

        match msg {
            EventMsg::AgentMessageDelta(_)
            | EventMsg::AgentReasoningDelta(_)
            | EventMsg::TerminalInteraction(_)
            | EventMsg::ExecCommandOutputDelta(_) => {}
            _ => {
                tracing::trace!("handle_codex_event: {:?}", msg);
            }
        }

        match msg {
            EventMsg::SessionConfigured(e) => self.on_session_configured(e),
            EventMsg::AgentMessage(AgentMessageEvent { message }) => self.on_agent_message(message),
            EventMsg::AgentMessageDelta(AgentMessageDeltaEvent { delta }) => {
                self.on_agent_message_delta(delta)
            }
            EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent { delta })
            | EventMsg::AgentReasoningRawContentDelta(AgentReasoningRawContentDeltaEvent {
                delta,
            }) => self.on_agent_reasoning_delta(delta),
            EventMsg::AgentReasoning(AgentReasoningEvent { .. }) => self.on_agent_reasoning_final(),
            EventMsg::AgentReasoningRawContent(AgentReasoningRawContentEvent { text }) => {
                self.on_agent_reasoning_delta(text);
                self.on_agent_reasoning_final();
            }
            EventMsg::AgentReasoningSectionBreak(_) => self.on_reasoning_section_break(),
            EventMsg::TurnStarted(_) => self.on_task_started(),
            EventMsg::TurnComplete(TurnCompleteEvent { last_agent_message }) => {
                self.on_task_complete(last_agent_message)
            }
            EventMsg::TokenCount(ev) => {
                self.set_token_info(ev.info);
                self.on_rate_limit_snapshot(ev.rate_limits);
            }
            EventMsg::Warning(WarningEvent { message }) => self.on_warning(message),
            EventMsg::Error(ErrorEvent { message, .. }) => self.on_error(message),
            EventMsg::McpStartupUpdate(ev) => self.on_mcp_startup_update(ev),
            EventMsg::McpStartupComplete(ev) => self.on_mcp_startup_complete(ev),
            EventMsg::TurnAborted(ev) => match ev.reason {
                TurnAbortReason::Interrupted => {
                    self.on_interrupted_turn(ev.reason);
                }
                TurnAbortReason::Replaced => {
                    self.on_error("Turn aborted: replaced by a new task".to_owned())
                }
                TurnAbortReason::ReviewEnded => {
                    self.on_interrupted_turn(ev.reason);
                }
            },
            EventMsg::PlanUpdate(update) => self.on_plan_update(update),
            EventMsg::ExecApprovalRequest(ev) => {
                // For replayed events, synthesize an empty id (these should not occur).
                self.on_exec_approval_request(id.unwrap_or_default(), ev)
            }
            EventMsg::ApplyPatchApprovalRequest(ev) => {
                self.on_apply_patch_approval_request(id.unwrap_or_default(), ev)
            }
            EventMsg::ElicitationRequest(ev) => {
                self.on_elicitation_request(ev);
            }
            EventMsg::ExecCommandBegin(ev) => self.on_exec_command_begin(ev),
            EventMsg::TerminalInteraction(delta) => self.on_terminal_interaction(delta),
            EventMsg::ExecCommandOutputDelta(delta) => self.on_exec_command_output_delta(delta),
            EventMsg::PatchApplyBegin(ev) => self.on_patch_apply_begin(ev),
            EventMsg::PatchApplyEnd(ev) => self.on_patch_apply_end(ev),
            EventMsg::ExecCommandEnd(ev) => self.on_exec_command_end(ev),
            EventMsg::ViewImageToolCall(ev) => self.on_view_image_tool_call(ev),
            EventMsg::McpToolCallBegin(ev) => self.on_mcp_tool_call_begin(ev),
            EventMsg::McpToolCallEnd(ev) => self.on_mcp_tool_call_end(ev),
            EventMsg::WebSearchBegin(ev) => self.on_web_search_begin(ev),
            EventMsg::WebSearchEnd(ev) => self.on_web_search_end(ev),
            EventMsg::GetHistoryEntryResponse(ev) => self.on_get_history_entry_response(ev),
            EventMsg::McpListToolsResponse(ev) => self.on_list_mcp_tools(ev),
            EventMsg::ListCustomPromptsResponse(ev) => self.on_list_custom_prompts(ev),
            EventMsg::ListSkillsResponse(ev) => self.on_list_skills(ev),
            EventMsg::SkillsUpdateAvailable => {
                self.submit_op(Op::ListSkills {
                    cwds: Vec::new(),
                    force_reload: true,
                });
            }
            EventMsg::ShutdownComplete => self.on_shutdown_complete(),
            EventMsg::TurnDiff(TurnDiffEvent { unified_diff }) => self.on_turn_diff(unified_diff),
            EventMsg::DeprecationNotice(ev) => self.on_deprecation_notice(ev),
            EventMsg::BackgroundEvent(BackgroundEventEvent { message }) => {
                self.on_background_event(message)
            }
            EventMsg::UndoStarted(ev) => self.on_undo_started(ev),
            EventMsg::UndoCompleted(ev) => self.on_undo_completed(ev),
            EventMsg::StreamError(StreamErrorEvent {
                message,
                additional_details,
                ..
            }) => self.on_stream_error(message, additional_details),
            EventMsg::UserMessage(ev) => {
                if from_replay {
                    self.on_user_message_event(ev);
                }
            }
            EventMsg::EnteredReviewMode(review_request) => {
                self.on_entered_review_mode(review_request)
            }
            EventMsg::ExitedReviewMode(review) => self.on_exited_review_mode(review),
            EventMsg::ContextCompacted(_) => self.on_agent_message("Context compacted".to_owned()),
            EventMsg::ThreadRolledBack(_) => {}
            EventMsg::RawResponseItem(_)
            | EventMsg::ItemStarted(_)
            | EventMsg::ItemCompleted(_)
            | EventMsg::AgentMessageContentDelta(_)
            | EventMsg::ReasoningContentDelta(_)
            | EventMsg::ReasoningRawContentDelta(_) => {}
        }
    }

    fn on_entered_review_mode(&mut self, review: ReviewRequest) {
        // Enter review mode and emit a concise banner
        if self.pre_review_token_info.is_none() {
            self.pre_review_token_info = Some(self.token_info.clone());
        }
        self.is_review_mode = true;
        let hint = review
            .user_facing_hint
            .unwrap_or_else(|| codex_core::review_prompts::user_facing_hint(&review.target));
        let banner = format!(">> Code review started: {hint} <<");
        self.add_to_history(history_cell::new_review_status_line(banner));
        self.request_redraw();
    }

    fn on_exited_review_mode(&mut self, review: ExitedReviewModeEvent) {
        // Leave review mode; if output is present, flush pending stream + show results.
        if let Some(output) = review.review_output {
            self.flush_answer_stream_with_separator();
            self.flush_interrupt_queue();
            self.flush_active_cell();

            if output.findings.is_empty() {
                let explanation = output.overall_explanation.trim().to_string();
                if explanation.is_empty() {
                    tracing::error!("Reviewer failed to output a response.");
                    self.add_to_history(history_cell::new_error_event(
                        "Reviewer failed to output a response.".to_owned(),
                    ));
                } else {
                    // Show explanation when there are no structured findings.
                    let mut rendered: Vec<ratatui::text::Line<'static>> = vec!["".into()];
                    append_markdown(&explanation, None, &mut rendered);
                    let body_cell = AgentMessageCell::new(rendered, false);
                    self.app_event_tx
                        .send(AppEvent::InsertHistoryCell(Box::new(body_cell)));
                }
            }
            // Final message is rendered as part of the AgentMessage.
        }

        self.is_review_mode = false;
        self.restore_pre_review_token_info();
        // Append a finishing banner at the end of this turn.
        self.add_to_history(history_cell::new_review_status_line(
            "<< Code review finished >>".to_string(),
        ));
        self.request_redraw();
    }

    fn on_user_message_event(&mut self, event: UserMessageEvent) {
        let message = event.message.trim();
        if !message.is_empty() {
            self.add_to_history(history_cell::new_user_prompt(message.to_string()));
        }
    }

    fn request_exit(&self) {
        self.app_event_tx.send(AppEvent::ExitRequest);
    }

    pub(crate) fn prepare_for_exit(&mut self) {
        self.disconnect_weave_agent();
    }

    fn request_redraw(&mut self) {
        self.frame_requester.schedule_frame();
    }

    fn notify(&mut self, notification: Notification) {
        if !notification.allowed_for(&self.config.tui_notifications) {
            return;
        }
        self.pending_notification = Some(notification);
        self.request_redraw();
    }

    pub(crate) fn maybe_post_pending_notification(&mut self, tui: &mut crate::tui::Tui) {
        if let Some(notif) = self.pending_notification.take() {
            tui.notify(notif.display());
        }
    }

    /// Mark the active cell as failed (✗) and flush it into history.
    fn finalize_active_cell_as_failed(&mut self) {
        if let Some(mut cell) = self.active_cell.take() {
            // Insert finalized cell into history and keep grouping consistent.
            if let Some(exec) = cell.as_any_mut().downcast_mut::<ExecCell>() {
                exec.mark_failed();
            } else if let Some(tool) = cell.as_any_mut().downcast_mut::<McpToolCallCell>() {
                tool.mark_failed();
            }
            self.add_boxed_history(cell);
        }
    }

    // If idle and there are queued inputs, submit exactly one to start the next turn.
    fn maybe_send_next_queued_input(&mut self) {
        if self.bottom_pane.is_task_running() {
            return;
        }
        if self
            .queued_user_messages
            .front()
            .is_some_and(|message| self.should_queue_weave_task(message))
        {
            return;
        }
        if self.queued_user_messages.front().is_some_and(|message| {
            matches!(
                &message.source,
                UserMessageSource::Weave(weave) if self.should_defer_weave_reply(weave)
            )
        }) {
            return;
        }
        if let Some(user_message) = self.pop_batched_weave_replies() {
            self.submit_user_message(user_message);
            self.refresh_queued_user_messages();
            return;
        }
        if let Some(user_message) = self.queued_user_messages.pop_front() {
            self.submit_user_message(user_message);
        }
        // Update the list to reflect the remaining queued messages (if any).
        self.refresh_queued_user_messages();
    }

    fn pop_batched_weave_replies(&mut self) -> Option<UserMessage> {
        let first = self.queued_user_messages.front()?;
        let second = self.queued_user_messages.get(1)?;
        let first_weave = match &first.source {
            UserMessageSource::Weave(weave) => weave.clone(),
            UserMessageSource::Local => return None,
        };
        let second_weave = match &second.source {
            UserMessageSource::Weave(weave) => weave.clone(),
            UserMessageSource::Local => return None,
        };
        if !self.should_batch_weave_replies(&first_weave, &second_weave) {
            return None;
        }
        let relay_targets = first_weave.relay_targets.clone();
        let kind = first_weave.kind;
        let conversation_id = first_weave.conversation_id.clone();
        let conversation_owner = first_weave.conversation_owner.clone();
        let task_id = first_weave.task_id.clone();
        let mut replies = Vec::new();
        while let Some(front) = self.queued_user_messages.front() {
            let UserMessageSource::Weave(weave) = &front.source else {
                break;
            };
            if !self.should_batch_weave_reply_item(
                weave,
                conversation_id.as_str(),
                conversation_owner.as_str(),
                &task_id,
            ) {
                break;
            }
            let front = self.queued_user_messages.pop_front()?;
            let UserMessageSource::Weave(weave) = front.source else {
                continue;
            };
            replies.push((weave.reply_target.display_name.clone(), front.text));
        }
        let (display_name, text) = if replies.len() == 1 {
            let (sender, text) = replies
                .pop()
                .unwrap_or_else(|| ("unknown".to_string(), String::new()));
            (sender, text)
        } else {
            (
                "multiple agents".to_string(),
                format_batched_weave_replies(replies),
            )
        };
        let reply_target = WeaveReplyTarget {
            session_id: first_weave.reply_target.session_id.clone(),
            agent_id: first_weave.reply_target.agent_id.clone(),
            display_name,
            conversation_id: conversation_id.clone(),
            conversation_owner: conversation_owner.clone(),
            parent_message_id: None,
            task_id: task_id.clone(),
            action_group_id: None,
            action_id: None,
            action_index: None,
        };
        let weave = WeaveUserMessage {
            reply_target,
            auto_reply: false,
            relay_targets,
            conversation_id,
            conversation_owner,
            task_id,
            kind,
        };
        Some(UserMessage::from_weave(text, weave))
    }

    fn should_batch_weave_replies(
        &self,
        first: &WeaveUserMessage,
        second: &WeaveUserMessage,
    ) -> bool {
        self.should_batch_weave_reply_item(
            first,
            first.conversation_id.as_str(),
            first.conversation_owner.as_str(),
            &first.task_id,
        ) && self.should_batch_weave_reply_item(
            second,
            first.conversation_id.as_str(),
            first.conversation_owner.as_str(),
            &first.task_id,
        )
    }

    fn should_batch_weave_reply_item(
        &self,
        weave: &WeaveUserMessage,
        conversation_id: &str,
        conversation_owner: &str,
        task_id: &Option<String>,
    ) -> bool {
        weave.relay_targets.is_some()
            && weave.conversation_owner == self.weave_agent_id
            && weave.conversation_id == conversation_id
            && weave.conversation_owner == conversation_owner
            && &weave.task_id == task_id
    }

    /// Rebuild and update the queued user messages from the current queue.
    fn refresh_queued_user_messages(&mut self) {
        let messages: Vec<String> = self
            .queued_user_messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        self.bottom_pane.set_queued_user_messages(messages);
    }

    pub(crate) fn add_diff_in_progress(&mut self) {
        self.request_redraw();
    }

    pub(crate) fn on_diff_complete(&mut self) {
        self.request_redraw();
    }

    pub(crate) fn add_status_output(&mut self) {
        let default_usage = TokenUsage::default();
        let token_info = self.token_info.as_ref();
        let total_usage = token_info
            .map(|ti| &ti.total_token_usage)
            .unwrap_or(&default_usage);
        self.add_to_history(crate::status::new_status_output(
            &self.config,
            self.auth_manager.as_ref(),
            token_info,
            total_usage,
            &self.thread_id,
            self.rate_limit_snapshot.as_ref(),
            self.plan_type,
            Local::now(),
            &self.model,
        ));
    }

    pub(crate) fn add_ps_output(&mut self) {
        let processes = self
            .unified_exec_processes
            .iter()
            .map(|process| process.command_display.clone())
            .collect();
        self.add_to_history(history_cell::new_unified_exec_processes_output(processes));
    }

    fn stop_rate_limit_poller(&mut self) {
        if let Some(handle) = self.rate_limit_poller.take() {
            handle.abort();
        }
    }

    fn prefetch_rate_limits(&mut self) {
        self.stop_rate_limit_poller();

        if self.auth_manager.auth_cached().map(|auth| auth.mode) != Some(AuthMode::ChatGPT) {
            return;
        }

        let base_url = self.config.chatgpt_base_url.clone();
        let app_event_tx = self.app_event_tx.clone();
        let auth_manager = Arc::clone(&self.auth_manager);

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));

            loop {
                if let Some(auth) = auth_manager.auth().await
                    && auth.mode == AuthMode::ChatGPT
                    && let Some(snapshot) = fetch_rate_limits(base_url.clone(), auth).await
                {
                    app_event_tx.send(AppEvent::RateLimitSnapshotFetched(snapshot));
                }
                interval.tick().await;
            }
        });

        self.rate_limit_poller = Some(handle);
    }

    fn lower_cost_preset(&self) -> Option<ModelPreset> {
        let models = self.models_manager.try_list_models(&self.config).ok()?;
        models
            .iter()
            .find(|preset| preset.show_in_picker && preset.model == NUDGE_MODEL_SLUG)
            .cloned()
    }

    fn rate_limit_switch_prompt_hidden(&self) -> bool {
        self.config
            .notices
            .hide_rate_limit_model_nudge
            .unwrap_or(false)
    }

    fn maybe_show_pending_rate_limit_prompt(&mut self) {
        if self.rate_limit_switch_prompt_hidden() {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
            return;
        }
        if !matches!(
            self.rate_limit_switch_prompt,
            RateLimitSwitchPromptState::Pending
        ) {
            return;
        }
        if let Some(preset) = self.lower_cost_preset() {
            self.open_rate_limit_switch_prompt(preset);
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Shown;
        } else {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
        }
    }

    fn open_rate_limit_switch_prompt(&mut self, preset: ModelPreset) {
        let switch_model = preset.model.to_string();
        let display_name = preset.display_name.to_string();
        let default_effort: ReasoningEffortConfig = preset.default_reasoning_effort;

        let switch_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::CodexOp(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: None,
                sandbox_policy: None,
                model: Some(switch_model.clone()),
                effort: Some(Some(default_effort)),
                summary: None,
            }));
            tx.send(AppEvent::UpdateModel(switch_model.clone()));
            tx.send(AppEvent::UpdateReasoningEffort(Some(default_effort)));
        })];

        let keep_actions: Vec<SelectionAction> = Vec::new();
        let never_actions: Vec<SelectionAction> = vec![Box::new(|tx| {
            tx.send(AppEvent::UpdateRateLimitSwitchPromptHidden(true));
            tx.send(AppEvent::PersistRateLimitSwitchPromptHidden);
        })];
        let description = if preset.description.is_empty() {
            Some("Uses fewer credits for upcoming turns.".to_string())
        } else {
            Some(preset.description)
        };

        let items = vec![
            SelectionItem {
                name: format!("Switch to {display_name}"),
                description,
                selected_description: None,
                is_current: false,
                actions: switch_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Keep current model".to_string(),
                description: None,
                selected_description: None,
                is_current: false,
                actions: keep_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Keep current model (never show again)".to_string(),
                description: Some(
                    "Hide future rate limit reminders about switching models.".to_string(),
                ),
                selected_description: None,
                is_current: false,
                actions: never_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Approaching rate limits".to_string()),
            subtitle: Some(format!("Switch to {display_name} for lower credit usage?")),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    /// Open a popup to choose a quick auto model. Selecting "All models"
    /// opens the full picker with every available preset.
    pub(crate) fn open_model_popup(&mut self) {
        let presets: Vec<ModelPreset> = match self.models_manager.try_list_models(&self.config) {
            Ok(models) => models,
            Err(_) => {
                self.add_info_message(
                    "Models are being updated; please try /model again in a moment.".to_string(),
                    None,
                );
                return;
            }
        };
        self.open_model_popup_with_presets(presets);
    }

    fn model_menu_header(&self, title: &str, subtitle: &str) -> Box<dyn Renderable> {
        let title = title.to_string();
        let subtitle = subtitle.to_string();
        let mut header = ColumnRenderable::new();
        header.push(Line::from(title.bold()));
        header.push(Line::from(subtitle.dim()));
        if let Some(warning) = self.model_menu_warning_line() {
            header.push(warning);
        }
        Box::new(header)
    }

    fn model_menu_warning_line(&self) -> Option<Line<'static>> {
        let base_url = self.custom_openai_base_url()?;
        let warning = format!(
            "Warning: OPENAI_BASE_URL is set to {base_url}. Selecting models may not be supported or work properly."
        );
        Some(Line::from(warning.red()))
    }

    fn custom_openai_base_url(&self) -> Option<String> {
        if !self.config.model_provider.is_openai() {
            return None;
        }

        let base_url = self.config.model_provider.base_url.as_ref()?;
        let trimmed = base_url.trim();
        if trimmed.is_empty() {
            return None;
        }

        let normalized = trimmed.trim_end_matches('/');
        if normalized == DEFAULT_OPENAI_BASE_URL {
            return None;
        }

        Some(trimmed.to_string())
    }

    pub(crate) fn open_model_popup_with_presets(&mut self, presets: Vec<ModelPreset>) {
        let presets: Vec<ModelPreset> = presets
            .into_iter()
            .filter(|preset| preset.show_in_picker)
            .collect();

        let current_label = presets
            .iter()
            .find(|preset| preset.model == self.model)
            .map(|preset| preset.display_name.to_string())
            .unwrap_or_else(|| self.model.clone());

        let (mut auto_presets, other_presets): (Vec<ModelPreset>, Vec<ModelPreset>) = presets
            .into_iter()
            .partition(|preset| Self::is_auto_model(&preset.model));

        if auto_presets.is_empty() {
            self.open_all_models_popup(other_presets);
            return;
        }

        auto_presets.sort_by_key(|preset| Self::auto_model_order(&preset.model));

        let mut items: Vec<SelectionItem> = auto_presets
            .into_iter()
            .map(|preset| {
                let description =
                    (!preset.description.is_empty()).then_some(preset.description.clone());
                let model = preset.model.clone();
                let actions = Self::model_selection_actions(
                    model.clone(),
                    Some(preset.default_reasoning_effort),
                );
                SelectionItem {
                    name: preset.display_name.clone(),
                    description,
                    is_current: model == self.model,
                    is_default: preset.is_default,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        if !other_presets.is_empty() {
            let all_models = other_presets;
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenAllModelsPopup {
                    models: all_models.clone(),
                });
            })];

            let is_current = !items.iter().any(|item| item.is_current);
            let description = Some(format!(
                "Choose a specific model and reasoning level (current: {current_label})"
            ));

            items.push(SelectionItem {
                name: "All models".to_string(),
                description,
                is_current,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        let header = self.model_menu_header(
            "Select Model",
            "Pick a quick auto mode or browse all models.",
        );
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header,
            ..Default::default()
        });
    }

    fn is_auto_model(model: &str) -> bool {
        model.starts_with("codex-auto-")
    }

    fn auto_model_order(model: &str) -> usize {
        match model {
            "codex-auto-fast" => 0,
            "codex-auto-balanced" => 1,
            "codex-auto-thorough" => 2,
            _ => 3,
        }
    }

    pub(crate) fn open_all_models_popup(&mut self, presets: Vec<ModelPreset>) {
        if presets.is_empty() {
            self.add_info_message(
                "No additional models are available right now.".to_string(),
                None,
            );
            return;
        }

        let mut items: Vec<SelectionItem> = Vec::new();
        for preset in presets.into_iter() {
            let description =
                (!preset.description.is_empty()).then_some(preset.description.to_string());
            let is_current = preset.model == self.model;
            let single_supported_effort = preset.supported_reasoning_efforts.len() == 1;
            let preset_for_action = preset.clone();
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                let preset_for_event = preset_for_action.clone();
                tx.send(AppEvent::OpenReasoningPopup {
                    model: preset_for_event,
                });
            })];
            items.push(SelectionItem {
                name: preset.display_name.clone(),
                description,
                is_current,
                is_default: preset.is_default,
                actions,
                dismiss_on_select: single_supported_effort,
                ..Default::default()
            });
        }

        let header = self.model_menu_header(
            "Select Model and Effort",
            "Access legacy models by running codex -m <model_name> or in your config.toml",
        );
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some("Press enter to select reasoning effort, or esc to dismiss.".into()),
            items,
            header,
            ..Default::default()
        });
    }

    fn model_selection_actions(
        model_for_action: String,
        effort_for_action: Option<ReasoningEffortConfig>,
    ) -> Vec<SelectionAction> {
        vec![Box::new(move |tx| {
            let effort_label = effort_for_action
                .map(|effort| effort.to_string())
                .unwrap_or_else(|| "default".to_string());
            tx.send(AppEvent::CodexOp(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: None,
                sandbox_policy: None,
                model: Some(model_for_action.clone()),
                effort: Some(effort_for_action),
                summary: None,
            }));
            tx.send(AppEvent::UpdateModel(model_for_action.clone()));
            tx.send(AppEvent::UpdateReasoningEffort(effort_for_action));
            tx.send(AppEvent::PersistModelSelection {
                model: model_for_action.clone(),
                effort: effort_for_action,
            });
            tracing::info!(
                "Selected model: {}, Selected effort: {}",
                model_for_action,
                effort_label
            );
        })]
    }

    /// Open a popup to choose the reasoning effort (stage 2) for the given model.
    pub(crate) fn open_reasoning_popup(&mut self, preset: ModelPreset) {
        let default_effort: ReasoningEffortConfig = preset.default_reasoning_effort;
        let supported = preset.supported_reasoning_efforts;

        let warn_effort = if supported
            .iter()
            .any(|option| option.effort == ReasoningEffortConfig::XHigh)
        {
            Some(ReasoningEffortConfig::XHigh)
        } else if supported
            .iter()
            .any(|option| option.effort == ReasoningEffortConfig::High)
        {
            Some(ReasoningEffortConfig::High)
        } else {
            None
        };
        let warning_text = warn_effort.map(|effort| {
            let effort_label = Self::reasoning_effort_label(effort);
            format!("⚠ {effort_label} reasoning effort can quickly consume Plus plan rate limits.")
        });
        let warn_for_model = preset.model.starts_with("gpt-5.1-codex")
            || preset.model.starts_with("gpt-5.1-codex-max")
            || preset.model.starts_with("gpt-5.2");

        struct EffortChoice {
            stored: Option<ReasoningEffortConfig>,
            display: ReasoningEffortConfig,
        }
        let mut choices: Vec<EffortChoice> = Vec::new();
        for effort in ReasoningEffortConfig::iter() {
            if supported.iter().any(|option| option.effort == effort) {
                choices.push(EffortChoice {
                    stored: Some(effort),
                    display: effort,
                });
            }
        }
        if choices.is_empty() {
            choices.push(EffortChoice {
                stored: Some(default_effort),
                display: default_effort,
            });
        }

        if choices.len() == 1 {
            if let Some(effort) = choices.first().and_then(|c| c.stored) {
                self.apply_model_and_effort(preset.model, Some(effort));
            } else {
                self.apply_model_and_effort(preset.model, None);
            }
            return;
        }

        let default_choice: Option<ReasoningEffortConfig> = choices
            .iter()
            .any(|choice| choice.stored == Some(default_effort))
            .then_some(Some(default_effort))
            .flatten()
            .or_else(|| choices.iter().find_map(|choice| choice.stored))
            .or(Some(default_effort));

        let model_slug = preset.model.to_string();
        let is_current_model = self.model == preset.model;
        let highlight_choice = if is_current_model {
            self.config.model_reasoning_effort
        } else {
            default_choice
        };
        let selection_choice = highlight_choice.or(default_choice);
        let initial_selected_idx = choices
            .iter()
            .position(|choice| choice.stored == selection_choice)
            .or_else(|| {
                selection_choice
                    .and_then(|effort| choices.iter().position(|choice| choice.display == effort))
            });
        let mut items: Vec<SelectionItem> = Vec::new();
        for choice in choices.iter() {
            let effort = choice.display;
            let mut effort_label = Self::reasoning_effort_label(effort).to_string();
            if choice.stored == default_choice {
                effort_label.push_str(" (default)");
            }

            let description = choice
                .stored
                .and_then(|effort| {
                    supported
                        .iter()
                        .find(|option| option.effort == effort)
                        .map(|option| option.description.to_string())
                })
                .filter(|text| !text.is_empty());

            let show_warning = warn_for_model && warn_effort == Some(effort);
            let selected_description = if show_warning {
                warning_text.as_ref().map(|warning_message| {
                    description.as_ref().map_or_else(
                        || warning_message.clone(),
                        |d| format!("{d}\n{warning_message}"),
                    )
                })
            } else {
                None
            };

            let model_for_action = model_slug.clone();
            let actions = Self::model_selection_actions(model_for_action, choice.stored);

            items.push(SelectionItem {
                name: effort_label,
                description,
                selected_description,
                is_current: is_current_model && choice.stored == highlight_choice,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        let mut header = ColumnRenderable::new();
        header.push(Line::from(
            format!("Select Reasoning Level for {model_slug}").bold(),
        ));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    fn reasoning_effort_label(effort: ReasoningEffortConfig) -> &'static str {
        match effort {
            ReasoningEffortConfig::None => "None",
            ReasoningEffortConfig::Minimal => "Minimal",
            ReasoningEffortConfig::Low => "Low",
            ReasoningEffortConfig::Medium => "Medium",
            ReasoningEffortConfig::High => "High",
            ReasoningEffortConfig::XHigh => "Extra high",
        }
    }

    fn apply_model_and_effort(&self, model: String, effort: Option<ReasoningEffortConfig>) {
        self.app_event_tx
            .send(AppEvent::CodexOp(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: None,
                sandbox_policy: None,
                model: Some(model.clone()),
                effort: Some(effort),
                summary: None,
            }));
        self.app_event_tx.send(AppEvent::UpdateModel(model.clone()));
        self.app_event_tx
            .send(AppEvent::UpdateReasoningEffort(effort));
        self.app_event_tx.send(AppEvent::PersistModelSelection {
            model: model.clone(),
            effort,
        });
        tracing::info!(
            "Selected model: {}, Selected effort: {}",
            model,
            effort
                .map(|e| e.to_string())
                .unwrap_or_else(|| "default".to_string())
        );
    }

    /// Open a popup to choose the approvals mode (ask for approval policy + sandbox policy).
    pub(crate) fn open_approvals_popup(&mut self) {
        let current_approval = self.config.approval_policy.value();
        let current_sandbox = self.config.sandbox_policy.get();
        let mut items: Vec<SelectionItem> = Vec::new();
        let presets: Vec<ApprovalPreset> = builtin_approval_presets();

        #[cfg(target_os = "windows")]
        let windows_degraded_sandbox_enabled = codex_core::get_platform_sandbox().is_some()
            && !codex_core::is_windows_elevated_sandbox_enabled();
        #[cfg(not(target_os = "windows"))]
        let windows_degraded_sandbox_enabled = false;

        let show_elevate_sandbox_hint = codex_core::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
            && windows_degraded_sandbox_enabled
            && presets.iter().any(|preset| preset.id == "auto");

        for preset in presets.into_iter() {
            let is_current =
                Self::preset_matches_current(current_approval, current_sandbox, &preset);
            let name = if preset.id == "auto" && windows_degraded_sandbox_enabled {
                "Agent (non-elevated sandbox)".to_string()
            } else {
                preset.label.to_string()
            };
            let description = Some(preset.description.to_string());
            let disabled_reason = match self.config.approval_policy.can_set(&preset.approval) {
                Ok(()) => None,
                Err(err) => Some(err.to_string()),
            };
            let requires_confirmation = preset.id == "full-access"
                && !self
                    .config
                    .notices
                    .hide_full_access_warning
                    .unwrap_or(false);
            let actions: Vec<SelectionAction> = if requires_confirmation {
                let preset_clone = preset.clone();
                vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenFullAccessConfirmation {
                        preset: preset_clone.clone(),
                    });
                })]
            } else if preset.id == "auto" {
                #[cfg(target_os = "windows")]
                {
                    if codex_core::get_platform_sandbox().is_none() {
                        let preset_clone = preset.clone();
                        if codex_core::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                            && codex_core::windows_sandbox::sandbox_setup_is_complete(
                                self.config.codex_home.as_path(),
                            )
                        {
                            vec![Box::new(move |tx| {
                                tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                                    preset: preset_clone.clone(),
                                    mode: WindowsSandboxEnableMode::Elevated,
                                });
                            })]
                        } else {
                            vec![Box::new(move |tx| {
                                tx.send(AppEvent::OpenWindowsSandboxEnablePrompt {
                                    preset: preset_clone.clone(),
                                });
                            })]
                        }
                    } else if let Some((sample_paths, extra_count, failed_scan)) =
                        self.world_writable_warning_details()
                    {
                        let preset_clone = preset.clone();
                        vec![Box::new(move |tx| {
                            tx.send(AppEvent::OpenWorldWritableWarningConfirmation {
                                preset: Some(preset_clone.clone()),
                                sample_paths: sample_paths.clone(),
                                extra_count,
                                failed_scan,
                            });
                        })]
                    } else {
                        Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
                }
            } else {
                Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
            };
            items.push(SelectionItem {
                name,
                description,
                is_current,
                actions,
                dismiss_on_select: true,
                disabled_reason,
                ..Default::default()
            });
        }

        let footer_note = show_elevate_sandbox_hint.then(|| {
            vec![
                "The non-elevated sandbox protects your files and prevents network access under most circumstances. However, it carries greater risk if prompt injected. To upgrade to the elevated sandbox, run ".dim(),
                "/setup-elevated-sandbox".cyan(),
                ".".dim(),
            ]
            .into()
        });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select Approval Mode".to_string()),
            footer_note,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(()),
            ..Default::default()
        });
    }

    pub(crate) fn open_experimental_popup(&mut self) {
        let features: Vec<BetaFeatureItem> = FEATURES
            .iter()
            .filter_map(|spec| {
                let name = spec.stage.beta_menu_name()?;
                let description = spec.stage.beta_menu_description()?;
                Some(BetaFeatureItem {
                    feature: spec.id,
                    name: name.to_string(),
                    description: description.to_string(),
                    enabled: self.config.features.enabled(spec.id),
                })
            })
            .collect();

        let view = ExperimentalFeaturesView::new(features, self.app_event_tx.clone());
        self.bottom_pane.show_view(Box::new(view));
    }

    fn request_weave_session_menu(&self) {
        Self::spawn_weave_session_menu_refresh(self.app_event_tx.clone());
    }

    pub(crate) fn open_weave_session_menu(&mut self, sessions: Vec<WeaveSession>) {
        let mut sessions = sessions;
        sessions.sort_by(|left, right| {
            left.display_name()
                .cmp(&right.display_name())
                .then_with(|| left.id.cmp(&right.id))
        });
        let session_count = sessions.len();
        let mut items: Vec<SelectionItem> = Vec::with_capacity(session_count.saturating_add(4));
        let agent_name = self.weave_agent_name.clone();
        items.push(SelectionItem {
            name: "Set agent name".to_string(),
            description: Some(format!("Current: {agent_name}")),
            selected_description: Some("Press Enter to rename this agent.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::OpenWeaveAgentNamePrompt);
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        items.push(SelectionItem {
            name: String::new(),
            ..Default::default()
        });
        let create_idx = items.len();
        items.push(SelectionItem {
            name: "Create new session".to_string(),
            description: Some("Create a new Weave session.".to_string()),
            selected_description: Some("Press Enter to create a new session.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::OpenWeaveSessionCreatePrompt);
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        let close_disabled = session_count == 0;
        items.push(SelectionItem {
            name: "Close session".to_string(),
            description: Some("Close or manage existing Weave sessions.".to_string()),
            selected_description: Some("Press Enter to manage session close.".to_string()),
            actions: vec![Box::new(|tx| {
                let tx = tx.clone();
                Self::spawn_weave_session_close_menu_refresh(tx);
            })],
            dismiss_on_select: true,
            disabled_reason: close_disabled.then(|| "No sessions to close.".to_string()),
            ..Default::default()
        });
        if session_count > 0 {
            items.push(SelectionItem {
                name: String::new(),
                ..Default::default()
            });
            items.push(SelectionItem {
                is_search_row: true,
                ..Default::default()
            });
        }

        let selected_id = self.selected_weave_session_id.as_deref();
        let mut initial_selected_idx = None;
        let session_start_idx = items.len();
        for session in sessions {
            let display_name = session.display_name();
            let session_id = session.id.clone();
            let search_value = format!("{display_name} {session_id}");
            let is_selected = selected_id == Some(session_id.as_str());
            if is_selected {
                initial_selected_idx = Some(items.len());
            }
            let selected_description = if is_selected {
                "Press Enter to leave this session."
            } else {
                "Press Enter to join this session."
            };
            let selection = if is_selected {
                None
            } else {
                Some(session.clone())
            };
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(AppEvent::SetWeaveSessionSelection {
                    session: selection.clone(),
                });
            })];
            items.push(SelectionItem {
                name: if is_selected {
                    format!("✓ {display_name}")
                } else {
                    display_name
                },
                description: Some(format!("id: {session_id}")),
                selected_description: Some(selected_description.to_string()),
                actions,
                dismiss_on_select: true,
                search_value: Some(search_value),
                ..Default::default()
            });
        }

        let subtitle = if session_count == 0 {
            "No sessions found. Create one to get started.".to_string()
        } else {
            "Select a session to join or leave, or manage sessions.".to_string()
        };
        let initial_selected_idx = initial_selected_idx
            .or_else(|| (session_count > 0).then_some(session_start_idx))
            .or(Some(create_idx));
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Weave".to_string()),
            subtitle: Some(subtitle),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(()),
            is_searchable: session_count > 0,
            search_placeholder: (session_count > 0).then(|| "Type to filter sessions".to_string()),
            show_search_bar: false,
            initial_selected_idx,
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(crate) fn open_weave_session_close_menu(&mut self, sessions: Vec<WeaveSession>) {
        let mut sessions = sessions;
        sessions.sort_by(|left, right| {
            left.display_name()
                .cmp(&right.display_name())
                .then_with(|| left.id.cmp(&right.id))
        });
        let session_count = sessions.len();
        let selected_id = self.selected_weave_session_id.as_deref();
        let mut items: Vec<SelectionItem> = Vec::with_capacity(session_count);
        let mut initial_selected_idx = None;
        for session in sessions {
            let display_name = session.display_name();
            let session_id = session.id.clone();
            let search_value = format!("{display_name} {session_id}");
            let is_selected = selected_id == Some(session_id.as_str());
            if is_selected {
                initial_selected_idx = Some(items.len());
            }
            let session_for_action = session.clone();
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                let tx = tx.clone();
                let session = session_for_action.clone();
                Self::spawn_weave_session_close(tx, session);
            })];
            items.push(SelectionItem {
                name: if is_selected {
                    format!("✓ {display_name}")
                } else {
                    display_name
                },
                description: Some(format!("id: {session_id}")),
                selected_description: Some("Press Enter to close this session.".to_string()),
                actions,
                dismiss_on_select: true,
                search_value: Some(search_value),
                ..Default::default()
            });
        }

        let subtitle = if session_count == 0 {
            "No sessions available to close.".to_string()
        } else {
            "Select a session to close.".to_string()
        };
        let initial_selected_idx = initial_selected_idx.or(Some(0));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Close Weave session".to_string()),
            subtitle: Some(subtitle),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to filter sessions".to_string()),
            initial_selected_idx,
            on_cancel: Some(Box::new(|tx| {
                let tx = tx.clone();
                Self::spawn_weave_session_menu_refresh(tx);
            })),
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(crate) fn open_weave_agent_name_prompt(&mut self) {
        let current_name = self.weave_agent_name.clone();
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            "Weave agent name".to_string(),
            "Type an agent name and press Enter".to_string(),
            Some(format!("Current: {current_name}")),
            Box::new(move |prompt: String| {
                tx.send(AppEvent::SetWeaveAgentName { name: prompt });
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn open_weave_session_create_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            "Create Weave session".to_string(),
            "Type a session name and press Enter".to_string(),
            None,
            Box::new(move |prompt: String| {
                let tx = tx.clone();
                Self::spawn_weave_session_create(tx, prompt);
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    fn weave_session_footer_label(&self) -> Option<String> {
        let session = self.selected_weave_session_name.as_deref()?;
        let agent_name = &self.weave_agent_name;
        Some(format!("name: {agent_name} • session: {session}"))
    }

    fn refresh_weave_session_label(&mut self) {
        self.bottom_pane
            .set_weave_session_label(self.weave_session_footer_label());
    }

    pub(crate) fn set_weave_session_selection(&mut self, session: Option<WeaveSession>) {
        let (session_id, session_name) = session.map_or((None, None), |session| {
            let label = session.display_name();
            let session_id = session.id;
            (Some(session_id), Some(label))
        });
        if self.selected_weave_session_id == session_id
            && self.selected_weave_session_name == session_name
        {
            return;
        }
        self.disconnect_weave_agent();
        self.selected_weave_session_id = session_id;
        self.selected_weave_session_name = session_name;
        self.refresh_weave_session_label();
        if let Some(session_id) = self.selected_weave_session_id.clone() {
            self.weave_agents = Some(Vec::new());
            self.bottom_pane.set_weave_agents(Some(Vec::new()));
            self.connect_weave_agent(session_id);
        } else {
            self.weave_agents = None;
            self.bottom_pane.set_weave_agents(None);
        }
    }

    pub(crate) fn on_weave_session_closed(&mut self, session_id: &str) {
        if self.selected_weave_session_id.as_deref() == Some(session_id) {
            self.set_weave_session_selection(None);
        }
    }

    pub(crate) fn on_weave_agent_disconnected(&mut self, session_id: &str) {
        if self.selected_weave_session_id.as_deref() != Some(session_id) {
            return;
        }
        let label = self
            .selected_weave_session_name
            .clone()
            .unwrap_or_else(|| session_id.to_string());
        self.set_weave_session_selection(None);
        self.add_to_history(history_cell::new_error_event(format!(
            "Weave session closed: {label}"
        )));
    }

    pub(crate) fn set_weave_agent_name(&mut self, name: String) {
        let trimmed = name.trim();
        if trimmed.is_empty() || self.weave_agent_name == trimmed {
            return;
        }
        self.weave_agent_name = trimmed.to_string();
        if self.selected_weave_session_id.is_some() {
            self.refresh_weave_session_label();
        }
        if let Some(connection) = self.weave_agent_connection.as_mut() {
            connection.set_agent_name(self.weave_agent_name.clone());
            let sender = connection.sender();
            let name = self.weave_agent_name.clone();
            let tx = self.app_event_tx.clone();
            tokio::spawn(async move {
                if let Err(err) = sender.update_agent_name(name).await {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!(
                            "Failed to update Weave agent name: {err}"
                        )),
                    )));
                }
            });
        }
    }

    pub(crate) fn on_weave_agent_connected(
        &mut self,
        session_id: String,
        mut connection: WeaveAgentConnection,
    ) {
        if self.selected_weave_session_id.as_deref() != Some(session_id.as_str()) {
            let mut connection = connection;
            connection.shutdown();
            return;
        }
        if let Some(mut incoming_rx) = connection.take_incoming_rx() {
            let tx = self.app_event_tx.clone();
            let session_id_for_event = session_id;
            tokio::spawn(async move {
                while let Some(message) = incoming_rx.recv().await {
                    tx.send(AppEvent::WeaveMessageReceived { message });
                }
                tx.send(AppEvent::WeaveAgentDisconnected {
                    session_id: session_id_for_event,
                });
            });
        }
        self.weave_agent_connection = Some(connection);
        self.request_weave_agent_list();
        self.maybe_send_pending_weave_new_session_result();
    }

    pub(crate) fn on_weave_agent_connect_failed(&mut self, session_id: &str) {
        if self.selected_weave_session_id.as_deref() == Some(session_id) {
            self.set_weave_session_selection(None);
        }
    }

    pub(crate) fn apply_weave_agent_list(&mut self, session_id: String, agents: Vec<WeaveAgent>) {
        if self.selected_weave_session_id.as_deref() != Some(session_id.as_str()) {
            return;
        }
        self.weave_agents = Some(agents.clone());
        self.bottom_pane.set_weave_agents(Some(agents));
        self.request_redraw();
    }

    pub(crate) fn on_weave_message_received(&mut self, mut message: WeaveIncomingMessage) {
        if self.selected_weave_session_id.as_deref() != Some(message.session_id.as_str()) {
            return;
        }
        if let Some(action_result) = message.action_result.take() {
            self.handle_weave_action_result(&message.src, message.src_name.clone(), action_result);
            return;
        }
        if let Some(task_update) = message.task_update.take() {
            self.handle_weave_task_update(&message.src, task_update);
            return;
        }
        if let Some(task_done) = message.task_done.take() {
            self.handle_weave_task_done(&message.src, task_done);
            return;
        }
        if message.defer_until_ready && self.should_defer_weave_action_message() {
            self.pending_weave_action_messages.push_back(message);
            return;
        }
        let WeaveIncomingMessage {
            session_id,
            message_id,
            src,
            src_name,
            text,
            kind,
            conversation_id,
            conversation_owner,
            tool,
            has_conversation_metadata,
            parent_message_id: _,
            task_id,
            action_group_id,
            action_id,
            action_index,
            reply_to_action_id,
            action_result: _,
            task_update: _,
            task_done: _,
            defer_until_ready: _,
        } = message;
        if let Some(agents) = self.weave_agents.as_mut() {
            if let Some(agent) = agents.iter_mut().find(|agent| agent.id == src) {
                if let Some(name) = src_name.clone() {
                    agent.name = Some(name);
                }
            } else {
                agents.push(WeaveAgent {
                    id: src.clone(),
                    name: src_name.clone(),
                });
            }
            self.bottom_pane.set_weave_agents(Some(agents.clone()));
        }
        let display_src = src_name
            .or_else(|| {
                self.weave_agents.as_ref().and_then(|agents| {
                    agents
                        .iter()
                        .find(|agent| agent.id == src)
                        .map(WeaveAgent::display_name)
                })
            })
            .unwrap_or_else(|| src.clone());
        if let Some(reply_to_action_id) = reply_to_action_id.as_deref()
            && self.clear_pending_weave_action(src.as_str(), reply_to_action_id)
        {
            self.maybe_send_next_queued_input();
        }
        if let Some(action_id) = action_id.as_deref() {
            self.update_weave_target_last_action_id(src.as_str(), action_id);
        }
        if !self.should_accept_weave_message(src.as_str(), task_id.as_deref()) {
            self.add_to_history(history_cell::new_error_event(format!(
                "Dropped Weave message from {display_src}: missing task_id."
            )));
            return;
        }
        if let Some(tool) = tool
            && self.apply_weave_tool(
                tool,
                WeaveToolContext {
                    sender: display_src.as_str(),
                    sender_id: src.as_str(),
                    text: text.as_str(),
                    conversation_id: conversation_id.as_str(),
                    conversation_owner: conversation_owner.as_str(),
                    has_conversation_metadata,
                    action_group_id: action_group_id.as_deref(),
                    action_id: action_id.as_deref(),
                    action_index,
                },
            )
        {
            return;
        }
        if text.trim().is_empty() {
            return;
        }
        let sender_is_owner = conversation_owner == src;
        let dst_is_owner = conversation_owner == self.weave_agent_id;
        self.add_to_history(history_cell::new_weave_inbound(
            display_src.clone(),
            self.weave_agent_name.clone(),
            text.clone(),
            sender_is_owner,
            dst_is_owner,
        ));
        let reply_target = WeaveReplyTarget {
            session_id,
            agent_id: src,
            display_name: display_src,
            conversation_id: conversation_id.clone(),
            conversation_owner: conversation_owner.clone(),
            parent_message_id: Some(message_id),
            task_id: task_id.clone(),
            action_group_id,
            action_id,
            action_index,
        };
        let auto_reply = matches!(kind, WeaveMessageKind::User);
        let relay_targets = if conversation_owner == self.weave_agent_id
            && self.matches_active_weave_relay(
                reply_target.agent_id.as_str(),
                conversation_id.as_str(),
                task_id.as_deref(),
            ) {
            self.active_weave_relay.clone()
        } else {
            None
        };
        let weave_message = WeaveUserMessage {
            reply_target,
            auto_reply,
            relay_targets,
            conversation_id,
            conversation_owner,
            task_id,
            kind,
        };
        self.queue_user_message(UserMessage::from_weave(text, weave_message));
        self.request_redraw();
    }

    fn should_accept_weave_message(&mut self, sender_id: &str, task_id: Option<&str>) -> bool {
        let Some(task_id) = task_id.map(str::trim).filter(|value| !value.is_empty()) else {
            return true;
        };
        let should_update = self
            .weave_inbound_task_ids
            .get(sender_id)
            .is_none_or(|active| active != task_id);
        if should_update {
            self.weave_inbound_task_ids
                .insert(sender_id.to_string(), task_id.to_string());
        }
        true
    }

    fn handle_weave_action_result(
        &mut self,
        sender_id: &str,
        sender_name: Option<String>,
        result: WeaveActionResult,
    ) {
        let status = result.status.trim();
        let is_terminal = matches!(status, "completed" | "rejected" | "error");
        if is_terminal && self.clear_pending_weave_action(sender_id, result.action_id.as_str()) {
            self.maybe_send_next_queued_input();
        }
        if let (Some(context_id), Some(task_id)) = (
            result.new_context_id.as_deref(),
            result.new_task_id.as_deref(),
        ) {
            let state = self
                .weave_target_states
                .entry(sender_id.to_string())
                .or_insert_with(|| {
                    WeaveRelayTargetState::new(context_id.to_string(), task_id.to_string())
                });
            state.context_id = context_id.to_string();
            state.task_id = task_id.to_string();
            if let Some(reply_target) = self.active_weave_reply_target.as_mut()
                && reply_target.agent_id == sender_id
            {
                reply_target.conversation_id = context_id.to_string();
                reply_target.conversation_owner = self.weave_agent_id.clone();
                reply_target.task_id = Some(task_id.to_string());
            }
        }
        if status == "accepted" {
            return;
        }
        if !matches!(status, "error" | "rejected") {
            return;
        }
        let display_src = sender_name
            .or_else(|| {
                self.weave_agents.as_ref().and_then(|agents| {
                    agents
                        .iter()
                        .find(|agent| agent.id == sender_id)
                        .map(WeaveAgent::display_name)
                })
            })
            .unwrap_or_else(|| sender_id.to_string());
        let detail = result
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let message = match detail {
            Some(detail) => {
                format!("Weave action result from {display_src} reported {status}: {detail}")
            }
            None => format!("Weave action result from {display_src} reported {status}."),
        };
        self.add_to_history(history_cell::new_error_event(message));
    }

    pub(crate) fn on_weave_action_submit_failed(&mut self, group_id: &str) {
        if self.clear_pending_weave_actions_for_group(group_id) {
            self.maybe_send_next_queued_input();
        }
    }

    fn handle_weave_task_update(&mut self, sender_id: &str, update: WeaveTaskUpdate) {
        let status = update.status.trim();
        if status.is_empty() {
            return;
        }
        if status == "done" {
            self.handle_weave_task_done(
                sender_id,
                WeaveTaskDone {
                    task_id: update.task_id,
                    summary: None,
                },
            );
            return;
        }
        let display_src = self
            .weave_agents
            .as_ref()
            .and_then(|agents| {
                agents
                    .iter()
                    .find(|agent| agent.id == sender_id)
                    .map(WeaveAgent::display_name)
            })
            .unwrap_or_else(|| sender_id.to_string());
        let message = format!("Weave task update from {display_src}: {status}.");
        self.add_to_history(history_cell::new_info_event(message, None));
    }

    fn handle_weave_task_done(&mut self, sender_id: &str, done: WeaveTaskDone) {
        let display_src = self
            .weave_agents
            .as_ref()
            .and_then(|agents| {
                agents
                    .iter()
                    .find(|agent| agent.id == sender_id)
                    .map(WeaveAgent::display_name)
            })
            .unwrap_or_else(|| sender_id.to_string());
        let summary = done
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let message = match summary {
            Some(summary) => format!("Weave task done from {display_src}: {summary}"),
            None => format!("Weave task done from {display_src}."),
        };
        self.add_to_history(history_cell::new_info_event(message, None));
        if self.clear_pending_weave_actions_for_task(sender_id, done.task_id.as_str()) {
            self.maybe_send_next_queued_input();
        }
    }

    fn ensure_weave_target_state(&mut self, agent_id: &str) -> (String, String) {
        let state = self
            .weave_target_states
            .entry(agent_id.to_string())
            .or_insert_with(|| {
                WeaveRelayTargetState::new(new_weave_conversation_id(), new_weave_task_id())
            });
        (state.context_id.clone(), state.task_id.clone())
    }

    fn reset_weave_relay_targets(&mut self, targets: &WeaveRelayTargets) {
        for target_id in &targets.target_ids {
            let state = self
                .weave_target_states
                .entry(target_id.clone())
                .or_insert_with(|| {
                    WeaveRelayTargetState::new(new_weave_conversation_id(), new_weave_task_id())
                });
            state.context_id = new_weave_conversation_id();
            state.task_id = new_weave_task_id();
            state.pending_actions.clear();
            state.last_action_id = None;
        }
    }

    fn update_weave_target_last_action_id(&mut self, agent_id: &str, action_id: &str) {
        let state = self
            .weave_target_states
            .entry(agent_id.to_string())
            .or_insert_with(|| {
                WeaveRelayTargetState::new(new_weave_conversation_id(), new_weave_task_id())
            });
        state.last_action_id = Some(action_id.to_string());
    }

    fn register_pending_weave_action(
        &mut self,
        agent_id: &str,
        action_id: &str,
        group_id: &str,
        expects_reply: bool,
    ) {
        let state = self
            .weave_target_states
            .entry(agent_id.to_string())
            .or_insert_with(|| {
                WeaveRelayTargetState::new(new_weave_conversation_id(), new_weave_task_id())
            });
        state.pending_actions.insert(
            action_id.to_string(),
            WeavePendingAction {
                group_id: group_id.to_string(),
                expects_reply,
            },
        );
    }

    fn clear_pending_weave_action(&mut self, agent_id: &str, action_id: &str) -> bool {
        let Some(state) = self.weave_target_states.get_mut(agent_id) else {
            return false;
        };
        state.pending_actions.remove(action_id).is_some()
    }

    fn clear_pending_weave_actions_for_task(&mut self, agent_id: &str, task_id: &str) -> bool {
        let Some(state) = self.weave_target_states.get_mut(agent_id) else {
            return false;
        };
        if state.task_id != task_id {
            return false;
        }
        if state.pending_actions.is_empty() {
            return false;
        }
        state.pending_actions.clear();
        true
    }

    fn clear_pending_weave_actions_for_targets(&mut self, targets: &WeaveRelayTargets) {
        for target_id in &targets.target_ids {
            if let Some(state) = self.weave_target_states.get_mut(target_id) {
                state.pending_actions.clear();
            }
        }
    }

    fn clear_pending_weave_actions_for_group(&mut self, group_id: &str) -> bool {
        let mut removed = false;
        for state in self.weave_target_states.values_mut() {
            let before = state.pending_actions.len();
            state
                .pending_actions
                .retain(|_, pending| pending.group_id != group_id);
            if state.pending_actions.len() != before {
                removed = true;
            }
        }
        removed
    }

    fn should_defer_weave_action_message(&self) -> bool {
        self.pending_weave_new_session_result.is_some() || self.thread_id.is_none()
    }

    fn flush_pending_weave_action_messages(&mut self) {
        if self.pending_weave_action_messages.is_empty() {
            return;
        }
        let pending_context = self.pending_weave_new_session_context.clone();
        let mut pending = std::mem::take(&mut self.pending_weave_action_messages);
        while let Some(mut message) = pending.pop_front() {
            message.defer_until_ready = false;
            if let Some(context) = pending_context.as_ref()
                && message
                    .action_group_id
                    .as_deref()
                    .is_some_and(|group_id| group_id == context.group_id.as_str())
            {
                message.conversation_id = context.conversation_id.clone();
                message.conversation_owner = context.conversation_owner.clone();
                message.task_id = Some(context.task_id.clone());
                message.has_conversation_metadata = true;
            }
            self.on_weave_message_received(message);
        }
    }

    fn apply_weave_tool(&mut self, tool: WeaveTool, context: WeaveToolContext<'_>) -> bool {
        let WeaveToolContext {
            sender,
            sender_id,
            text,
            conversation_id,
            conversation_owner,
            has_conversation_metadata,
            action_group_id,
            action_id,
            action_index,
        } = context;
        let is_task_running = self.bottom_pane.is_task_running();
        match tool {
            WeaveTool::NewSession => {
                let can_defer = is_task_running
                    && self.should_defer_weave_new_session(
                        conversation_id,
                        conversation_owner,
                        sender_id,
                    );
                if is_task_running && !can_defer {
                    let message = "'/new' is disabled while a task is in progress.".to_string();
                    self.add_to_history(history_cell::new_error_event(message));
                    self.send_weave_action_result_for_context(
                        sender_id,
                        action_group_id,
                        action_id,
                        action_index,
                        WeaveActionResultContext {
                            status: "blocked",
                            detail: None,
                            new_context_id: None,
                            new_task_id: None,
                        },
                    );
                    self.request_redraw();
                    return true;
                }
                self.add_to_history(history_cell::new_info_event(
                    format!("Weave control: /new requested by {sender}."),
                    None,
                ));
                if let (Some(group_id), Some(action_id), Some(action_index)) =
                    (action_group_id, action_id, action_index)
                {
                    self.pending_weave_new_session_result = Some(WeavePendingActionResult {
                        sender_id: sender_id.to_string(),
                        group_id: group_id.to_string(),
                        action_id: action_id.to_string(),
                        action_index,
                    });
                    self.pending_weave_new_session_context = Some(WeavePendingNewSessionContext {
                        group_id: group_id.to_string(),
                        conversation_id: new_weave_conversation_id(),
                        conversation_owner: sender_id.to_string(),
                        task_id: new_weave_task_id(),
                    });
                }
                if can_defer {
                    self.pending_weave_new_session = true;
                    self.request_redraw();
                    return true;
                }
                self.pending_weave_new_session = false;
                self.start_weave_new_session();
                true
            }
            WeaveTool::Compact => {
                if is_task_running {
                    let message = "'/compact' is disabled while a task is in progress.".to_string();
                    self.add_to_history(history_cell::new_error_event(message));
                    self.send_weave_action_result_for_context(
                        sender_id,
                        action_group_id,
                        action_id,
                        action_index,
                        WeaveActionResultContext {
                            status: "blocked",
                            detail: None,
                            new_context_id: None,
                            new_task_id: None,
                        },
                    );
                    self.request_redraw();
                    return true;
                }
                self.active_weave_control_context = Some(WeaveControlContext {
                    sender_id: sender_id.to_string(),
                });
                self.add_to_history(history_cell::new_info_event(
                    format!("Weave control: /compact requested by {sender}."),
                    None,
                ));
                self.clear_token_usage();
                self.app_event_tx.send(AppEvent::CodexOp(Op::Compact));
                self.send_weave_action_result_for_context(
                    sender_id,
                    action_group_id,
                    action_id,
                    action_index,
                    WeaveActionResultContext {
                        status: "completed",
                        detail: None,
                        new_context_id: None,
                        new_task_id: None,
                    },
                );
                self.request_redraw();
                true
            }
            WeaveTool::Interrupt => {
                let mut canceled =
                    self.interrupt_weave_task(conversation_id, conversation_owner, sender_id);
                let mut should_interrupt = if has_conversation_metadata {
                    self.should_interrupt_on_interrupt(
                        conversation_id,
                        conversation_owner,
                        sender_id,
                    )
                } else {
                    self.should_interrupt_for_sender_interrupt(sender_id)
                };
                if has_conversation_metadata && !should_interrupt {
                    should_interrupt = self.should_interrupt_for_sender_interrupt(sender_id);
                }
                if has_conversation_metadata {
                    self.clear_weave_reply_state(conversation_id, conversation_owner);
                } else if self.clear_weave_reply_state_for_sender(sender_id) {
                    canceled = true;
                }
                if should_interrupt {
                    self.suppress_weave_interrupt = true;
                    self.submit_op(Op::Interrupt);
                }
                let message = if canceled || should_interrupt {
                    format!("Weave control: /interrupt requested by {sender}.")
                } else {
                    format!("Weave control: /interrupt from {sender} (no active task).")
                };
                self.add_to_history(history_cell::new_info_event(message, None));
                let status = if canceled || should_interrupt {
                    "completed"
                } else {
                    "rejected"
                };
                self.send_weave_action_result_for_context(
                    sender_id,
                    action_group_id,
                    action_id,
                    action_index,
                    WeaveActionResultContext {
                        status,
                        detail: None,
                        new_context_id: None,
                        new_task_id: None,
                    },
                );
                self.request_redraw();
                true
            }
            WeaveTool::Review { instructions } => {
                if is_task_running {
                    let message = "'/review' is disabled while a task is in progress.".to_string();
                    self.add_to_history(history_cell::new_error_event(message));
                    self.send_weave_action_result_for_context(
                        sender_id,
                        action_group_id,
                        action_id,
                        action_index,
                        WeaveActionResultContext {
                            status: "blocked",
                            detail: None,
                            new_context_id: None,
                            new_task_id: None,
                        },
                    );
                    self.request_redraw();
                    return true;
                }
                let text = text.trim();
                let instructions = instructions.or_else(|| {
                    if text.is_empty() || text.eq_ignore_ascii_case("/review") {
                        None
                    } else {
                        Some(text.to_string())
                    }
                });
                let review_request = if let Some(instructions) = instructions {
                    ReviewRequest {
                        target: ReviewTarget::Custom { instructions },
                        user_facing_hint: None,
                    }
                } else {
                    ReviewRequest {
                        target: ReviewTarget::UncommittedChanges,
                        user_facing_hint: None,
                    }
                };
                self.active_weave_control_context = Some(WeaveControlContext {
                    sender_id: sender_id.to_string(),
                });
                self.add_to_history(history_cell::new_info_event(
                    format!("Weave control: /review requested by {sender}."),
                    None,
                ));
                self.submit_op(Op::Review { review_request });
                self.send_weave_action_result_for_context(
                    sender_id,
                    action_group_id,
                    action_id,
                    action_index,
                    WeaveActionResultContext {
                        status: "completed",
                        detail: None,
                        new_context_id: None,
                        new_task_id: None,
                    },
                );
                self.request_redraw();
                true
            }
        }
    }

    fn connect_weave_agent(&mut self, session_id: String) {
        let agent_id = self.weave_agent_id.clone();
        let agent_name = self.weave_agent_name.clone();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            match weave_client::connect_agent(session_id.clone(), agent_id, Some(agent_name)).await
            {
                Ok(connection) => {
                    tx.send(AppEvent::WeaveAgentConnected {
                        session_id,
                        connection,
                    });
                }
                Err(err) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!(
                            "Failed to join Weave session {session_id}: {err}"
                        )),
                    )));
                    tx.send(AppEvent::WeaveAgentConnectFailed { session_id });
                }
            }
        });
    }

    fn disconnect_weave_agent(&mut self) {
        if let Some(mut connection) = self.weave_agent_connection.take() {
            connection.shutdown();
        }
        self.weave_agents = None;
        self.pending_weave_relay = None;
        self.active_weave_relay = None;
        self.active_weave_plan = None;
        self.pending_weave_plan = false;
        self.weave_relay_buffer.clear();
        self.weave_inbound_task_ids.clear();
        self.weave_target_states.clear();
        self.pending_weave_action_messages.clear();
        self.pending_weave_new_session_result = None;
        self.pending_weave_new_session_context = None;
        self.pending_weave_new_session = false;
        self.active_weave_control_context = None;
        self.bottom_pane.set_weave_agents(None);
        self.refresh_weave_session_label();
    }

    pub(crate) fn request_weave_agent_list(&self) {
        let Some(session_id) = self.selected_weave_session_id.clone() else {
            return;
        };
        let src = self.weave_agent_id.clone();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            match weave_client::list_agents(&session_id, &src).await {
                Ok(agents) => {
                    tx.send(AppEvent::WeaveAgentListResult { session_id, agents });
                }
                Err(err) => {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!("Weave agents unavailable: {err}")),
                    )));
                }
            }
        });
    }

    fn spawn_weave_session_menu_refresh(app_event_tx: AppEventSender) {
        tokio::spawn(async move {
            match weave_client::list_sessions().await {
                Ok(sessions) => {
                    app_event_tx.send(AppEvent::OpenWeaveSessionMenu { sessions });
                }
                Err(err) => {
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!("Weave sessions unavailable: {err}")),
                    )));
                }
            }
        });
    }

    fn spawn_weave_session_close_menu_refresh(app_event_tx: AppEventSender) {
        tokio::spawn(async move {
            match weave_client::list_sessions().await {
                Ok(sessions) => {
                    app_event_tx.send(AppEvent::OpenWeaveSessionCloseMenu { sessions });
                }
                Err(err) => {
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!("Weave sessions unavailable: {err}")),
                    )));
                }
            }
        });
    }

    fn spawn_weave_session_create(app_event_tx: AppEventSender, name: String) {
        tokio::spawn(async move {
            match weave_client::create_session(Some(name)).await {
                Ok(session) => {
                    let label = session.display_name();
                    let session_id = session.id.clone();
                    let message = if label == session_id {
                        format!("Created Weave session {label}")
                    } else {
                        format!("Created Weave session {label} ({session_id})")
                    };
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(message, None),
                    )));
                    app_event_tx.send(AppEvent::SetWeaveSessionSelection {
                        session: Some(session),
                    });
                }
                Err(err) => {
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!(
                            "Failed to create Weave session: {err}"
                        )),
                    )));
                }
            }
        });
    }

    fn spawn_weave_session_close(app_event_tx: AppEventSender, session: WeaveSession) {
        tokio::spawn(async move {
            let label = session.display_name();
            match weave_client::close_session(&session.id).await {
                Ok(()) => {
                    let session_id = session.id;
                    let message = if label == session_id {
                        format!("Closed Weave session {label}")
                    } else {
                        format!("Closed Weave session {label} ({session_id})")
                    };
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(message, None),
                    )));
                    app_event_tx.send(AppEvent::WeaveSessionClosed { session_id });
                }
                Err(err) => {
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(format!(
                            "Failed to close Weave session {label}: {err}"
                        )),
                    )));
                }
            }
        });
    }

    fn approval_preset_actions(
        approval: AskForApproval,
        sandbox: SandboxPolicy,
    ) -> Vec<SelectionAction> {
        vec![Box::new(move |tx| {
            let sandbox_clone = sandbox.clone();
            tx.send(AppEvent::CodexOp(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(approval),
                sandbox_policy: Some(sandbox_clone.clone()),
                model: None,
                effort: None,
                summary: None,
            }));
            tx.send(AppEvent::UpdateAskForApprovalPolicy(approval));
            tx.send(AppEvent::UpdateSandboxPolicy(sandbox_clone));
        })]
    }

    fn preset_matches_current(
        current_approval: AskForApproval,
        current_sandbox: &SandboxPolicy,
        preset: &ApprovalPreset,
    ) -> bool {
        if current_approval != preset.approval {
            return false;
        }
        matches!(
            (&preset.sandbox, current_sandbox),
            (SandboxPolicy::ReadOnly, SandboxPolicy::ReadOnly)
                | (
                    SandboxPolicy::DangerFullAccess,
                    SandboxPolicy::DangerFullAccess
                )
                | (
                    SandboxPolicy::WorkspaceWrite { .. },
                    SandboxPolicy::WorkspaceWrite { .. }
                )
        )
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn world_writable_warning_details(&self) -> Option<(Vec<String>, usize, bool)> {
        if self
            .config
            .notices
            .hide_world_writable_warning
            .unwrap_or(false)
        {
            return None;
        }
        let cwd = self.config.cwd.clone();
        let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
        match codex_windows_sandbox::apply_world_writable_scan_and_denies(
            self.config.codex_home.as_path(),
            cwd.as_path(),
            &env_map,
            self.config.sandbox_policy.get(),
            Some(self.config.codex_home.as_path()),
        ) {
            Ok(_) => None,
            Err(_) => Some((Vec::new(), 0, true)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn world_writable_warning_details(&self) -> Option<(Vec<String>, usize, bool)> {
        None
    }

    pub(crate) fn open_full_access_confirmation(&mut self, preset: ApprovalPreset) {
        let approval = preset.approval;
        let sandbox = preset.sandbox;
        let mut header_children: Vec<Box<dyn Renderable>> = Vec::new();
        let title_line = Line::from("Enable full access?").bold();
        let info_line = Line::from(vec![
            "When Codex runs with full access, it can edit any file on your computer and run commands with network, without your approval. "
                .into(),
            "Exercise caution when enabling full access. This significantly increases the risk of data loss, leaks, or unexpected behavior."
                .fg(Color::Red),
        ]);
        header_children.push(Box::new(title_line));
        header_children.push(Box::new(
            Paragraph::new(vec![info_line]).wrap(Wrap { trim: false }),
        ));
        let header = ColumnRenderable::with(header_children);

        let mut accept_actions = Self::approval_preset_actions(approval, sandbox.clone());
        accept_actions.push(Box::new(|tx| {
            tx.send(AppEvent::UpdateFullAccessWarningAcknowledged(true));
        }));

        let mut accept_and_remember_actions = Self::approval_preset_actions(approval, sandbox);
        accept_and_remember_actions.push(Box::new(|tx| {
            tx.send(AppEvent::UpdateFullAccessWarningAcknowledged(true));
            tx.send(AppEvent::PersistFullAccessWarningAcknowledged);
        }));

        let deny_actions: Vec<SelectionAction> = vec![Box::new(|tx| {
            tx.send(AppEvent::OpenApprovalsPopup);
        })];

        let items = vec![
            SelectionItem {
                name: "Yes, continue anyway".to_string(),
                description: Some("Apply full access for this session".to_string()),
                actions: accept_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Yes, and don't ask again".to_string(),
                description: Some("Enable full access and remember this choice".to_string()),
                actions: accept_and_remember_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Cancel".to_string(),
                description: Some("Go back without enabling full access".to_string()),
                actions: deny_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn open_world_writable_warning_confirmation(
        &mut self,
        preset: Option<ApprovalPreset>,
        sample_paths: Vec<String>,
        extra_count: usize,
        failed_scan: bool,
    ) {
        let (approval, sandbox) = match &preset {
            Some(p) => (Some(p.approval), Some(p.sandbox.clone())),
            None => (None, None),
        };
        let mut header_children: Vec<Box<dyn Renderable>> = Vec::new();
        let describe_policy = |policy: &SandboxPolicy| match policy {
            SandboxPolicy::WorkspaceWrite { .. } => "Agent mode",
            SandboxPolicy::ReadOnly => "Read-Only mode",
            _ => "Agent mode",
        };
        let mode_label = preset
            .as_ref()
            .map(|p| describe_policy(&p.sandbox))
            .unwrap_or_else(|| describe_policy(self.config.sandbox_policy.get()));
        let info_line = if failed_scan {
            Line::from(vec![
                "We couldn't complete the world-writable scan, so protections cannot be verified. "
                    .into(),
                format!("The Windows sandbox cannot guarantee protection in {mode_label}.")
                    .fg(Color::Red),
            ])
        } else {
            Line::from(vec![
                "The Windows sandbox cannot protect writes to folders that are writable by Everyone.".into(),
                " Consider removing write access for Everyone from the following folders:".into(),
            ])
        };
        header_children.push(Box::new(
            Paragraph::new(vec![info_line]).wrap(Wrap { trim: false }),
        ));

        if !sample_paths.is_empty() {
            // Show up to three examples and optionally an "and X more" line.
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            for p in &sample_paths {
                lines.push(Line::from(format!("  - {p}")));
            }
            if extra_count > 0 {
                lines.push(Line::from(format!("and {extra_count} more")));
            }
            header_children.push(Box::new(Paragraph::new(lines).wrap(Wrap { trim: false })));
        }
        let header = ColumnRenderable::with(header_children);

        // Build actions ensuring acknowledgement happens before applying the new sandbox policy,
        // so downstream policy-change hooks don't re-trigger the warning.
        let mut accept_actions: Vec<SelectionAction> = Vec::new();
        // Suppress the immediate re-scan only when a preset will be applied (i.e., via /approvals),
        // to avoid duplicate warnings from the ensuing policy change.
        if preset.is_some() {
            accept_actions.push(Box::new(|tx| {
                tx.send(AppEvent::SkipNextWorldWritableScan);
            }));
        }
        if let (Some(approval), Some(sandbox)) = (approval, sandbox.clone()) {
            accept_actions.extend(Self::approval_preset_actions(approval, sandbox));
        }

        let mut accept_and_remember_actions: Vec<SelectionAction> = Vec::new();
        accept_and_remember_actions.push(Box::new(|tx| {
            tx.send(AppEvent::UpdateWorldWritableWarningAcknowledged(true));
            tx.send(AppEvent::PersistWorldWritableWarningAcknowledged);
        }));
        if let (Some(approval), Some(sandbox)) = (approval, sandbox) {
            accept_and_remember_actions.extend(Self::approval_preset_actions(approval, sandbox));
        }

        let items = vec![
            SelectionItem {
                name: "Continue".to_string(),
                description: Some(format!("Apply {mode_label} for this session")),
                actions: accept_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Continue and don't warn again".to_string(),
                description: Some(format!("Enable {mode_label} and remember this choice")),
                actions: accept_and_remember_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn open_world_writable_warning_confirmation(
        &mut self,
        _preset: Option<ApprovalPreset>,
        _sample_paths: Vec<String>,
        _extra_count: usize,
        _failed_scan: bool,
    ) {
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn open_windows_sandbox_enable_prompt(&mut self, preset: ApprovalPreset) {
        use ratatui_macros::line;

        if !codex_core::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED {
            // Legacy flow (pre-NUX): explain the experimental sandbox and let the user enable it
            // directly (no elevation prompts).
            let mut header = ColumnRenderable::new();
            header.push(*Box::new(
                Paragraph::new(vec![
                    line!["Agent mode on Windows uses an experimental sandbox to limit network and filesystem access.".bold()],
                    line!["Learn more: https://developers.openai.com/codex/windows"],
                ])
                .wrap(Wrap { trim: false }),
            ));

            let preset_clone = preset;
            let items = vec![
                SelectionItem {
                    name: "Enable experimental sandbox".to_string(),
                    description: None,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                            preset: preset_clone.clone(),
                            mode: WindowsSandboxEnableMode::Legacy,
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Go back".to_string(),
                    description: None,
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::OpenApprovalsPopup);
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ];

            self.bottom_pane.show_selection_view(SelectionViewParams {
                title: None,
                footer_hint: Some(standard_popup_hint_line()),
                items,
                header: Box::new(header),
                ..Default::default()
            });
            return;
        }

        let current_approval = self.config.approval_policy.value();
        let current_sandbox = self.config.sandbox_policy.get();
        let presets = builtin_approval_presets();
        let stay_full_access = presets
            .iter()
            .find(|preset| preset.id == "full-access")
            .is_some_and(|preset| {
                Self::preset_matches_current(current_approval, current_sandbox, preset)
            });
        let stay_actions = if stay_full_access {
            Vec::new()
        } else {
            presets
                .iter()
                .find(|preset| preset.id == "read-only")
                .map(|preset| {
                    Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
                })
                .unwrap_or_default()
        };
        let stay_label = if stay_full_access {
            "Stay in Agent Full Access".to_string()
        } else {
            "Stay in Read-Only".to_string()
        };

        let mut header = ColumnRenderable::new();
        header.push(*Box::new(
            Paragraph::new(vec![
                line!["Set Up Agent Sandbox".bold()],
                line![""],
                line!["Agent mode uses an experimental Windows sandbox that protects your files and prevents network access by default."],
                line!["Learn more: https://developers.openai.com/codex/windows"],
            ])
            .wrap(Wrap { trim: false }),
        ));

        let items = vec![
            SelectionItem {
                name: "Set up agent sandbox (requires elevation)".to_string(),
                description: None,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::BeginWindowsSandboxElevatedSetup {
                        preset: preset.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: stay_label,
                description: None,
                actions: stay_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn open_windows_sandbox_enable_prompt(&mut self, _preset: ApprovalPreset) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn open_windows_sandbox_fallback_prompt(
        &mut self,
        preset: ApprovalPreset,
        reason: WindowsSandboxFallbackReason,
    ) {
        use ratatui_macros::line;

        let _ = reason;

        let current_approval = self.config.approval_policy.value();
        let current_sandbox = self.config.sandbox_policy.get();
        let presets = builtin_approval_presets();
        let stay_full_access = presets
            .iter()
            .find(|preset| preset.id == "full-access")
            .is_some_and(|preset| {
                Self::preset_matches_current(current_approval, current_sandbox, preset)
            });
        let stay_actions = if stay_full_access {
            Vec::new()
        } else {
            presets
                .iter()
                .find(|preset| preset.id == "read-only")
                .map(|preset| {
                    Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
                })
                .unwrap_or_default()
        };
        let stay_label = if stay_full_access {
            "Stay in Agent Full Access".to_string()
        } else {
            "Stay in Read-Only".to_string()
        };

        let mut lines = Vec::new();
        lines.push(line!["Use Non-Elevated Sandbox?".bold()]);
        lines.push(line![""]);
        lines.push(line![
            "Elevation failed. You can also use a non-elevated sandbox, which protects your files and prevents network access under most circumstances. However, it carries greater risk if prompt injected."
        ]);
        lines.push(line![
            "Learn more: https://developers.openai.com/codex/windows"
        ]);

        let mut header = ColumnRenderable::new();
        header.push(*Box::new(Paragraph::new(lines).wrap(Wrap { trim: false })));

        let elevated_preset = preset.clone();
        let legacy_preset = preset;
        let items = vec![
            SelectionItem {
                name: "Try elevated agent sandbox setup again".to_string(),
                description: None,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::BeginWindowsSandboxElevatedSetup {
                        preset: elevated_preset.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Use non-elevated agent sandbox".to_string(),
                description: None,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                        preset: legacy_preset.clone(),
                        mode: WindowsSandboxEnableMode::Legacy,
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: stay_label,
                description: None,
                actions: stay_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn open_windows_sandbox_fallback_prompt(
        &mut self,
        _preset: ApprovalPreset,
        _reason: WindowsSandboxFallbackReason,
    ) {
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn maybe_prompt_windows_sandbox_enable(&mut self) {
        if self.config.forced_auto_mode_downgraded_on_windows
            && codex_core::get_platform_sandbox().is_none()
            && let Some(preset) = builtin_approval_presets()
                .into_iter()
                .find(|preset| preset.id == "auto")
        {
            self.open_windows_sandbox_enable_prompt(preset);
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn maybe_prompt_windows_sandbox_enable(&mut self) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn show_windows_sandbox_setup_status(&mut self) {
        // While elevated sandbox setup runs, prevent typing so the user doesn't
        // accidentally queue messages that will run under an unexpected mode.
        self.bottom_pane.set_composer_input_enabled(
            false,
            Some("Input disabled until setup completes.".to_string()),
        );
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane.set_interrupt_hint_visible(false);
        self.set_status_header("Setting up agent sandbox. This can take a minute.".to_string());
        self.request_redraw();
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn show_windows_sandbox_setup_status(&mut self) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn clear_windows_sandbox_setup_status(&mut self) {
        self.bottom_pane.set_composer_input_enabled(true, None);
        self.bottom_pane.hide_status_indicator();
        self.request_redraw();
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn clear_windows_sandbox_setup_status(&mut self) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn clear_forced_auto_mode_downgrade(&mut self) {
        self.config.forced_auto_mode_downgraded_on_windows = false;
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn clear_forced_auto_mode_downgrade(&mut self) {}

    /// Set the approval policy in the widget's config copy.
    pub(crate) fn set_approval_policy(&mut self, policy: AskForApproval) {
        if let Err(err) = self.config.approval_policy.set(policy) {
            tracing::warn!(%err, "failed to set approval_policy on chat config");
        }
    }

    /// Set the sandbox policy in the widget's config copy.
    pub(crate) fn set_sandbox_policy(&mut self, policy: SandboxPolicy) -> ConstraintResult<()> {
        #[cfg(target_os = "windows")]
        let should_clear_downgrade = !matches!(&policy, SandboxPolicy::ReadOnly)
            || codex_core::get_platform_sandbox().is_some();

        self.config.sandbox_policy.set(policy)?;

        #[cfg(target_os = "windows")]
        if should_clear_downgrade {
            self.config.forced_auto_mode_downgraded_on_windows = false;
        }

        Ok(())
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn set_feature_enabled(&mut self, feature: Feature, enabled: bool) {
        if enabled {
            self.config.features.enable(feature);
        } else {
            self.config.features.disable(feature);
        }
    }

    pub(crate) fn set_full_access_warning_acknowledged(&mut self, acknowledged: bool) {
        self.config.notices.hide_full_access_warning = Some(acknowledged);
    }

    pub(crate) fn set_world_writable_warning_acknowledged(&mut self, acknowledged: bool) {
        self.config.notices.hide_world_writable_warning = Some(acknowledged);
    }

    pub(crate) fn set_rate_limit_switch_prompt_hidden(&mut self, hidden: bool) {
        self.config.notices.hide_rate_limit_model_nudge = Some(hidden);
        if hidden {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
        }
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn world_writable_warning_hidden(&self) -> bool {
        self.config
            .notices
            .hide_world_writable_warning
            .unwrap_or(false)
    }

    /// Set the reasoning effort in the widget's config copy.
    pub(crate) fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        self.config.model_reasoning_effort = effort;
    }

    /// Set the model in the widget's config copy.
    pub(crate) fn set_model(&mut self, model: &str) {
        self.session_header.set_model(model);
        self.model = model.to_string();
    }

    pub(crate) fn add_info_message(&mut self, message: String, hint: Option<String>) {
        self.add_to_history(history_cell::new_info_event(message, hint));
        self.request_redraw();
    }

    pub(crate) fn add_plain_history_lines(&mut self, lines: Vec<Line<'static>>) {
        self.add_boxed_history(Box::new(PlainHistoryCell::new(lines)));
        self.request_redraw();
    }

    pub(crate) fn add_error_message(&mut self, message: String) {
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();
    }

    pub(crate) fn add_mcp_output(&mut self) {
        if self.config.mcp_servers.is_empty() {
            self.add_to_history(history_cell::empty_mcp_output());
        } else {
            self.submit_op(Op::ListMcpTools);
        }
    }

    /// Forward file-search results to the bottom pane.
    pub(crate) fn apply_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        self.bottom_pane.on_file_search_result(query, matches);
    }

    /// Handle Ctrl-C key press.
    fn on_ctrl_c(&mut self) {
        if self.bottom_pane.on_ctrl_c() == CancellationEvent::Handled {
            return;
        }

        if self.bottom_pane.is_task_running() {
            self.bottom_pane.show_ctrl_c_quit_hint();
            self.submit_op(Op::Interrupt);
            return;
        }

        self.submit_op(Op::Shutdown);
    }

    pub(crate) fn composer_is_empty(&self) -> bool {
        self.bottom_pane.composer_is_empty()
    }

    /// True when the UI is in the regular composer state with no running task,
    /// no modal overlay (e.g. approvals or status indicator), and no composer popups.
    /// In this state Esc-Esc backtracking is enabled.
    pub(crate) fn is_normal_backtrack_mode(&self) -> bool {
        self.bottom_pane.is_normal_backtrack_mode()
    }

    pub(crate) fn insert_str(&mut self, text: &str) {
        self.bottom_pane.insert_str(text);
    }

    /// Replace the composer content with the provided text and reset cursor.
    pub(crate) fn set_composer_text(&mut self, text: String) {
        self.bottom_pane.set_composer_text(text);
    }

    pub(crate) fn show_esc_backtrack_hint(&mut self) {
        self.bottom_pane.show_esc_backtrack_hint();
    }

    pub(crate) fn clear_esc_backtrack_hint(&mut self) {
        self.bottom_pane.clear_esc_backtrack_hint();
    }
    /// Forward an `Op` directly to codex.
    pub(crate) fn submit_op(&self, op: Op) {
        // Record outbound operation for session replay fidelity.
        crate::session_log::log_outbound_op(&op);
        if let Err(e) = self.codex_op_tx.send(op) {
            tracing::error!("failed to submit op: {e}");
        }
    }

    fn on_list_mcp_tools(&mut self, ev: McpListToolsResponseEvent) {
        self.add_to_history(history_cell::new_mcp_tools_output(
            &self.config,
            ev.tools,
            ev.resources,
            ev.resource_templates,
            &ev.auth_statuses,
        ));
    }

    fn on_list_custom_prompts(&mut self, ev: ListCustomPromptsResponseEvent) {
        let len = ev.custom_prompts.len();
        debug!("received {len} custom prompts");
        // Forward to bottom pane so the slash popup can show them now.
        self.bottom_pane.set_custom_prompts(ev.custom_prompts);
    }

    fn on_list_skills(&mut self, ev: ListSkillsResponseEvent) {
        self.set_skills_from_response(&ev);
    }

    pub(crate) fn open_review_popup(&mut self) {
        let mut items: Vec<SelectionItem> = Vec::new();

        items.push(SelectionItem {
            name: "Review against a base branch".to_string(),
            description: Some("(PR Style)".into()),
            actions: vec![Box::new({
                let cwd = self.config.cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewBranchPicker(cwd.clone()));
                }
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Review uncommitted changes".to_string(),
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::CodexOp(Op::Review {
                    review_request: ReviewRequest {
                        target: ReviewTarget::UncommittedChanges,
                        user_facing_hint: None,
                    },
                }));
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        // New: Review a specific commit (opens commit picker)
        items.push(SelectionItem {
            name: "Review a commit".to_string(),
            actions: vec![Box::new({
                let cwd = self.config.cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewCommitPicker(cwd.clone()));
                }
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Custom review instructions".to_string(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenReviewCustomPrompt);
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a review preset".into()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) async fn show_review_branch_picker(&mut self, cwd: &Path) {
        let branches = local_git_branches(cwd).await;
        let current_branch = current_branch_name(cwd)
            .await
            .unwrap_or_else(|| "(detached HEAD)".to_string());
        let mut items: Vec<SelectionItem> = Vec::with_capacity(branches.len());

        for option in branches {
            let branch = option.clone();
            items.push(SelectionItem {
                name: format!("{current_branch} -> {branch}"),
                actions: vec![Box::new(move |tx3: &AppEventSender| {
                    tx3.send(AppEvent::CodexOp(Op::Review {
                        review_request: ReviewRequest {
                            target: ReviewTarget::BaseBranch {
                                branch: branch.clone(),
                            },
                            user_facing_hint: None,
                        },
                    }));
                })],
                dismiss_on_select: true,
                search_value: Some(option),
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a base branch".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search branches".to_string()),
            ..Default::default()
        });
    }

    pub(crate) async fn show_review_commit_picker(&mut self, cwd: &Path) {
        let commits = codex_core::git_info::recent_commits(cwd, 100).await;

        let mut items: Vec<SelectionItem> = Vec::with_capacity(commits.len());
        for entry in commits {
            let subject = entry.subject.clone();
            let sha = entry.sha.clone();
            let search_val = format!("{subject} {sha}");

            items.push(SelectionItem {
                name: subject.clone(),
                actions: vec![Box::new(move |tx3: &AppEventSender| {
                    tx3.send(AppEvent::CodexOp(Op::Review {
                        review_request: ReviewRequest {
                            target: ReviewTarget::Commit {
                                sha: sha.clone(),
                                title: Some(subject.clone()),
                            },
                            user_facing_hint: None,
                        },
                    }));
                })],
                dismiss_on_select: true,
                search_value: Some(search_val),
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a commit to review".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search commits".to_string()),
            ..Default::default()
        });
    }

    pub(crate) fn show_review_custom_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            "Custom review instructions".to_string(),
            "Type instructions and press Enter".to_string(),
            None,
            Box::new(move |prompt: String| {
                let trimmed = prompt.trim().to_string();
                if trimmed.is_empty() {
                    return;
                }
                tx.send(AppEvent::CodexOp(Op::Review {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Custom {
                            instructions: trimmed,
                        },
                        user_facing_hint: None,
                    },
                }));
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    pub(crate) fn token_usage(&self) -> TokenUsage {
        self.token_info
            .as_ref()
            .map(|ti| ti.total_token_usage.clone())
            .unwrap_or_default()
    }

    pub(crate) fn thread_id(&self) -> Option<ThreadId> {
        self.thread_id
    }

    pub(crate) fn rollout_path(&self) -> Option<PathBuf> {
        self.current_rollout_path.clone()
    }

    /// Return a reference to the widget's current config (includes any
    /// runtime overrides applied via TUI, e.g., model or approval policy).
    pub(crate) fn config_ref(&self) -> &Config {
        &self.config
    }

    pub(crate) fn clear_token_usage(&mut self) {
        self.token_info = None;
    }

    fn as_renderable(&self) -> RenderableItem<'_> {
        let active_cell_renderable = match &self.active_cell {
            Some(cell) => RenderableItem::Borrowed(cell).inset(Insets::tlbr(1, 0, 0, 0)),
            None => RenderableItem::Owned(Box::new(())),
        };
        let mut flex = FlexRenderable::new();
        flex.push(1, active_cell_renderable);
        flex.push(
            0,
            RenderableItem::Borrowed(&self.bottom_pane).inset(Insets::tlbr(1, 0, 0, 0)),
        );
        RenderableItem::Owned(Box::new(flex))
    }
}

impl Drop for ChatWidget {
    fn drop(&mut self) {
        self.stop_rate_limit_poller();
        self.disconnect_weave_agent();
    }
}

impl Renderable for ChatWidget {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.as_renderable().render(area, buf);
        self.last_rendered_width.set(Some(area.width as usize));
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.as_renderable().desired_height(width)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.as_renderable().cursor_pos(area)
    }
}

enum Notification {
    AgentTurnComplete { response: String },
    ExecApprovalRequested { command: String },
    EditApprovalRequested { cwd: PathBuf, changes: Vec<PathBuf> },
    ElicitationRequested { server_name: String },
}

impl Notification {
    fn display(&self) -> String {
        match self {
            Notification::AgentTurnComplete { response } => {
                Notification::agent_turn_preview(response)
                    .unwrap_or_else(|| "Agent turn complete".to_string())
            }
            Notification::ExecApprovalRequested { command } => {
                format!("Approval requested: {}", truncate_text(command, 30))
            }
            Notification::EditApprovalRequested { cwd, changes } => {
                format!(
                    "Codex wants to edit {}",
                    if changes.len() == 1 {
                        #[allow(clippy::unwrap_used)]
                        display_path_for(changes.first().unwrap(), cwd)
                    } else {
                        format!("{} files", changes.len())
                    }
                )
            }
            Notification::ElicitationRequested { server_name } => {
                format!("Approval requested by {server_name}")
            }
        }
    }

    fn type_name(&self) -> &str {
        match self {
            Notification::AgentTurnComplete { .. } => "agent-turn-complete",
            Notification::ExecApprovalRequested { .. }
            | Notification::EditApprovalRequested { .. }
            | Notification::ElicitationRequested { .. } => "approval-requested",
        }
    }

    fn allowed_for(&self, settings: &Notifications) -> bool {
        match settings {
            Notifications::Enabled(enabled) => *enabled,
            Notifications::Custom(allowed) => allowed.iter().any(|a| a == self.type_name()),
        }
    }

    fn agent_turn_preview(response: &str) -> Option<String> {
        let mut normalized = String::new();
        for part in response.split_whitespace() {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push_str(part);
        }
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(truncate_text(trimmed, AGENT_NOTIFICATION_PREVIEW_GRAPHEMES))
        }
    }
}

const AGENT_NOTIFICATION_PREVIEW_GRAPHEMES: usize = 200;
const EXAMPLE_PROMPTS: [&str; 6] = [
    "Explain this codebase",
    "Summarize recent commits",
    "Implement {feature}",
    "Find and fix a bug in @filename",
    "Write tests for @filename",
    "Improve documentation in @filename",
];

// Extract the first bold (Markdown) element in the form **...** from `s`.
// Returns the inner text if found; otherwise `None`.
fn extract_first_bold(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'*' {
            let start = i + 2;
            let mut j = start;
            while j + 1 < bytes.len() {
                if bytes[j] == b'*' && bytes[j + 1] == b'*' {
                    // Found closing **
                    let inner = &s[start..j];
                    let trimmed = inner.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    } else {
                        return None;
                    }
                }
                j += 1;
            }
            // No closing; stop searching (wait for more deltas)
            return None;
        }
        i += 1;
    }
    None
}

async fn fetch_rate_limits(base_url: String, auth: CodexAuth) -> Option<RateLimitSnapshot> {
    match BackendClient::from_auth(base_url, &auth) {
        Ok(client) => match client.get_rate_limits().await {
            Ok(snapshot) => Some(snapshot),
            Err(err) => {
                debug!(error = ?err, "failed to fetch rate limits from /usage");
                None
            }
        },
        Err(err) => {
            debug!(error = ?err, "failed to construct backend client for rate limits");
            None
        }
    }
}

#[cfg(test)]
pub(crate) fn show_review_commit_picker_with_entries(
    chat: &mut ChatWidget,
    entries: Vec<codex_core::git_info::CommitLogEntry>,
) {
    let mut items: Vec<SelectionItem> = Vec::with_capacity(entries.len());
    for entry in entries {
        let subject = entry.subject.clone();
        let sha = entry.sha.clone();
        let search_val = format!("{subject} {sha}");

        items.push(SelectionItem {
            name: subject.clone(),
            actions: vec![Box::new(move |tx3: &AppEventSender| {
                tx3.send(AppEvent::CodexOp(Op::Review {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Commit {
                            sha: sha.clone(),
                            title: Some(subject.clone()),
                        },
                        user_facing_hint: None,
                    },
                }));
            })],
            dismiss_on_select: true,
            search_value: Some(search_val),
            ..Default::default()
        });
    }

    chat.bottom_pane.show_selection_view(SelectionViewParams {
        title: Some("Select a commit to review".to_string()),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        is_searchable: true,
        search_placeholder: Some("Type to search commits".to_string()),
        ..Default::default()
    });
}

fn skills_for_cwd(cwd: &Path, skills_entries: &[SkillsListEntry]) -> Vec<SkillMetadata> {
    skills_entries
        .iter()
        .find(|entry| entry.cwd.as_path() == cwd)
        .map(|entry| {
            entry
                .skills
                .iter()
                .map(|skill| SkillMetadata {
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                    short_description: skill.short_description.clone(),
                    path: skill.path.clone(),
                    scope: skill.scope,
                })
                .collect()
        })
        .unwrap_or_default()
}

struct WeaveTokenContext<'a> {
    text: &'a str,
    mention_lookup: &'a HashMap<String, WeaveAgent>,
    recipients: &'a mut Vec<WeaveAgent>,
    seen_ids: &'a mut HashSet<String>,
    self_agent_id: Option<&'a str>,
}

#[derive(Clone, Debug)]
struct WeaveControlAction {
    targets: Vec<WeaveAgent>,
    tool: WeaveTool,
}

fn weave_tool_from_relay_command(command: WeaveRelayCommand) -> WeaveTool {
    match command {
        WeaveRelayCommand::New => WeaveTool::NewSession,
        WeaveRelayCommand::Compact => WeaveTool::Compact,
        WeaveRelayCommand::Interrupt => WeaveTool::Interrupt,
        WeaveRelayCommand::Review => WeaveTool::Review { instructions: None },
    }
}

fn weave_control_action_parts(tool: &WeaveTool) -> (&'static str, &'static str) {
    match tool {
        WeaveTool::NewSession => ("new", "/new"),
        WeaveTool::Compact => ("compact", "/compact"),
        WeaveTool::Interrupt => ("interrupt", "/interrupt"),
        WeaveTool::Review { .. } => ("review", "/review"),
    }
}

fn weave_agent_label(agent: &WeaveAgent) -> String {
    let name = agent
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    match name {
        Some(name) if name != agent.id => format!("{name} ({})", agent.id),
        Some(name) => name.to_string(),
        None => agent.id.clone(),
    }
}

fn weave_task_target_labels(targets: &WeaveRelayTargets, agents: Option<&[WeaveAgent]>) -> String {
    let agents = agents.unwrap_or(&[]);
    let mut ids = targets.target_ids.iter().cloned().collect::<Vec<_>>();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let name = agents
                .iter()
                .find(|agent| agent.id == id)
                .map(WeaveAgent::display_name)
                .unwrap_or(id);
            format!("#{name}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn weave_pending_reply_labels(
    pending: &HashMap<String, WeaveRelayTargetState>,
    agents: Option<&[WeaveAgent]>,
) -> String {
    let mut ids = pending
        .iter()
        .filter(|(_, state)| state.has_pending_reply())
        .map(|(agent_id, _)| agent_id.clone())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return "none".to_string();
    }
    let agents = agents.unwrap_or(&[]);
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let name = agents
                .iter()
                .find(|agent| agent.id == id)
                .map(WeaveAgent::display_name)
                .unwrap_or(id);
            format!("#{name}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_batched_weave_replies(replies: Vec<(String, String)>) -> String {
    let mut lines = Vec::new();
    lines.push("Weave replies:".to_string());
    let total = replies.len();
    for (idx, (sender, text)) in replies.into_iter().enumerate() {
        let trimmed = text.trim_end();
        let mut iter = trimmed.lines();
        if let Some(first) = iter.next() {
            lines.push(format!("#{sender}: {first}"));
            for line in iter {
                lines.push(format!("  {line}"));
            }
        } else {
            lines.push(format!("#{sender}: <empty>"));
        }
        if idx + 1 < total {
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

fn build_weave_relay_prompt(targets: &[WeaveAgent], wants_plan: bool) -> String {
    let target_list = targets
        .iter()
        .map(|agent| format!("#{}", weave_agent_label(agent)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = Vec::new();
    lines.push("Weave relay:".to_string());
    lines.push(format!("Targets: {target_list}"));
    lines.push("Messages with #mentions indicate a weave task.".to_string());
    if wants_plan {
        lines.push(
            "For a new task, include a short plan in the FIRST JSON reply using a `plan.steps` array."
                .to_string(),
        );
        lines.push(
            "Do not send a standalone plan response; include the plan alongside the first message action."
                .to_string(),
        );
    }
    lines.push("Continue coordinating with the target(s) until the task is complete.".to_string());
    lines.push(
        "While the task is in progress, respond ONLY with a single-line JSON object.".to_string(),
    );
    lines.push(
        "Use type \"relay_actions\" with an `actions` array to send messages or control commands."
            .to_string(),
    );
    lines.push(
        "Never send `relay_actions` with an empty `actions` array; use `task_done` instead."
            .to_string(),
    );
    lines.push("Action types: \"message\", \"control\".".to_string());
    lines.push("Provide a `command` for control actions.".to_string());
    lines.push(
        "If asked to call /new, /interrupt, /compact, or /review, send a control action instead of describing it."
            .to_string(),
    );
    lines.push(
        "After sending actions to multiple targets, wait for replies before sending more messages unless the user requests otherwise."
            .to_string(),
    );
    lines.push(
        "Order actions in the array to guarantee control happens before the next message."
            .to_string(),
    );
    lines.push(
        "When the task is complete, respond with type \"task_done\" and a brief summary for the local user.".to_string(),
    );
    lines.push("Do not forward the original prompt verbatim.".to_string());
    lines.push(
        "Honor any instructions like \"do not share\" or secrets withheld from the target."
            .to_string(),
    );
    lines.push(
        "Respond ONLY with a single-line JSON object. No extra text, no code fences.".to_string(),
    );
    if wants_plan {
        lines.push(
            r#"{"type":"relay_actions","actions":[{"type":"message","dst":"<agent_id_or_name>","text":"...","plan":{"steps":["..."]}}]}"#
                .to_string(),
        );
    } else {
        lines.push(
            r#"{"type":"relay_actions","actions":[{"type":"message","dst":"<agent_id_or_name>","text":"..."}]}"#
                .to_string(),
        );
    }
    lines.push(
        r#"{"type":"relay_actions","actions":[{"type":"message","dst":"<agent_a>","text":"..."},{"type":"message","dst":"<agent_b>","text":"..."}]}"#
            .to_string(),
    );
    lines.push(
        r#"{"type":"relay_actions","actions":[{"type":"control","dst":"<agent_id_or_name>","command":"new"}]}"#.to_string(),
    );
    lines.push(r#"{"type":"task_done","summary":"..."}"#.to_string());
    lines.join("\n")
}

fn should_request_weave_plan(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let non_empty_lines = trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    if non_empty_lines > 1 {
        return true;
    }
    let keywords = [
        "if", "then", "until", "again", "repeat", "retry", "loop", "unless",
    ];
    trimmed.split_whitespace().any(|token| {
        let normalized = token
            .trim_matches(|ch: char| !ch.is_alphanumeric())
            .to_ascii_lowercase();
        keywords.iter().any(|keyword| keyword == &normalized)
    })
}

fn new_weave_conversation_id() -> String {
    Uuid::new_v4().to_string()
}

fn new_weave_task_id() -> String {
    Uuid::new_v4().to_string()
}

fn new_weave_action_group_id() -> String {
    Uuid::new_v4().to_string()
}

fn new_weave_action_id() -> String {
    Uuid::new_v4().to_string()
}

fn extract_weave_relay_control_tokens(text: &str) -> (Vec<WeaveTool>, String) {
    let mut tools = Vec::new();
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        match trimmed {
            "/new" => tools.push(WeaveTool::NewSession),
            "/interrupt" => tools.push(WeaveTool::Interrupt),
            _ => lines.push(line.to_string()),
        }
    }
    (tools, lines.join("\n"))
}

fn normalize_weave_relay_target(label: &str) -> String {
    let trimmed = label.trim().trim_start_matches('#');
    let trimmed = trimmed
        .split_once('(')
        .map(|(head, _)| head)
        .unwrap_or(trimmed)
        .trim();
    trimmed.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn resolve_weave_relay_targets(agents: &[WeaveAgent], targets: &[String]) -> Vec<WeaveAgent> {
    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    for target in targets {
        let label = normalize_weave_relay_target(target);
        if label.is_empty() {
            continue;
        }
        if let Some(agent) = agents
            .iter()
            .find(|agent| normalize_weave_relay_target(&agent.id) == label)
        {
            if seen.insert(agent.id.clone()) {
                resolved.push(agent.clone());
            }
            continue;
        }
        let lower = label.to_lowercase();
        for agent in agents {
            let Some(name) = agent
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            let normalized = normalize_weave_relay_target(name);
            if normalized.is_empty() || normalized.to_lowercase() != lower {
                continue;
            }
            if seen.insert(agent.id.clone()) {
                resolved.push(agent.clone());
            }
        }
    }
    resolved
}

fn parse_weave_control_actions(
    text: &str,
    agents: &[WeaveAgent],
    self_agent_id: Option<&str>,
) -> (Vec<WeaveControlAction>, String) {
    let original_text = text.to_string();
    let mut mention_lookup: HashMap<String, WeaveAgent> = HashMap::new();
    for agent in agents {
        mention_lookup
            .entry(agent.mention_text())
            .or_insert_with(|| agent.clone());
    }
    let mut actions = Vec::new();
    let mut kept_lines = Vec::new();
    let mut has_non_control_lines = false;
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() {
            kept_lines.push(line);
            continue;
        }
        let mut targets = Vec::new();
        let mut seen_ids = HashSet::new();
        for token in &tokens {
            let Some(mention) = token.strip_prefix('#') else {
                continue;
            };
            if mention.is_empty() {
                continue;
            }
            let Some(agent) = mention_lookup.get(mention) else {
                continue;
            };
            if self_agent_id == Some(agent.id.as_str()) {
                continue;
            }
            if seen_ids.insert(agent.id.clone()) {
                targets.push(agent.clone());
            }
        }
        let mut command = None;
        let mut command_index = None;
        for (idx, token) in tokens.iter().enumerate() {
            match *token {
                "/new" => {
                    command = Some("new");
                    command_index = Some(idx);
                    break;
                }
                "/compact" => {
                    command = Some("compact");
                    command_index = Some(idx);
                    break;
                }
                "/interrupt" => {
                    command = Some("interrupt");
                    command_index = Some(idx);
                    break;
                }
                "/review" => {
                    command = Some("review");
                    command_index = Some(idx);
                    break;
                }
                _ => {}
            }
        }
        let Some(command) = command else {
            kept_lines.push(line);
            has_non_control_lines = true;
            continue;
        };
        let Some(command_index) = command_index else {
            kept_lines.push(line);
            has_non_control_lines = true;
            continue;
        };
        if targets.is_empty()
            || tokens[..command_index]
                .iter()
                .any(|token| !token.starts_with('#'))
        {
            kept_lines.push(line);
            has_non_control_lines = true;
            continue;
        }
        let trailing = tokens.get(command_index + 1..).unwrap_or(&[]);
        if command != "review" && !trailing.is_empty() {
            kept_lines.push(line);
            has_non_control_lines = true;
            continue;
        }
        if command == "review" && trailing.iter().any(|token| token.starts_with('#')) {
            kept_lines.push(line);
            has_non_control_lines = true;
            continue;
        }
        let tool = match command {
            "new" => WeaveTool::NewSession,
            "compact" => WeaveTool::Compact,
            "interrupt" => WeaveTool::Interrupt,
            "review" => {
                let args = trailing.join(" ");
                let instructions = {
                    let trimmed = args.trim();
                    (!trimmed.is_empty()).then_some(trimmed.to_string())
                };
                WeaveTool::Review { instructions }
            }
            _ => {
                kept_lines.push(line);
                has_non_control_lines = true;
                continue;
            }
        };
        actions.push(WeaveControlAction { targets, tool });
        continue;
    }
    if has_non_control_lines {
        return (Vec::new(), original_text);
    }
    let cleaned_text = kept_lines.join("\n");
    (actions, cleaned_text)
}

fn find_weave_mentions(
    text: &str,
    agents: &[WeaveAgent],
    self_agent_id: Option<&str>,
) -> Vec<WeaveAgent> {
    let mut mention_lookup: HashMap<String, WeaveAgent> = HashMap::new();
    for agent in agents {
        mention_lookup
            .entry(agent.mention_text())
            .or_insert_with(|| agent.clone());
    }
    let mut recipients = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut ctx = WeaveTokenContext {
        text,
        mention_lookup: &mention_lookup,
        recipients: &mut recipients,
        seen_ids: &mut seen_ids,
        self_agent_id,
    };
    let mut token_start: Option<usize> = None;
    for (idx, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = token_start.take() {
                process_weave_token(&mut ctx, start, idx);
            }
            continue;
        }
        if token_start.is_none() {
            token_start = Some(idx);
        }
    }
    if let Some(start) = token_start {
        process_weave_token(&mut ctx, start, text.len());
    }
    recipients
}

fn process_weave_token(ctx: &mut WeaveTokenContext<'_>, start: usize, end: usize) {
    let token = &ctx.text[start..end];
    let Some(mention) = token.strip_prefix('#') else {
        return;
    };
    if mention.is_empty() {
        return;
    }
    let Some(agent) = ctx.mention_lookup.get(mention) else {
        return;
    };
    if ctx.self_agent_id == Some(agent.id.as_str()) {
        return;
    }
    if ctx.seen_ids.insert(agent.id.clone()) {
        ctx.recipients.push(agent.clone());
    }
}

fn find_skill_mentions(text: &str, skills: &[SkillMetadata]) -> Vec<SkillMetadata> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut matches: Vec<SkillMetadata> = Vec::new();
    for skill in skills {
        if seen.contains(&skill.name) {
            continue;
        }
        let needle = format!("${}", skill.name);
        if text.contains(&needle) {
            seen.insert(skill.name.clone());
            matches.push(skill.clone());
        }
    }
    matches
}

#[cfg(test)]
pub(crate) mod tests;
