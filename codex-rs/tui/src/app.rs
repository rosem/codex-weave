use crate::app_backtrack::BacktrackState;
use crate::app_event::AppEvent;
use crate::app_event::ExitMode;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxEnableMode;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxFallbackReason;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::chatwidget::ChatDraftState;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::ExternalEditorState;
use crate::cwd_prompt::CwdPromptAction;
use crate::diff_render::DiffSummary;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::external_editor;
use crate::file_search::FileSearchManager;
use crate::history_cell;
use crate::history_cell::HistoryCell;
#[cfg(not(debug_assertions))]
use crate::history_cell::UpdateAvailableHistoryCell;
use crate::model_migration::ModelMigrationOutcome;
use crate::model_migration::migration_copy_for_models;
use crate::model_migration::run_model_migration_prompt;
use crate::pager_overlay::Overlay;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::Renderable;
use crate::resume_picker::SessionSelection;
use crate::tui;
use crate::tui::TuiEvent;
use crate::update_action::UpdateAction;
use codex_ansi_escape::ansi_escape_line;
use codex_app_server_protocol::ConfigLayerSource;
use codex_core::AuthManager;
use codex_core::CodexAuth;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core::config::ConfigOverrides;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_core::config_loader::ConfigLayerStackOrdering;
#[cfg(target_os = "windows")]
use codex_core::features::Feature;
use codex_core::models_manager::manager::RefreshStrategy;
use codex_core::models_manager::model_presets::HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG;
use codex_core::models_manager::model_presets::HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG;
use codex_core::protocol::AskForApproval;
use codex_core::protocol::DeprecationNoticeEvent;
use codex_core::protocol::Event;
use codex_core::protocol::EventMsg;
use codex_core::protocol::FinalOutput;
use codex_core::protocol::ListSkillsResponseEvent;
#[cfg(any(test, target_os = "windows"))]
use codex_core::protocol::Op;
use codex_core::protocol::SandboxPolicy;
use codex_core::protocol::SessionSource;
use codex_core::protocol::SkillErrorInfo;
use codex_core::protocol::SubAgentSource;
use codex_core::protocol::TokenUsage;
use codex_otel::OtelManager;
use codex_protocol::ThreadId;
use codex_protocol::items::TurnItem;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelUpgrade;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::protocol::SessionConfiguredEvent;
use codex_utils_absolute_path::AbsolutePathBuf;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::unbounded_channel;
use toml::Value as TomlValue;

const EXTERNAL_EDITOR_HINT: &str = "Save and close external editor to continue.";
const THREAD_EVENT_CHANNEL_CAPACITY: usize = 1024;
const THREAD_ID_PREFIX_LEN: usize = 8;

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub thread_id: Option<ThreadId>,
    pub update_action: Option<UpdateAction>,
    pub exit_reason: ExitReason,
}

impl AppExitInfo {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            token_usage: TokenUsage::default(),
            thread_id: None,
            update_action: None,
            exit_reason: ExitReason::Fatal(message.into()),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AppRunControl {
    Continue,
    Exit(ExitReason),
}

#[derive(Debug, Clone)]
pub enum ExitReason {
    UserRequested,
    Fatal(String),
}

fn session_summary(token_usage: TokenUsage, thread_id: Option<ThreadId>) -> Option<SessionSummary> {
    if token_usage.is_zero() {
        return None;
    }

    let usage_line = FinalOutput::from(token_usage).to_string();
    let resume_command = thread_id.map(|thread_id| format!("codex resume {thread_id}"));
    Some(SessionSummary {
        usage_line,
        resume_command,
    })
}

fn errors_for_cwd(cwd: &Path, response: &ListSkillsResponseEvent) -> Vec<SkillErrorInfo> {
    response
        .skills
        .iter()
        .find(|entry| entry.cwd.as_path() == cwd)
        .map(|entry| entry.errors.clone())
        .unwrap_or_default()
}

fn emit_skill_load_warnings(app_event_tx: &AppEventSender, errors: &[SkillErrorInfo]) {
    if errors.is_empty() {
        return;
    }

    let error_count = errors.len();
    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        crate::history_cell::new_warning_event(format!(
            "Skipped loading {error_count} skill(s) due to invalid SKILL.md files."
        )),
    )));

    for error in errors {
        let path = error.path.display();
        let message = error.message.as_str();
        app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            crate::history_cell::new_warning_event(format!("{path}: {message}")),
        )));
    }
}

fn emit_deprecation_notice(app_event_tx: &AppEventSender, notice: Option<DeprecationNoticeEvent>) {
    let Some(DeprecationNoticeEvent { summary, details }) = notice else {
        return;
    };
    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        crate::history_cell::new_deprecation_notice(summary, details),
    )));
}

fn emit_project_config_warnings(app_event_tx: &AppEventSender, config: &Config) {
    let mut disabled_folders = Vec::new();

    for layer in config
        .config_layer_stack
        .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true)
    {
        let ConfigLayerSource::Project { dot_codex_folder } = &layer.name else {
            continue;
        };
        if layer.disabled_reason.is_none() {
            continue;
        }
        disabled_folders.push((
            dot_codex_folder.as_path().display().to_string(),
            layer
                .disabled_reason
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "Config folder disabled.".to_string()),
        ));
    }

    if disabled_folders.is_empty() {
        return;
    }

    let mut message = "The following config folders are disabled:\n".to_string();
    for (index, (folder, reason)) in disabled_folders.iter().enumerate() {
        let display_index = index + 1;
        message.push_str(&format!("    {display_index}. {folder}\n"));
        message.push_str(&format!("       {reason}\n"));
    }

    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        history_cell::new_warning_event(message),
    )));
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSummary {
    usage_line: String,
    resume_command: Option<String>,
}

#[derive(Debug, Clone)]
struct ThreadEventSnapshot {
    session_configured: Option<Event>,
    events: Vec<Event>,
}

#[derive(Debug)]
struct ThreadEventStore {
    session_configured: Option<Event>,
    buffer: VecDeque<Event>,
    user_message_ids: HashSet<String>,
    capacity: usize,
    active: bool,
}

impl ThreadEventStore {
    fn new(capacity: usize) -> Self {
        Self {
            session_configured: None,
            buffer: VecDeque::new(),
            user_message_ids: HashSet::new(),
            capacity,
            active: false,
        }
    }

    fn new_with_session_configured(capacity: usize, event: Event) -> Self {
        let mut store = Self::new(capacity);
        store.session_configured = Some(event);
        store
    }

    fn push_event(&mut self, event: Event) {
        match &event.msg {
            EventMsg::SessionConfigured(_) => {
                self.session_configured = Some(event);
                return;
            }
            EventMsg::ItemCompleted(completed) => {
                if let TurnItem::UserMessage(item) = &completed.item {
                    if !event.id.is_empty() && self.user_message_ids.contains(&event.id) {
                        return;
                    }
                    let legacy = Event {
                        id: event.id,
                        msg: item.as_legacy_event(),
                    };
                    self.push_legacy_event(legacy);
                    return;
                }
            }
            _ => {}
        }

        self.push_legacy_event(event);
    }

    fn push_legacy_event(&mut self, event: Event) {
        if let EventMsg::UserMessage(_) = &event.msg
            && !event.id.is_empty()
            && !self.user_message_ids.insert(event.id.clone())
        {
            return;
        }
        self.buffer.push_back(event);
        if self.buffer.len() > self.capacity
            && let Some(removed) = self.buffer.pop_front()
            && matches!(removed.msg, EventMsg::UserMessage(_))
            && !removed.id.is_empty()
        {
            self.user_message_ids.remove(&removed.id);
        }
    }

    fn snapshot(&self) -> ThreadEventSnapshot {
        ThreadEventSnapshot {
            session_configured: self.session_configured.clone(),
            events: self.buffer.iter().cloned().collect(),
        }
    }
}

#[derive(Debug)]
struct ThreadEventChannel {
    sender: mpsc::Sender<Event>,
    receiver: Option<mpsc::Receiver<Event>>,
    store: Arc<Mutex<ThreadEventStore>>,
}

impl ThreadEventChannel {
    fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new(capacity))),
        }
    }

    fn new_with_session_configured(capacity: usize, event: Event) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new_with_session_configured(
                capacity, event,
            ))),
        }
    }
}

fn should_show_model_migration_prompt(
    current_model: &str,
    target_model: &str,
    seen_migrations: &BTreeMap<String, String>,
    available_models: &[ModelPreset],
) -> bool {
    if target_model == current_model {
        return false;
    }

    if let Some(seen_target) = seen_migrations.get(current_model)
        && seen_target == target_model
    {
        return false;
    }

    if available_models
        .iter()
        .any(|preset| preset.model == current_model && preset.upgrade.is_some())
    {
        return true;
    }

    if available_models
        .iter()
        .any(|preset| preset.upgrade.as_ref().map(|u| u.id.as_str()) == Some(target_model))
    {
        return true;
    }

    false
}

fn migration_prompt_hidden(config: &Config, migration_config_key: &str) -> bool {
    match migration_config_key {
        HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG => config
            .notices
            .hide_gpt_5_1_codex_max_migration_prompt
            .unwrap_or(false),
        HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG => {
            config.notices.hide_gpt5_1_migration_prompt.unwrap_or(false)
        }
        _ => false,
    }
}

fn target_preset_for_upgrade<'a>(
    available_models: &'a [ModelPreset],
    target_model: &str,
) -> Option<&'a ModelPreset> {
    available_models
        .iter()
        .find(|preset| preset.model == target_model)
}

async fn handle_model_migration_prompt_if_needed(
    tui: &mut tui::Tui,
    config: &mut Config,
    model: &str,
    app_event_tx: &AppEventSender,
    available_models: Vec<ModelPreset>,
) -> Option<AppExitInfo> {
    let upgrade = available_models
        .iter()
        .find(|preset| preset.model == model)
        .and_then(|preset| preset.upgrade.as_ref());

    if let Some(ModelUpgrade {
        id: target_model,
        reasoning_effort_mapping,
        migration_config_key,
        model_link,
        upgrade_copy,
        migration_markdown,
    }) = upgrade
    {
        if migration_prompt_hidden(config, migration_config_key.as_str()) {
            return None;
        }

        let target_model = target_model.to_string();
        if !should_show_model_migration_prompt(
            model,
            &target_model,
            &config.notices.model_migrations,
            &available_models,
        ) {
            return None;
        }

        let current_preset = available_models.iter().find(|preset| preset.model == model);
        let target_preset = target_preset_for_upgrade(&available_models, &target_model);
        let target_preset = target_preset?;
        let target_display_name = target_preset.display_name.clone();
        let heading_label = if target_display_name == model {
            target_model.clone()
        } else {
            target_display_name.clone()
        };
        let target_description =
            (!target_preset.description.is_empty()).then(|| target_preset.description.clone());
        let can_opt_out = current_preset.is_some();
        let prompt_copy = migration_copy_for_models(
            model,
            &target_model,
            model_link.clone(),
            upgrade_copy.clone(),
            migration_markdown.clone(),
            heading_label,
            target_description,
            can_opt_out,
        );
        match run_model_migration_prompt(tui, prompt_copy).await {
            ModelMigrationOutcome::Accepted => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });

                let mapped_effort = if let Some(reasoning_effort_mapping) = reasoning_effort_mapping
                    && let Some(reasoning_effort) = config.model_reasoning_effort
                {
                    reasoning_effort_mapping
                        .get(&reasoning_effort)
                        .cloned()
                        .or(config.model_reasoning_effort)
                } else {
                    config.model_reasoning_effort
                };

                config.model = Some(target_model.clone());
                config.model_reasoning_effort = mapped_effort;
                app_event_tx.send(AppEvent::UpdateModel(target_model.clone()));
                app_event_tx.send(AppEvent::UpdateReasoningEffort(mapped_effort));
                app_event_tx.send(AppEvent::PersistModelSelection {
                    model: target_model.clone(),
                    effort: mapped_effort,
                });
            }
            ModelMigrationOutcome::Rejected => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });
            }
            ModelMigrationOutcome::Exit => {
                return Some(AppExitInfo {
                    token_usage: TokenUsage::default(),
                    thread_id: None,
                    update_action: None,
                    exit_reason: ExitReason::UserRequested,
                });
            }
        }
    }

    None
}

pub(crate) struct App {
    pub(crate) server: Arc<ThreadManager>,
    pub(crate) otel_manager: OtelManager,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) chat_widget: ChatWidget,
    pub(crate) auth_manager: Arc<AuthManager>,
    /// Config is stored here so we can recreate ChatWidgets as needed.
    pub(crate) config: Config,
    pub(crate) active_profile: Option<String>,
    cli_kv_overrides: Vec<(String, TomlValue)>,
    harness_overrides: ConfigOverrides,
    runtime_approval_policy_override: Option<AskForApproval>,
    runtime_sandbox_policy_override: Option<SandboxPolicy>,

    pub(crate) file_search: FileSearchManager,

    pub(crate) transcript_cells: Vec<Arc<dyn HistoryCell>>,

    // Pager overlay state (Transcript or Static like Diff)
    pub(crate) overlay: Option<Overlay>,
    pub(crate) deferred_history_lines: Vec<Line<'static>>,
    has_emitted_history_lines: bool,

    pub(crate) enhanced_keys_supported: bool,

    /// Controls the animation thread that sends CommitTick events.
    pub(crate) commit_anim_running: Arc<AtomicBool>,

    // Esc-backtracking state grouped
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
    /// When set, the next draw re-renders the transcript into terminal scrollback once.
    ///
    /// This is used after a confirmed thread rollback to ensure scrollback reflects the trimmed
    /// transcript cells.
    pub(crate) backtrack_render_pending: bool,
    pub(crate) feedback: codex_feedback::CodexFeedback,
    /// Set when the user confirms an update; propagated on exit.
    pub(crate) pending_update_action: Option<UpdateAction>,
    pending_scrollback_clear: bool,

    windows_sandbox: WindowsSandboxState,

    thread_event_channels: HashMap<ThreadId, ThreadEventChannel>,
    thread_order: Vec<ThreadId>,
    thread_drafts: HashMap<ThreadId, ChatDraftState>,
    closed_thread_ids: HashSet<ThreadId>,
    user_agent_aliases: HashMap<ThreadId, String>,
    active_thread_id: Option<ThreadId>,
    active_thread_rx: Option<mpsc::Receiver<Event>>,
    primary_thread_id: Option<ThreadId>,
    primary_session_configured: Option<SessionConfiguredEvent>,
    pending_primary_events: VecDeque<Event>,
}

#[derive(Default)]
struct WindowsSandboxState {
    setup_started_at: Option<Instant>,
    // One-shot suppression of the next world-writable scan after user confirmation.
    skip_world_writable_scan_once: bool,
}

fn normalize_harness_overrides_for_cwd(
    mut overrides: ConfigOverrides,
    base_cwd: &Path,
) -> Result<ConfigOverrides> {
    if overrides.additional_writable_roots.is_empty() {
        return Ok(overrides);
    }

    let mut normalized = Vec::with_capacity(overrides.additional_writable_roots.len());
    for root in overrides.additional_writable_roots.drain(..) {
        let absolute = AbsolutePathBuf::resolve_path_against_base(root, base_cwd)?;
        normalized.push(absolute.into_path_buf());
    }
    overrides.additional_writable_roots = normalized;
    Ok(overrides)
}

impl App {
    pub fn chatwidget_init_for_forked_or_resumed_thread(
        &self,
        tui: &mut tui::Tui,
        cfg: codex_core::config::Config,
    ) -> crate::chatwidget::ChatWidgetInit {
        crate::chatwidget::ChatWidgetInit {
            config: cfg,
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.scoped(),
            // Fork/resume bootstraps here don't carry any prefilled message content.
            initial_user_message: None,
            enhanced_keys_supported: self.enhanced_keys_supported,
            auth_manager: self.auth_manager.clone(),
            models_manager: self.server.get_models_manager(),
            feedback: self.feedback.clone(),
            is_first_run: false,
            model: Some(self.chat_widget.current_model().to_string()),
            otel_manager: self.otel_manager.clone(),
        }
    }

    async fn rebuild_config_for_cwd(&self, cwd: PathBuf) -> Result<Config> {
        let mut overrides = self.harness_overrides.clone();
        overrides.cwd = Some(cwd.clone());
        let cwd_display = cwd.display().to_string();
        ConfigBuilder::default()
            .codex_home(self.config.codex_home.clone())
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(overrides)
            .build()
            .await
            .wrap_err_with(|| format!("Failed to rebuild config for cwd {cwd_display}"))
    }

    fn apply_runtime_policy_overrides(&mut self, config: &mut Config) {
        if let Some(policy) = self.runtime_approval_policy_override.as_ref()
            && let Err(err) = config.approval_policy.set(*policy)
        {
            tracing::warn!(%err, "failed to carry forward approval policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward approval policy override: {err}"
            ));
        }
        if let Some(policy) = self.runtime_sandbox_policy_override.as_ref()
            && let Err(err) = config.sandbox_policy.set(policy.clone())
        {
            tracing::warn!(%err, "failed to carry forward sandbox policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward sandbox policy override: {err}"
            ));
        }
    }

    async fn shutdown_current_thread(&mut self) {
        if let Some(thread_id) = self.chat_widget.thread_id() {
            // Clear any in-flight rollback guard when switching threads.
            self.backtrack.pending_rollback = None;
            self.closed_thread_ids.insert(thread_id);
            if let Err(err) = self.server.shutdown_agent(thread_id).await {
                tracing::warn!(error = %err, "failed to shut down thread {thread_id}");
            }
        }
    }

    async fn start_new_session_for_active_thread(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let summary = session_summary(self.chat_widget.token_usage(), self.chat_widget.thread_id());
        let active_thread_id = self
            .active_thread_id
            .or_else(|| self.chat_widget.thread_id());
        let Some(active_thread_id) = active_thread_id else {
            return Ok(());
        };
        let is_primary = self.primary_thread_id == Some(active_thread_id);

        if self.active_thread_id == Some(active_thread_id) {
            self.clear_active_thread().await;
        }
        self.shutdown_current_thread().await;
        self.remove_thread_state(active_thread_id);

        if is_primary {
            let model = self.chat_widget.current_model().to_string();
            let init = crate::chatwidget::ChatWidgetInit {
                config: self.config.clone(),
                frame_requester: tui.frame_requester(),
                app_event_tx: self.app_event_tx.scoped(),
                // New sessions start without prefilled message content.
                initial_user_message: None,
                enhanced_keys_supported: self.enhanced_keys_supported,
                auth_manager: self.auth_manager.clone(),
                models_manager: self.server.get_models_manager(),
                feedback: self.feedback.clone(),
                is_first_run: false,
                model: Some(model),
                otel_manager: self.otel_manager.clone(),
            };
            self.primary_thread_id = None;
            self.primary_session_configured = None;
            self.pending_primary_events.clear();
            self.chat_widget = ChatWidget::new(init, self.server.clone());
            self.update_agent_mentions();
            if let Some(summary) = summary {
                let mut lines: Vec<Line<'static>> = vec![summary.usage_line.clone().into()];
                if let Some(command) = summary.resume_command {
                    let spans = vec!["To continue this session, run ".into(), command.cyan()];
                    lines.push(spans.into());
                }
                self.chat_widget.add_plain_history_lines(lines);
            }
            tui.frame_requester().schedule_frame();
            return Ok(());
        }

        let config = self.chat_widget.config_ref().clone();
        let new_thread_id = match self
            .server
            .spawn_agent_idle(config, Some(SessionSource::SubAgent(SubAgentSource::User)))
            .await
        {
            Ok(thread_id) => thread_id,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to start new session: {err}"));
                return Ok(());
            }
        };
        self.handle_thread_created(new_thread_id).await?;
        self.select_agent_thread(tui, new_thread_id).await?;
        if let Some(summary) = summary {
            let mut lines: Vec<Line<'static>> = vec![summary.usage_line.clone().into()];
            if let Some(command) = summary.resume_command {
                let spans = vec!["To continue this session, run ".into(), command.cyan()];
                lines.push(spans.into());
            }
            self.chat_widget.add_plain_history_lines(lines);
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    fn register_thread_order(&mut self, thread_id: ThreadId) {
        if !self.thread_order.contains(&thread_id) {
            self.thread_order.push(thread_id);
        }
    }

    fn ensure_thread_channel(&mut self, thread_id: ThreadId) -> &mut ThreadEventChannel {
        self.register_thread_order(thread_id);
        self.thread_event_channels
            .entry(thread_id)
            .or_insert_with(|| ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY))
    }

    async fn set_thread_active(&mut self, thread_id: ThreadId, active: bool) {
        if let Some(channel) = self.thread_event_channels.get_mut(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active = active;
        }
    }

    async fn activate_thread_channel(&mut self, thread_id: ThreadId) {
        if self.active_thread_id.is_some() {
            return;
        }
        self.set_thread_active(thread_id, true).await;
        let receiver = if let Some(channel) = self.thread_event_channels.get_mut(&thread_id) {
            channel.receiver.take()
        } else {
            None
        };
        self.active_thread_id = Some(thread_id);
        self.active_thread_rx = receiver;
    }

    async fn store_active_thread_receiver(&mut self) {
        let Some(active_id) = self.active_thread_id else {
            return;
        };
        let Some(receiver) = self.active_thread_rx.take() else {
            return;
        };
        if let Some(channel) = self.thread_event_channels.get_mut(&active_id) {
            let mut store = channel.store.lock().await;
            store.active = false;
            channel.receiver = Some(receiver);
        }
    }

    async fn activate_thread_for_replay(
        &mut self,
        thread_id: ThreadId,
    ) -> Option<(mpsc::Receiver<Event>, ThreadEventSnapshot)> {
        let channel = self.thread_event_channels.get_mut(&thread_id)?;
        let mut receiver = channel.receiver.take()?;
        while receiver.try_recv().is_ok() {}
        let mut store = channel.store.lock().await;
        store.active = true;
        let snapshot = store.snapshot();
        Some((receiver, snapshot))
    }

    async fn clear_active_thread(&mut self) {
        if let Some(active_id) = self.active_thread_id.take() {
            self.set_thread_active(active_id, false).await;
        }
        self.active_thread_rx = None;
    }

    async fn enqueue_thread_event(&mut self, thread_id: ThreadId, event: Event) -> Result<()> {
        let (sender, store) = {
            let channel = self.ensure_thread_channel(thread_id);
            (channel.sender.clone(), Arc::clone(&channel.store))
        };

        let should_send = {
            let mut guard = store.lock().await;
            guard.push_event(event.clone());
            guard.active
        };

        if should_send && let Err(err) = sender.send(event).await {
            tracing::warn!("thread {thread_id} event channel closed: {err}");
        }
        Ok(())
    }

    async fn enqueue_primary_event(&mut self, event: Event) -> Result<()> {
        if let Some(thread_id) = self.primary_thread_id {
            return self.enqueue_thread_event(thread_id, event).await;
        }

        if let EventMsg::SessionConfigured(session) = &event.msg {
            let thread_id = session.session_id;
            self.primary_thread_id = Some(thread_id);
            self.primary_session_configured = Some(session.clone());
            self.ensure_thread_channel(thread_id);
            self.activate_thread_channel(thread_id).await;

            let pending = std::mem::take(&mut self.pending_primary_events);
            for pending_event in pending {
                self.enqueue_thread_event(thread_id, pending_event).await?;
            }
            self.enqueue_thread_event(thread_id, event).await?;
        } else {
            self.pending_primary_events.push_back(event);
        }
        Ok(())
    }

    fn alias_for(&self, id: ThreadId) -> Option<&str> {
        self.user_agent_aliases.get(&id).map(String::as_str)
    }

    fn validate_alias(&self, alias: &str, id: Option<ThreadId>) -> Result<String, String> {
        let trimmed = alias.trim();
        if trimmed.is_empty() {
            return Err("Agent alias cannot be empty.".to_string());
        }
        if trimmed.eq_ignore_ascii_case("main") {
            return Err("Alias 'Main' is reserved for the primary agent.".to_string());
        }
        if self
            .user_agent_aliases
            .iter()
            .any(|(other_id, alias)| Some(*other_id) != id && alias.eq_ignore_ascii_case(trimmed))
        {
            return Err(format!("Alias '{trimmed}' is already in use."));
        }
        if self
            .thread_event_channels
            .keys()
            .any(|thread_id| Self::short_thread_id(*thread_id).eq_ignore_ascii_case(trimmed))
        {
            return Err(format!(
                "Alias '{trimmed}' conflicts with an existing agent id prefix."
            ));
        }
        Ok(trimmed.to_string())
    }

    fn set_alias(&mut self, id: ThreadId, name: String) -> Result<(), String> {
        let trimmed = self.validate_alias(&name, Some(id))?;
        self.user_agent_aliases.insert(id, trimmed);
        self.update_agent_mentions();
        Ok(())
    }

    fn short_thread_id(thread_id: ThreadId) -> String {
        let id = thread_id.to_string();
        id.get(..THREAD_ID_PREFIX_LEN).unwrap_or(&id).to_string()
    }

    fn thread_label(&self, thread_id: ThreadId) -> String {
        if let Some(alias) = self.alias_for(thread_id) {
            return alias.to_string();
        }
        if Some(thread_id) == self.main_thread_id() {
            return "Main".to_string();
        }
        Self::short_thread_id(thread_id)
    }

    fn agent_mention_aliases(&self) -> Vec<String> {
        let active_thread_id = self
            .active_thread_id
            .or_else(|| self.chat_widget.thread_id());
        let mut aliases: Vec<String> = self
            .user_agent_aliases
            .iter()
            .filter_map(|(id, alias)| {
                if Some(*id) == active_thread_id {
                    None
                } else {
                    Some(alias.clone())
                }
            })
            .collect();
        if let Some(main_thread_id) = self.main_thread_id()
            && Some(main_thread_id) != active_thread_id
            && !aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case("main"))
        {
            aliases.push("Main".to_string());
        }
        aliases.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        aliases
    }

    fn agent_mention_map(&self) -> HashMap<String, ThreadId> {
        let mut map = HashMap::new();
        if let Some(thread_id) = self.main_thread_id() {
            map.insert("main".to_string(), thread_id);
        }
        for (id, alias) in &self.user_agent_aliases {
            map.insert(alias.to_lowercase(), *id);
        }
        map
    }

    fn update_agent_mentions(&mut self) {
        let aliases = self.agent_mention_aliases();
        let mention_map = self.agent_mention_map();
        self.chat_widget.set_agent_mentions(aliases);
        self.chat_widget.set_agent_mention_map(mention_map);
    }

    fn ordered_thread_ids(&self) -> Vec<ThreadId> {
        let mut thread_ids = Vec::new();
        let main_thread_id = self.main_thread_id();
        if let Some(main_thread_id) = main_thread_id
            && self.thread_event_channels.contains_key(&main_thread_id)
        {
            thread_ids.push(main_thread_id);
        }

        for thread_id in &self.thread_order {
            if Some(*thread_id) == main_thread_id {
                continue;
            }
            if self.thread_event_channels.contains_key(thread_id) {
                thread_ids.push(*thread_id);
            }
        }

        if thread_ids.len() != self.thread_event_channels.len() {
            for thread_id in self.thread_event_channels.keys() {
                if !thread_ids.contains(thread_id) {
                    thread_ids.push(*thread_id);
                }
            }
        }

        thread_ids
    }

    fn main_thread_id(&self) -> Option<ThreadId> {
        self.primary_thread_id.or(self.chat_widget.thread_id())
    }

    fn agent_tabs_line(&self) -> Option<Line<'static>> {
        let thread_ids = self.ordered_thread_ids();
        if thread_ids.is_empty() {
            return None;
        }
        let active_thread_id = self
            .active_thread_id
            .or_else(|| self.chat_widget.thread_id());
        let mut spans = Vec::new();
        for (idx, thread_id) in thread_ids.iter().enumerate() {
            if idx > 0 {
                spans.push(" | ".into());
            }
            let label = self.thread_label(*thread_id);
            let span = if Some(*thread_id) == active_thread_id {
                label.cyan().bold()
            } else {
                label.into()
            };
            spans.push(span);
        }
        Some(Line::from(spans))
    }

    fn can_cycle_agent_tabs(&self) -> bool {
        self.thread_event_channels.len() > 1
            && self.chat_widget.composer_is_empty()
            && self.chat_widget.no_modal_or_popup_active()
    }

    async fn cycle_active_thread(&mut self, tui: &mut tui::Tui) -> Result<()> {
        if !self.can_cycle_agent_tabs() {
            return Ok(());
        }
        let Some(active_thread_id) = self.active_thread_id else {
            return Ok(());
        };
        let thread_ids = self.ordered_thread_ids();
        if thread_ids.len() < 2 {
            return Ok(());
        }
        let next_idx = thread_ids
            .iter()
            .position(|thread_id| *thread_id == active_thread_id)
            .map(|idx| (idx + 1) % thread_ids.len())
            .unwrap_or(0);
        let next_thread_id = thread_ids[next_idx];
        self.select_agent_thread(tui, next_thread_id).await
    }

    fn set_footer_tabs(&mut self) {
        let label = self.agent_tabs_line();
        self.chat_widget.set_footer_right_label(label);
    }

    fn open_agent_picker(&mut self) {
        if self.thread_event_channels.is_empty() {
            self.chat_widget
                .add_info_message("No agents available yet.".to_string(), None);
            return;
        }

        let mut items = Vec::new();
        items.push(SelectionItem {
            name: "New agent".to_string(),
            description: Some("Create an agent and switch to it.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::OpenNewAgentPrompt);
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        items.push(SelectionItem {
            name: "Close agent".to_string(),
            description: Some("Shut down an agent.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::OpenCloseAgentPicker);
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        items.push(SelectionItem {
            name: String::new(),
            is_heading: true,
            ..Default::default()
        });

        let thread_ids = self.ordered_thread_ids();
        items.extend(thread_ids.iter().map(|thread_id| {
            let id = *thread_id;
            let base_label = self.thread_label(id);
            let is_current = self.active_thread_id == Some(id);
            let label = if is_current {
                format!("{base_label} (current)")
            } else {
                base_label.clone()
            };
            let description = self
                .alias_for(id)
                .map(|_| format!("Thread {id}"))
                .or_else(|| Some(id.to_string()));
            SelectionItem {
                name: label.clone(),
                description,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::SelectAgentThread(id));
                })],
                dismiss_on_select: true,
                search_value: Some(format!("{base_label} {id}")),
                ..Default::default()
            }
        }));

        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Agents".to_string()),
            subtitle: Some("Create, close, or select an agent.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            show_indices: false,
            initial_selected_idx: Some(0),
            ..Default::default()
        });
    }

    fn open_close_agent_picker(&mut self) {
        if self.thread_event_channels.is_empty() {
            self.chat_widget
                .add_info_message("No agents available yet.".to_string(), None);
            return;
        }

        let main_thread_id = self.main_thread_id();
        let mut initial_selected_idx = None;
        let thread_ids = self.ordered_thread_ids();
        let items: Vec<SelectionItem> = thread_ids
            .iter()
            .enumerate()
            .map(|(idx, thread_id)| {
                if self.active_thread_id == Some(*thread_id) {
                    initial_selected_idx = Some(idx);
                }
                let id = *thread_id;
                let label = self.thread_label(id);
                let disabled_reason = (Some(id) == main_thread_id)
                    .then_some("Main agent can't be closed; use /quit.".to_string());
                SelectionItem {
                    name: label.clone(),
                    description: Some(id.to_string()),
                    is_current: self.active_thread_id == Some(id),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::CloseAgentThread(id));
                    })],
                    dismiss_on_select: true,
                    search_value: Some(format!("{label} {id}")),
                    disabled_reason,
                    ..Default::default()
                }
            })
            .collect();

        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Close agent".to_string()),
            subtitle: Some("Select an agent to shut down".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    async fn create_idle_agent(&mut self, tui: &mut tui::Tui, alias: String) -> Result<()> {
        let alias = match self.validate_alias(&alias, None) {
            Ok(alias) => alias,
            Err(err) => {
                self.chat_widget.add_error_message(err);
                return Ok(());
            }
        };
        let config = self.chat_widget.config_ref().clone();
        let thread_id = match self
            .server
            .spawn_agent_idle(config, Some(SessionSource::SubAgent(SubAgentSource::User)))
            .await
        {
            Ok(thread_id) => thread_id,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to spawn agent: {err}"));
                return Ok(());
            }
        };
        self.handle_thread_created(thread_id).await?;
        if let Err(err) = self.set_alias(thread_id, alias) {
            self.chat_widget.add_error_message(err);
        }
        self.select_agent_thread(tui, thread_id).await?;
        Ok(())
    }

    async fn close_agent_thread(&mut self, tui: &mut tui::Tui, thread_id: ThreadId) -> Result<()> {
        if Some(thread_id) == self.main_thread_id() {
            self.chat_widget
                .add_info_message("Main agent can't be closed; use /quit.".to_string(), None);
            return Ok(());
        }
        if !self.thread_event_channels.contains_key(&thread_id) {
            self.chat_widget
                .add_error_message(format!("Agent thread {thread_id} is no longer available."));
            return Ok(());
        }

        let thread_ids = self.ordered_thread_ids();
        let fallback_thread_id =
            thread_ids
                .iter()
                .position(|id| *id == thread_id)
                .and_then(|idx| {
                    if idx > 0 {
                        thread_ids.get(idx - 1).copied()
                    } else {
                        thread_ids.iter().find(|id| **id != thread_id).copied()
                    }
                });

        let was_active = self.active_thread_id == Some(thread_id);
        if let Err(err) = self.server.shutdown_agent(thread_id).await {
            self.chat_widget
                .add_error_message(format!("Failed to close agent {thread_id}: {err}"));
            return Ok(());
        }
        if was_active {
            self.clear_active_thread().await;
        }
        self.remove_thread_state(thread_id);

        if was_active {
            if let Some(next_thread_id) = fallback_thread_id {
                self.select_agent_thread(tui, next_thread_id).await?;
            }
        }
        Ok(())
    }

    async fn select_agent_thread(&mut self, tui: &mut tui::Tui, thread_id: ThreadId) -> Result<()> {
        if self.active_thread_id == Some(thread_id) {
            return Ok(());
        }

        let thread = match self.server.get_thread(thread_id).await {
            Ok(thread) => thread,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to attach to agent thread {thread_id}: {err}"
                ));
                return Ok(());
            }
        };

        let previous_thread_id = self.active_thread_id;
        self.store_active_thread_receiver().await;
        self.active_thread_id = None;
        let Some((receiver, snapshot)) = self.activate_thread_for_replay(thread_id).await else {
            self.chat_widget
                .add_error_message(format!("Agent thread {thread_id} is already active."));
            if let Some(previous_thread_id) = previous_thread_id {
                self.activate_thread_channel(previous_thread_id).await;
            }
            return Ok(());
        };

        if let Some(previous_thread_id) = previous_thread_id {
            let draft = self.chat_widget.take_draft_state();
            self.thread_drafts.insert(previous_thread_id, draft);
        }

        self.active_thread_id = Some(thread_id);
        self.active_thread_rx = Some(receiver);

        let init = self.chatwidget_init_for_forked_or_resumed_thread(tui, self.config.clone());
        let codex_op_tx = crate::chatwidget::spawn_op_forwarder(thread);
        self.chat_widget = ChatWidget::new_with_op_sender(init, codex_op_tx);
        self.update_agent_mentions();

        self.reset_for_thread_switch(tui)?;
        self.replay_thread_snapshot(snapshot);
        self.drain_active_thread_events(tui).await?;
        if let Some(draft) = self.thread_drafts.remove(&thread_id) {
            self.chat_widget.restore_draft_state(draft);
        }

        Ok(())
    }

    fn reset_for_thread_switch(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.overlay = None;
        self.transcript_cells.clear();
        self.deferred_history_lines.clear();
        self.has_emitted_history_lines = false;
        self.backtrack = BacktrackState::default();
        self.backtrack_render_pending = false;
        tui.clear_pending_history_lines();
        self.pending_scrollback_clear = true;
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    fn reset_thread_event_state(&mut self) {
        self.thread_event_channels.clear();
        self.thread_order.clear();
        self.thread_drafts.clear();
        self.closed_thread_ids.clear();
        self.user_agent_aliases.clear();
        self.active_thread_id = None;
        self.active_thread_rx = None;
        self.primary_thread_id = None;
        self.pending_primary_events.clear();
        self.pending_scrollback_clear = false;
        self.update_agent_mentions();
    }

    fn remove_thread_state(&mut self, thread_id: ThreadId) {
        self.thread_event_channels.remove(&thread_id);
        self.closed_thread_ids.insert(thread_id);
        self.thread_order.retain(|id| *id != thread_id);
        self.thread_drafts.remove(&thread_id);
        self.user_agent_aliases.remove(&thread_id);
        self.update_agent_mentions();
    }

    async fn drain_active_thread_events(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let Some(mut rx) = self.active_thread_rx.take() else {
            return Ok(());
        };

        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(event) => self.handle_codex_event_now(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if !disconnected {
            self.active_thread_rx = Some(rx);
        } else {
            self.clear_active_thread().await;
        }

        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    fn replay_thread_snapshot(&mut self, snapshot: ThreadEventSnapshot) {
        if let Some(event) = snapshot.session_configured {
            self.handle_codex_event_replay(event);
        }
        for event in snapshot.events {
            self.handle_codex_event_replay(event);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        auth_manager: Arc<AuthManager>,
        mut config: Config,
        cli_kv_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
        active_profile: Option<String>,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        session_selection: SessionSelection,
        feedback: codex_feedback::CodexFeedback,
        is_first_run: bool,
        ollama_chat_support_notice: Option<DeprecationNoticeEvent>,
    ) -> Result<AppExitInfo> {
        use tokio_stream::StreamExt;
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);
        emit_deprecation_notice(&app_event_tx, ollama_chat_support_notice);
        emit_project_config_warnings(&app_event_tx, &config);

        let harness_overrides =
            normalize_harness_overrides_for_cwd(harness_overrides, &config.cwd)?;
        let thread_manager = Arc::new(ThreadManager::new(
            config.codex_home.clone(),
            auth_manager.clone(),
            SessionSource::Cli,
        ));
        let mut model = thread_manager
            .get_models_manager()
            .get_default_model(&config.model, &config, RefreshStrategy::Offline)
            .await;
        let available_models = thread_manager
            .get_models_manager()
            .list_models(&config, RefreshStrategy::Offline)
            .await;
        let exit_info = handle_model_migration_prompt_if_needed(
            tui,
            &mut config,
            model.as_str(),
            &app_event_tx,
            available_models,
        )
        .await;
        if let Some(exit_info) = exit_info {
            return Ok(exit_info);
        }
        if let Some(updated_model) = config.model.clone() {
            model = updated_model;
        }

        let auth = auth_manager.auth().await;
        let auth_ref = auth.as_ref();
        let model_info = thread_manager
            .get_models_manager()
            .get_model_info(model.as_str(), &config)
            .await;
        let otel_manager = OtelManager::new(
            ThreadId::new(),
            model.as_str(),
            model_info.slug.as_str(),
            auth_ref.and_then(CodexAuth::get_account_id),
            auth_ref.and_then(CodexAuth::get_account_email),
            auth_ref.map(|auth| auth.mode),
            config.otel.log_user_prompt,
            codex_core::terminal::user_agent(),
            SessionSource::Cli,
        );

        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let mut chat_widget = match session_selection {
            SessionSelection::StartFresh | SessionSelection::Exit => {
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.scoped(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: thread_manager.get_models_manager(),
                    feedback: feedback.clone(),
                    is_first_run,
                    model: Some(model.clone()),
                    otel_manager: otel_manager.clone(),
                };
                ChatWidget::new(init, thread_manager.clone())
            }
            SessionSelection::Resume(path) => {
                let resumed = thread_manager
                    .resume_thread_from_rollout(config.clone(), path.clone(), auth_manager.clone())
                    .await
                    .wrap_err_with(|| {
                        let path_display = path.display();
                        format!("Failed to resume session from {path_display}")
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.scoped(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: thread_manager.get_models_manager(),
                    feedback: feedback.clone(),
                    is_first_run,
                    model: config.model.clone(),
                    otel_manager: otel_manager.clone(),
                };
                ChatWidget::new_from_existing(init, resumed.thread, resumed.session_configured)
            }
            SessionSelection::Fork(path) => {
                let forked = thread_manager
                    .fork_thread(usize::MAX, config.clone(), path.clone())
                    .await
                    .wrap_err_with(|| {
                        let path_display = path.display();
                        format!("Failed to fork session from {path_display}")
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.scoped(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: thread_manager.get_models_manager(),
                    feedback: feedback.clone(),
                    is_first_run,
                    model: config.model.clone(),
                    otel_manager: otel_manager.clone(),
                };
                ChatWidget::new_from_existing(init, forked.thread, forked.session_configured)
            }
        };

        chat_widget.maybe_prompt_windows_sandbox_enable();

        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        #[cfg(not(debug_assertions))]
        let upgrade_version = crate::updates::get_upgrade_version(&config);

        let mut app = Self {
            server: thread_manager.clone(),
            otel_manager: otel_manager.clone(),
            app_event_tx,
            chat_widget,
            auth_manager: auth_manager.clone(),
            config,
            active_profile,
            cli_kv_overrides,
            harness_overrides,
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            file_search,
            enhanced_keys_supported,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: feedback.clone(),
            pending_update_action: None,
            pending_scrollback_clear: false,
            windows_sandbox: WindowsSandboxState::default(),
            thread_event_channels: HashMap::new(),
            thread_order: Vec::new(),
            thread_drafts: HashMap::new(),
            closed_thread_ids: HashSet::new(),
            user_agent_aliases: HashMap::new(),
            active_thread_id: None,
            active_thread_rx: None,
            primary_thread_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
        };

        // On startup, if Agent mode (workspace-write) or ReadOnly is active, warn about world-writable dirs on Windows.
        #[cfg(target_os = "windows")]
        {
            let should_check = codex_core::get_platform_sandbox().is_some()
                && matches!(
                    app.config.sandbox_policy.get(),
                    codex_core::protocol::SandboxPolicy::WorkspaceWrite { .. }
                        | codex_core::protocol::SandboxPolicy::ReadOnly
                )
                && !app
                    .config
                    .notices
                    .hide_world_writable_warning
                    .unwrap_or(false);
            if should_check {
                let cwd = app.config.cwd.clone();
                let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
                let tx = app.app_event_tx.clone();
                let logs_base_dir = app.config.codex_home.clone();
                let sandbox_policy = app.config.sandbox_policy.get().clone();
                Self::spawn_world_writable_scan(cwd, env_map, logs_base_dir, sandbox_policy, tx);
            }
        }

        #[cfg(not(debug_assertions))]
        if let Some(latest_version) = upgrade_version {
            let control = app
                .handle_event(
                    tui,
                    AppEvent::InsertHistoryCell(Box::new(UpdateAvailableHistoryCell::new(
                        latest_version,
                        crate::update_action::get_update_action(),
                    ))),
                )
                .await?;
            if let AppRunControl::Exit(exit_reason) = control {
                return Ok(AppExitInfo {
                    token_usage: app.token_usage(),
                    thread_id: app.chat_widget.thread_id(),
                    update_action: app.pending_update_action,
                    exit_reason,
                });
            }
        }

        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        tui.frame_requester().schedule_frame();

        let mut thread_created_rx = thread_manager.subscribe_thread_created();
        let mut listen_for_threads = true;

        let exit_reason = loop {
            let control = select! {
                Some(event) = app_event_rx.recv() => {
                    app.handle_event(tui, event).await?
                }
                active = async {
                    if let Some(rx) = app.active_thread_rx.as_mut() {
                        rx.recv().await
                    } else {
                        None
                    }
                }, if app.active_thread_rx.is_some() => {
                    if let Some(event) = active {
                        app.handle_active_thread_event(tui, event)?;
                    } else {
                        app.clear_active_thread().await;
                    }
                    AppRunControl::Continue
                }
                Some(event) = tui_events.next() => {
                    app.handle_tui_event(tui, event).await?
                }
                // Listen on new thread creation due to collab tools.
                created = thread_created_rx.recv(), if listen_for_threads => {
                    match created {
                        Ok(thread_id) => {
                            app.handle_thread_created(thread_id).await?;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            tracing::warn!("thread_created receiver lagged; skipping resync");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            listen_for_threads = false;
                        }
                    }
                    AppRunControl::Continue
                }
            };
            match control {
                AppRunControl::Continue => {}
                AppRunControl::Exit(reason) => break reason,
            }
        };
        tui.terminal.clear()?;
        Ok(AppExitInfo {
            token_usage: app.token_usage(),
            thread_id: app.chat_widget.thread_id(),
            update_action: app.pending_update_action,
            exit_reason,
        })
    }

    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<AppRunControl> {
        if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, key_event).await;
                }
                TuiEvent::Paste(pasted) => {
                    // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                    // but tui-textarea expects \n. Normalize CR to LF.
                    // [tui-textarea]: https://github.com/rhysd/tui-textarea/blob/4d18622eeac13b309e0ff6a55a46ac6706da68cf/src/textarea.rs#L782-L783
                    // [iTerm2]: https://github.com/gnachman/iTerm2/blob/5d0c0d9f68523cbd0494dad5422998964a2ecd8d/sources/iTermPasteHelper.m#L206-L216
                    let pasted = pasted.replace("\r", "\n");
                    self.chat_widget.handle_paste(pasted);
                }
                TuiEvent::Draw => {
                    if self.pending_scrollback_clear {
                        self.pending_scrollback_clear = false;
                        if let Err(err) = tui.terminal.clear_scrollback() {
                            tracing::warn!("failed to clear scrollback: {err}");
                        }
                        if let Err(err) = tui.terminal.clear_screen() {
                            tracing::warn!("failed to clear screen: {err}");
                        }
                    }
                    if self.backtrack_render_pending {
                        self.backtrack_render_pending = false;
                        self.render_transcript_once(tui);
                    }
                    self.chat_widget.maybe_post_pending_notification(tui);
                    if self
                        .chat_widget
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(AppRunControl::Continue);
                    }
                    let width = tui.terminal.size()?.width;
                    let chat_height = self.chat_widget.desired_height(width);
                    let total_height = chat_height;
                    self.set_footer_tabs();
                    tui.draw(total_height, |frame| {
                        let area = frame.area();
                        self.chat_widget.render(area, frame.buffer);
                        if let Some((x, y)) = self.chat_widget.cursor_pos(area) {
                            frame.set_cursor_position((x, y));
                        }
                    })?;
                    if self.chat_widget.external_editor_state() == ExternalEditorState::Requested {
                        self.chat_widget
                            .set_external_editor_state(ExternalEditorState::Active);
                        self.app_event_tx.send(AppEvent::LaunchExternalEditor);
                    }
                }
            }
        }
        Ok(AppRunControl::Continue)
    }

    async fn handle_event(&mut self, tui: &mut tui::Tui, event: AppEvent) -> Result<AppRunControl> {
        match event {
            AppEvent::NewSession => {
                self.start_new_session_for_active_thread(tui).await?;
            }
            AppEvent::OpenResumePicker => {
                match crate::resume_picker::run_resume_picker(
                    tui,
                    &self.config.codex_home,
                    &self.config.model_provider_id,
                    false,
                )
                .await?
                {
                    SessionSelection::Resume(path) => {
                        let current_cwd = self.config.cwd.clone();
                        let resume_cwd = match crate::resolve_cwd_for_resume_or_fork(
                            tui,
                            &current_cwd,
                            &path,
                            CwdPromptAction::Resume,
                            true,
                        )
                        .await?
                        {
                            Some(cwd) => cwd,
                            None => current_cwd.clone(),
                        };
                        let mut resume_config = if crate::cwds_differ(&current_cwd, &resume_cwd) {
                            match self.rebuild_config_for_cwd(resume_cwd).await {
                                Ok(cfg) => cfg,
                                Err(err) => {
                                    self.chat_widget.add_error_message(format!(
                                        "Failed to rebuild configuration for resume: {err}"
                                    ));
                                    return Ok(AppRunControl::Continue);
                                }
                            }
                        } else {
                            // No rebuild needed: current_cwd comes from self.config.cwd.
                            self.config.clone()
                        };
                        self.apply_runtime_policy_overrides(&mut resume_config);
                        let summary = session_summary(
                            self.chat_widget.token_usage(),
                            self.chat_widget.thread_id(),
                        );
                        match self
                            .server
                            .resume_thread_from_rollout(
                                resume_config.clone(),
                                path.clone(),
                                self.auth_manager.clone(),
                            )
                            .await
                        {
                            Ok(resumed) => {
                                self.shutdown_current_thread().await;
                                self.config = resume_config;
                                self.file_search = FileSearchManager::new(
                                    self.config.cwd.clone(),
                                    self.app_event_tx.clone(),
                                );
                                let init = self.chatwidget_init_for_forked_or_resumed_thread(
                                    tui,
                                    self.config.clone(),
                                );
                                self.chat_widget = ChatWidget::new_from_existing(
                                    init,
                                    resumed.thread,
                                    resumed.session_configured,
                                );
                                self.reset_thread_event_state();
                                if let Some(summary) = summary {
                                    let mut lines: Vec<Line<'static>> =
                                        vec![summary.usage_line.clone().into()];
                                    if let Some(command) = summary.resume_command {
                                        let spans = vec![
                                            "To continue this session, run ".into(),
                                            command.cyan(),
                                        ];
                                        lines.push(spans.into());
                                    }
                                    self.chat_widget.add_plain_history_lines(lines);
                                }
                            }
                            Err(err) => {
                                let path_display = path.display();
                                self.chat_widget.add_error_message(format!(
                                    "Failed to resume session from {path_display}: {err}"
                                ));
                            }
                        }
                    }
                    SessionSelection::Exit
                    | SessionSelection::StartFresh
                    | SessionSelection::Fork(_) => {}
                }

                // Leaving alt-screen may blank the inline viewport; force a redraw either way.
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ForkCurrentSession => {
                let summary =
                    session_summary(self.chat_widget.token_usage(), self.chat_widget.thread_id());
                if let Some(path) = self.chat_widget.rollout_path() {
                    match self
                        .server
                        .fork_thread(usize::MAX, self.config.clone(), path.clone())
                        .await
                    {
                        Ok(forked) => {
                            self.shutdown_current_thread().await;
                            let init = self.chatwidget_init_for_forked_or_resumed_thread(
                                tui,
                                self.config.clone(),
                            );
                            self.chat_widget = ChatWidget::new_from_existing(
                                init,
                                forked.thread,
                                forked.session_configured,
                            );
                            self.reset_thread_event_state();
                            if let Some(summary) = summary {
                                let mut lines: Vec<Line<'static>> =
                                    vec![summary.usage_line.clone().into()];
                                if let Some(command) = summary.resume_command {
                                    let spans = vec![
                                        "To continue this session, run ".into(),
                                        command.cyan(),
                                    ];
                                    lines.push(spans.into());
                                }
                                self.chat_widget.add_plain_history_lines(lines);
                            }
                        }
                        Err(err) => {
                            let path_display = path.display();
                            self.chat_widget.add_error_message(format!(
                                "Failed to fork current session from {path_display}: {err}"
                            ));
                        }
                    }
                } else {
                    self.chat_widget
                        .add_error_message("Current session is not ready to fork yet.".to_string());
                }

                tui.frame_requester().schedule_frame();
            }
            AppEvent::InsertHistoryCell(cell) => {
                self.handle_history_cell(tui, None, cell)?;
            }
            AppEvent::InsertHistoryCellForThread { thread_id, cell } => {
                self.handle_history_cell(tui, thread_id, cell)?;
            }
            AppEvent::StartCommitAnimation => {
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(50));
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            AppEvent::CommitTick => {
                self.chat_widget.on_commit_tick();
            }
            AppEvent::CodexEvent(event) => {
                self.enqueue_primary_event(event).await?;
            }
            AppEvent::CodexEventForThread { thread_id, event } => {
                if self.closed_thread_ids.contains(&thread_id) {
                    return Ok(AppRunControl::Continue);
                }
                if let EventMsg::SessionConfigured(session) = &event.msg {
                    if self.primary_thread_id.is_none() {
                        self.primary_thread_id = Some(thread_id);
                    }
                    if self.primary_thread_id == Some(thread_id) {
                        self.primary_session_configured = Some(session.clone());
                    }
                    if self.active_thread_id.is_none() {
                        self.ensure_thread_channel(thread_id);
                        self.activate_thread_channel(thread_id).await;
                    }
                    self.update_agent_mentions();
                }
                self.enqueue_thread_event(thread_id, event).await?;
            }
            AppEvent::Exit(mode) => match mode {
                ExitMode::ShutdownFirst => {
                    if let Err(err) = self.server.remove_and_close_all_threads().await {
                        tracing::warn!("failed to shut down threads on quit: {err}");
                    }
                }
                ExitMode::Immediate => {
                    return Ok(AppRunControl::Exit(ExitReason::UserRequested));
                }
            },
            AppEvent::FatalExitRequest(message) => {
                return Ok(AppRunControl::Exit(ExitReason::Fatal(message)));
            }
            AppEvent::CodexOp(op) => {
                self.chat_widget.submit_op(op);
            }
            AppEvent::DiffResult(text) => {
                // Clear the in-progress state in the bottom pane
                self.chat_widget.on_diff_complete();
                // Enter alternate screen using TUI helper and build pager lines
                let _ = tui.enter_alt_screen();
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                self.overlay = Some(Overlay::new_static_with_lines(
                    pager_lines,
                    "D I F F".to_string(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::StartFileSearch(query) => {
                if !query.is_empty() {
                    self.file_search.on_user_query(query);
                }
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_widget.apply_file_search_result(query, matches);
            }
            AppEvent::RateLimitSnapshotFetched(snapshot) => {
                self.chat_widget.on_rate_limit_snapshot(Some(snapshot));
            }
            AppEvent::UpdateReasoningEffort(effort) => {
                self.on_update_reasoning_effort(effort);
            }
            AppEvent::UpdateModel(model) => {
                self.chat_widget.set_model(&model);
            }
            AppEvent::UpdateCollaborationMode(mask) => {
                self.chat_widget.set_collaboration_mask(mask);
            }
            AppEvent::OpenReasoningPopup { model } => {
                self.chat_widget.open_reasoning_popup(model);
            }
            AppEvent::OpenAllModelsPopup { models } => {
                self.chat_widget.open_all_models_popup(models);
            }
            AppEvent::OpenFullAccessConfirmation {
                preset,
                return_to_permissions,
            } => {
                self.chat_widget
                    .open_full_access_confirmation(preset, return_to_permissions);
            }
            AppEvent::OpenWorldWritableWarningConfirmation {
                preset,
                sample_paths,
                extra_count,
                failed_scan,
            } => {
                self.chat_widget.open_world_writable_warning_confirmation(
                    preset,
                    sample_paths,
                    extra_count,
                    failed_scan,
                );
            }
            AppEvent::OpenFeedbackNote {
                category,
                include_logs,
            } => {
                self.chat_widget.open_feedback_note(category, include_logs);
            }
            AppEvent::OpenFeedbackConsent { category } => {
                self.chat_widget.open_feedback_consent(category);
            }
            AppEvent::LaunchExternalEditor => {
                if self.chat_widget.external_editor_state() == ExternalEditorState::Active {
                    self.launch_external_editor(tui).await;
                }
            }
            AppEvent::OpenWindowsSandboxEnablePrompt { preset } => {
                self.chat_widget.open_windows_sandbox_enable_prompt(preset);
            }
            AppEvent::OpenWindowsSandboxFallbackPrompt { preset, reason } => {
                self.otel_manager
                    .counter("codex.windows_sandbox.fallback_prompt_shown", 1, &[]);
                self.chat_widget.clear_windows_sandbox_setup_status();
                if let Some(started_at) = self.windows_sandbox.setup_started_at.take() {
                    self.otel_manager.record_duration(
                        "codex.windows_sandbox.elevated_setup_duration_ms",
                        started_at.elapsed(),
                        &[("result", "failure")],
                    );
                }
                self.chat_widget
                    .open_windows_sandbox_fallback_prompt(preset, reason);
            }
            AppEvent::BeginWindowsSandboxElevatedSetup { preset } => {
                #[cfg(target_os = "windows")]
                {
                    let policy = preset.sandbox.clone();
                    let policy_cwd = self.config.cwd.clone();
                    let command_cwd = policy_cwd.clone();
                    let env_map: std::collections::HashMap<String, String> =
                        std::env::vars().collect();
                    let codex_home = self.config.codex_home.clone();
                    let tx = self.app_event_tx.clone();

                    // If the elevated setup already ran on this machine, don't prompt for
                    // elevation again - just flip the config to use the elevated path.
                    if codex_core::windows_sandbox::sandbox_setup_is_complete(codex_home.as_path())
                    {
                        tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                            preset,
                            mode: WindowsSandboxEnableMode::Elevated,
                        });
                        return Ok(AppRunControl::Continue);
                    }

                    self.chat_widget.show_windows_sandbox_setup_status();
                    self.windows_sandbox.setup_started_at = Some(Instant::now());
                    let otel_manager = self.otel_manager.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = codex_core::windows_sandbox::run_elevated_setup(
                            &policy,
                            policy_cwd.as_path(),
                            command_cwd.as_path(),
                            &env_map,
                            codex_home.as_path(),
                        );
                        let event = match result {
                            Ok(()) => {
                                otel_manager.counter(
                                    "codex.windows_sandbox.elevated_setup_success",
                                    1,
                                    &[],
                                );
                                AppEvent::EnableWindowsSandboxForAgentMode {
                                    preset: preset.clone(),
                                    mode: WindowsSandboxEnableMode::Elevated,
                                }
                            }
                            Err(err) => {
                                otel_manager.counter(
                                    "codex.windows_sandbox.elevated_setup_failure",
                                    1,
                                    &[],
                                );
                                tracing::error!(
                                    error = %err,
                                    "failed to run elevated Windows sandbox setup"
                                );
                                AppEvent::OpenWindowsSandboxFallbackPrompt {
                                    preset,
                                    reason: WindowsSandboxFallbackReason::ElevationFailed,
                                }
                            }
                        };
                        tx.send(event);
                    });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = preset;
                }
            }
            AppEvent::EnableWindowsSandboxForAgentMode { preset, mode } => {
                #[cfg(target_os = "windows")]
                {
                    self.chat_widget.clear_windows_sandbox_setup_status();
                    if let Some(started_at) = self.windows_sandbox.setup_started_at.take() {
                        self.otel_manager.record_duration(
                            "codex.windows_sandbox.elevated_setup_duration_ms",
                            started_at.elapsed(),
                            &[("result", "success")],
                        );
                    }
                    let profile = self.active_profile.as_deref();
                    let feature_key = Feature::WindowsSandbox.key();
                    let elevated_key = Feature::WindowsSandboxElevated.key();
                    let elevated_enabled = matches!(mode, WindowsSandboxEnableMode::Elevated);
                    match ConfigEditsBuilder::new(&self.config.codex_home)
                        .with_profile(profile)
                        .set_feature_enabled(feature_key, true)
                        .set_feature_enabled(elevated_key, elevated_enabled)
                        .apply()
                        .await
                    {
                        Ok(()) => {
                            self.config.set_windows_sandbox_globally(true);
                            self.config
                                .set_windows_elevated_sandbox_globally(elevated_enabled);
                            self.chat_widget
                                .set_feature_enabled(Feature::WindowsSandbox, true);
                            self.chat_widget.set_feature_enabled(
                                Feature::WindowsSandboxElevated,
                                elevated_enabled,
                            );
                            self.chat_widget.clear_forced_auto_mode_downgrade();
                            if let Some((sample_paths, extra_count, failed_scan)) =
                                self.chat_widget.world_writable_warning_details()
                            {
                                self.app_event_tx.send(
                                    AppEvent::OpenWorldWritableWarningConfirmation {
                                        preset: Some(preset.clone()),
                                        sample_paths,
                                        extra_count,
                                        failed_scan,
                                    },
                                );
                            } else {
                                self.app_event_tx.send(AppEvent::CodexOp(
                                    Op::OverrideTurnContext {
                                        cwd: None,
                                        approval_policy: Some(preset.approval),
                                        sandbox_policy: Some(preset.sandbox.clone()),
                                        model: None,
                                        effort: None,
                                        summary: None,
                                        collaboration_mode: None,
                                        personality: None,
                                    },
                                ));
                                self.app_event_tx
                                    .send(AppEvent::UpdateAskForApprovalPolicy(preset.approval));
                                self.app_event_tx
                                    .send(AppEvent::UpdateSandboxPolicy(preset.sandbox.clone()));
                                self.chat_widget.add_info_message(
                                    match mode {
                                        WindowsSandboxEnableMode::Elevated => {
                                            "Enabled elevated agent sandbox.".to_string()
                                        }
                                        WindowsSandboxEnableMode::Legacy => {
                                            "Enabled non-elevated agent sandbox.".to_string()
                                        }
                                    },
                                    None,
                                );
                            }
                        }
                        Err(err) => {
                            tracing::error!(
                                error = %err,
                                "failed to enable Windows sandbox feature"
                            );
                            self.chat_widget.add_error_message(format!(
                                "Failed to enable the Windows sandbox feature: {err}"
                            ));
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = (preset, mode);
                }
            }
            AppEvent::PersistModelSelection { model, effort } => {
                let profile = self.active_profile.as_deref();
                match ConfigEditsBuilder::new(&self.config.codex_home)
                    .with_profile(profile)
                    .set_model(Some(model.as_str()), effort)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        let mut message = format!("Model changed to {model}");
                        if let Some(label) = Self::reasoning_label_for(&model, effort) {
                            message.push(' ');
                            message.push_str(label);
                        }
                        if let Some(profile) = profile {
                            message.push_str(" for ");
                            message.push_str(profile);
                            message.push_str(" profile");
                        }
                        self.chat_widget.add_info_message(message, None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist model selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save model for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_widget
                                .add_error_message(format!("Failed to save default model: {err}"));
                        }
                    }
                }
            }
            AppEvent::UpdateAskForApprovalPolicy(policy) => {
                self.runtime_approval_policy_override = Some(policy);
                if let Err(err) = self.config.approval_policy.set(policy) {
                    tracing::warn!(%err, "failed to set approval policy on app config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set approval policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.chat_widget.set_approval_policy(policy);
            }
            AppEvent::UpdateSandboxPolicy(policy) => {
                #[cfg(target_os = "windows")]
                let policy_is_workspace_write_or_ro = matches!(
                    &policy,
                    codex_core::protocol::SandboxPolicy::WorkspaceWrite { .. }
                        | codex_core::protocol::SandboxPolicy::ReadOnly
                );

                if let Err(err) = self.config.sandbox_policy.set(policy.clone()) {
                    tracing::warn!(%err, "failed to set sandbox policy on app config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                #[cfg(target_os = "windows")]
                if !matches!(&policy, codex_core::protocol::SandboxPolicy::ReadOnly)
                    || codex_core::get_platform_sandbox().is_some()
                {
                    self.config.forced_auto_mode_downgraded_on_windows = false;
                }
                if let Err(err) = self.chat_widget.set_sandbox_policy(policy) {
                    tracing::warn!(%err, "failed to set sandbox policy on chat config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.runtime_sandbox_policy_override =
                    Some(self.config.sandbox_policy.get().clone());

                // If sandbox policy becomes workspace-write or read-only, run the Windows world-writable scan.
                #[cfg(target_os = "windows")]
                {
                    // One-shot suppression if the user just confirmed continue.
                    if self.windows_sandbox.skip_world_writable_scan_once {
                        self.windows_sandbox.skip_world_writable_scan_once = false;
                        return Ok(AppRunControl::Continue);
                    }

                    let should_check = codex_core::get_platform_sandbox().is_some()
                        && policy_is_workspace_write_or_ro
                        && !self.chat_widget.world_writable_warning_hidden();
                    if should_check {
                        let cwd = self.config.cwd.clone();
                        let env_map: std::collections::HashMap<String, String> =
                            std::env::vars().collect();
                        let tx = self.app_event_tx.clone();
                        let logs_base_dir = self.config.codex_home.clone();
                        let sandbox_policy = self.config.sandbox_policy.get().clone();
                        Self::spawn_world_writable_scan(
                            cwd,
                            env_map,
                            logs_base_dir,
                            sandbox_policy,
                            tx,
                        );
                    }
                }
            }
            AppEvent::UpdateFeatureFlags { updates } => {
                if updates.is_empty() {
                    return Ok(AppRunControl::Continue);
                }
                let mut builder = ConfigEditsBuilder::new(&self.config.codex_home)
                    .with_profile(self.active_profile.as_deref());
                for (feature, enabled) in &updates {
                    let feature_key = feature.key();
                    if *enabled {
                        // Update the in-memory configs.
                        self.config.features.enable(*feature);
                        self.chat_widget.set_feature_enabled(*feature, true);
                        builder = builder.set_feature_enabled(feature_key, true);
                    } else {
                        // Update the in-memory configs.
                        self.config.features.disable(*feature);
                        self.chat_widget.set_feature_enabled(*feature, false);
                        if feature.default_enabled() {
                            builder = builder.set_feature_enabled(feature_key, false);
                        } else {
                            // If the feature already default to `false`, we drop the key
                            // in the config file so that the user does not miss the feature
                            // once it gets globally released.
                            builder = builder.with_edits(vec![ConfigEdit::ClearPath {
                                segments: vec!["features".to_string(), feature_key.to_string()],
                            }]);
                        }
                    }
                }
                if let Err(err) = builder.apply().await {
                    tracing::error!(error = %err, "failed to persist feature flags");
                    self.chat_widget.add_error_message(format!(
                        "Failed to update experimental features: {err}"
                    ));
                }
            }
            AppEvent::SkipNextWorldWritableScan => {
                self.windows_sandbox.skip_world_writable_scan_once = true;
            }
            AppEvent::UpdateFullAccessWarningAcknowledged(ack) => {
                self.chat_widget.set_full_access_warning_acknowledged(ack);
            }
            AppEvent::UpdateWorldWritableWarningAcknowledged(ack) => {
                self.chat_widget
                    .set_world_writable_warning_acknowledged(ack);
            }
            AppEvent::UpdateRateLimitSwitchPromptHidden(hidden) => {
                self.chat_widget.set_rate_limit_switch_prompt_hidden(hidden);
            }
            AppEvent::PersistFullAccessWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.codex_home)
                    .set_hide_full_access_warning(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist full access warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save full access confirmation preference: {err}"
                    ));
                }
            }
            AppEvent::PersistWorldWritableWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.codex_home)
                    .set_hide_world_writable_warning(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist world-writable warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save Agent mode warning preference: {err}"
                    ));
                }
            }
            AppEvent::PersistRateLimitSwitchPromptHidden => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.codex_home)
                    .set_hide_rate_limit_model_nudge(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist rate limit switch prompt preference"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save rate limit reminder preference: {err}"
                    ));
                }
            }
            AppEvent::PersistModelMigrationPromptAcknowledged {
                from_model,
                to_model,
            } => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.codex_home)
                    .record_model_migration_seen(from_model.as_str(), to_model.as_str())
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist model migration prompt acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save model migration prompt preference: {err}"
                    ));
                }
            }
            AppEvent::OpenApprovalsPopup => {
                self.chat_widget.open_approvals_popup();
            }
            AppEvent::OpenAgentPicker => {
                self.open_agent_picker();
            }
            AppEvent::SelectAgentThread(thread_id) => {
                self.select_agent_thread(tui, thread_id).await?;
            }
            AppEvent::OpenNewAgentPrompt => {
                self.chat_widget.open_agent_alias_prompt();
            }
            AppEvent::OpenCloseAgentPicker => {
                self.open_close_agent_picker();
            }
            AppEvent::CreateIdleAgent { alias } => {
                self.create_idle_agent(tui, alias).await?;
            }
            AppEvent::CloseAgentThread(thread_id) => {
                self.close_agent_thread(tui, thread_id).await?;
            }
            AppEvent::OpenSkillsList => {
                self.chat_widget.open_skills_list();
            }
            AppEvent::OpenManageSkillsPopup => {
                self.chat_widget.open_manage_skills_popup();
            }
            AppEvent::SetSkillEnabled { path, enabled } => {
                let edits = [ConfigEdit::SetSkillConfig {
                    path: path.clone(),
                    enabled,
                }];
                match ConfigEditsBuilder::new(&self.config.codex_home)
                    .with_edits(edits)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        self.chat_widget.update_skill_enabled(path.clone(), enabled);
                    }
                    Err(err) => {
                        let path_display = path.display();
                        self.chat_widget.add_error_message(format!(
                            "Failed to update skill config for {path_display}: {err}"
                        ));
                    }
                }
            }
            AppEvent::OpenPermissionsPopup => {
                self.chat_widget.open_permissions_popup();
            }
            AppEvent::OpenReviewBranchPicker(cwd) => {
                self.chat_widget.show_review_branch_picker(&cwd).await;
            }
            AppEvent::OpenReviewCommitPicker(cwd) => {
                self.chat_widget.show_review_commit_picker(&cwd).await;
            }
            AppEvent::OpenReviewCustomPrompt => {
                self.chat_widget.show_review_custom_prompt();
            }
            AppEvent::SubmitUserMessageWithMode {
                text,
                collaboration_mode,
            } => {
                self.chat_widget
                    .submit_user_message_with_mode(text, collaboration_mode);
            }
            AppEvent::ManageSkillsClosed => {
                self.chat_widget.handle_manage_skills_closed();
            }
            AppEvent::FullScreenApprovalRequest(request) => match request {
                ApprovalRequest::ApplyPatch { cwd, changes, .. } => {
                    let _ = tui.enter_alt_screen();
                    let diff_summary = DiffSummary::new(changes, cwd);
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![diff_summary.into()],
                        "P A T C H".to_string(),
                    ));
                }
                ApprovalRequest::Exec { command, .. } => {
                    let _ = tui.enter_alt_screen();
                    let full_cmd = strip_bash_lc_and_escape(&command);
                    let full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                    self.overlay = Some(Overlay::new_static_with_lines(
                        full_cmd_lines,
                        "E X E C".to_string(),
                    ));
                }
                ApprovalRequest::McpElicitation {
                    server_name,
                    message,
                    ..
                } => {
                    let _ = tui.enter_alt_screen();
                    let paragraph = Paragraph::new(vec![
                        Line::from(vec!["Server: ".into(), server_name.bold()]),
                        Line::from(""),
                        Line::from(message),
                    ])
                    .wrap(Wrap { trim: false });
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(paragraph)],
                        "E L I C I T A T I O N".to_string(),
                    ));
                }
            },
        }
        Ok(AppRunControl::Continue)
    }

    fn handle_codex_event_now(&mut self, event: Event) {
        if let EventMsg::ListSkillsResponse(response) = &event.msg {
            let cwd = self.chat_widget.config_ref().cwd.clone();
            let errors = errors_for_cwd(&cwd, response);
            emit_skill_load_warnings(&self.app_event_tx, &errors);
        }
        self.handle_backtrack_event(&event.msg);
        self.chat_widget.handle_codex_event(event);
    }

    fn should_render_history_cell(&self, thread_id: Option<ThreadId>) -> bool {
        if let Some(thread_id) = thread_id {
            if self.closed_thread_ids.contains(&thread_id) {
                return false;
            }
            return Some(thread_id)
                == self
                    .active_thread_id
                    .or_else(|| self.chat_widget.thread_id());
        }
        true
    }

    fn handle_history_cell(
        &mut self,
        tui: &mut tui::Tui,
        thread_id: Option<ThreadId>,
        cell: Box<dyn HistoryCell>,
    ) -> Result<()> {
        if !self.should_render_history_cell(thread_id) {
            return Ok(());
        }
        let cell: Arc<dyn HistoryCell> = cell.into();
        if let Some(Overlay::Transcript(t)) = &mut self.overlay {
            t.insert_cell(cell.clone());
            tui.frame_requester().schedule_frame();
        }
        self.transcript_cells.push(cell.clone());
        let mut display = cell.display_lines(tui.terminal.last_known_screen_size.width);
        if !display.is_empty() {
            // Only insert a separating blank line for new cells that are not
            // part of an ongoing stream. Streaming continuations should not
            // accrue extra blank lines between chunks.
            if !cell.is_stream_continuation() {
                if self.has_emitted_history_lines {
                    display.insert(0, Line::from(""));
                } else {
                    self.has_emitted_history_lines = true;
                }
            }
            if self.overlay.is_some() {
                self.deferred_history_lines.extend(display);
            } else {
                tui.insert_history_lines(display);
            }
        }
        Ok(())
    }

    fn handle_codex_event_replay(&mut self, event: Event) {
        self.handle_backtrack_event(&event.msg);
        self.chat_widget.handle_codex_event_replay(event);
    }

    fn handle_active_thread_event(&mut self, tui: &mut tui::Tui, event: Event) -> Result<()> {
        self.handle_codex_event_now(event);
        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    async fn handle_thread_created(&mut self, thread_id: ThreadId) -> Result<()> {
        self.closed_thread_ids.remove(&thread_id);
        if self.thread_event_channels.contains_key(&thread_id) {
            return Ok(());
        }
        let thread = match self.server.get_thread(thread_id).await {
            Ok(thread) => thread,
            Err(err) => {
                tracing::warn!("failed to attach listener for thread {thread_id}: {err}");
                return Ok(());
            }
        };
        let config = thread.config_snapshot().await;
        if !matches!(
            config.session_source,
            SessionSource::SubAgent(SubAgentSource::User)
        ) {
            if matches!(config.session_source, SessionSource::SubAgent(_)) {
                tokio::spawn(async move { while thread.next_event().await.is_ok() {} });
            }
            return Ok(());
        }
        let rollout_path = thread.rollout_path();
        let event = Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(
                self.session_configured_for_thread(thread_id, rollout_path),
            ),
        };
        let channel =
            ThreadEventChannel::new_with_session_configured(THREAD_EVENT_CHANNEL_CAPACITY, event);
        let sender = channel.sender.clone();
        let store = Arc::clone(&channel.store);
        self.thread_event_channels.insert(thread_id, channel);
        self.register_thread_order(thread_id);
        tokio::spawn(async move {
            loop {
                let event = match thread.next_event().await {
                    Ok(event) => event,
                    Err(err) => {
                        tracing::debug!("external thread {thread_id} listener stopped: {err}");
                        break;
                    }
                };
                let should_send = {
                    let mut guard = store.lock().await;
                    guard.push_event(event.clone());
                    guard.active
                };
                if should_send && let Err(err) = sender.send(event).await {
                    tracing::debug!("external thread {thread_id} channel closed: {err}");
                    break;
                }
            }
        });
        Ok(())
    }

    fn session_configured_for_thread(
        &self,
        thread_id: ThreadId,
        rollout_path: Option<PathBuf>,
    ) -> SessionConfiguredEvent {
        let mut session_configured =
            self.primary_session_configured
                .clone()
                .unwrap_or_else(|| SessionConfiguredEvent {
                    session_id: thread_id,
                    forked_from_id: None,
                    model: self.chat_widget.current_model().to_string(),
                    model_provider_id: self.config.model_provider_id.clone(),
                    approval_policy: *self.config.approval_policy.get(),
                    sandbox_policy: self.config.sandbox_policy.get().clone(),
                    cwd: self.config.cwd.clone(),
                    reasoning_effort: None,
                    history_log_id: 0,
                    history_entry_count: 0,
                    initial_messages: None,
                    rollout_path: Some(PathBuf::new()),
                });
        session_configured.session_id = thread_id;
        session_configured.forked_from_id = None;
        session_configured.history_log_id = 0;
        session_configured.history_entry_count = 0;
        session_configured.initial_messages = None;
        session_configured.rollout_path = rollout_path;
        session_configured
    }

    fn reasoning_label(reasoning_effort: Option<ReasoningEffortConfig>) -> &'static str {
        match reasoning_effort {
            Some(ReasoningEffortConfig::Minimal) => "minimal",
            Some(ReasoningEffortConfig::Low) => "low",
            Some(ReasoningEffortConfig::Medium) => "medium",
            Some(ReasoningEffortConfig::High) => "high",
            Some(ReasoningEffortConfig::XHigh) => "xhigh",
            None | Some(ReasoningEffortConfig::None) => "default",
        }
    }

    fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<ReasoningEffortConfig>,
    ) -> Option<&'static str> {
        (!model.starts_with("codex-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> codex_core::protocol::TokenUsage {
        self.chat_widget.token_usage()
    }

    fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        // TODO(aibrahim): Remove this and don't use config as a state object.
        // Instead, explicitly pass the stored collaboration mode's effort into new sessions.
        self.config.model_reasoning_effort = effort;
        self.chat_widget.set_reasoning_effort(effort);
    }

    async fn launch_external_editor(&mut self, tui: &mut tui::Tui) {
        let editor_cmd = match external_editor::resolve_editor_command() {
            Ok(cmd) => cmd,
            Err(external_editor::EditorError::MissingEditor) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(
                        "Cannot open external editor: set $VISUAL or $EDITOR".to_string(),
                    ));
                self.reset_external_editor_state(tui);
                return;
            }
            Err(err) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
                self.reset_external_editor_state(tui);
                return;
            }
        };

        let seed = self.chat_widget.composer_text_with_pending();
        let editor_result = tui
            .with_restored(tui::RestoreMode::KeepRaw, || async {
                external_editor::run_editor(&seed, &editor_cmd).await
            })
            .await;
        self.reset_external_editor_state(tui);

        match editor_result {
            Ok(new_text) => {
                // Trim trailing whitespace
                let cleaned = new_text.trim_end().to_string();
                self.chat_widget.apply_external_edit(cleaned);
            }
            Err(err) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
            }
        }
        tui.frame_requester().schedule_frame();
    }

    fn request_external_editor_launch(&mut self, tui: &mut tui::Tui) {
        self.chat_widget
            .set_external_editor_state(ExternalEditorState::Requested);
        self.chat_widget.set_footer_hint_override(Some(vec![(
            EXTERNAL_EDITOR_HINT.to_string(),
            String::new(),
        )]));
        tui.frame_requester().schedule_frame();
    }

    fn reset_external_editor_state(&mut self, tui: &mut tui::Tui) {
        self.chat_widget
            .set_external_editor_state(ExternalEditorState::Closed);
        self.chat_widget.set_footer_hint_override(None);
        tui.frame_requester().schedule_frame();
    }

    async fn handle_key_event(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        if matches!(
            key_event,
            KeyEvent {
                code: KeyCode::Tab,
                modifiers: crossterm::event::KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                ..
            }
        ) && self.can_cycle_agent_tabs()
        {
            if let Err(err) = self.cycle_active_thread(tui).await {
                self.chat_widget
                    .add_error_message(format!("Failed to switch agent: {err}"));
            }
            return;
        }

        match key_event {
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                // Enter alternate screen and set viewport to full size.
                let _ = tui.enter_alt_screen();
                self.overlay = Some(Overlay::new_transcript(self.transcript_cells.clone()));
                tui.frame_requester().schedule_frame();
            }
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                // Only launch the external editor if there is no overlay and the bottom pane is not in use.
                // Note that it can be launched while a task is running to enable editing while the previous turn is ongoing.
                if self.overlay.is_none()
                    && self.chat_widget.can_launch_external_editor()
                    && self.chat_widget.external_editor_state() == ExternalEditorState::Closed
                {
                    self.request_external_editor_launch(tui);
                }
            }
            // Esc primes/advances backtracking only in normal (not working) mode
            // with the composer focused and empty. In any other state, forward
            // Esc so the active UI (e.g. status indicator, modals, popups)
            // handles it.
            KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                if self.chat_widget.is_normal_backtrack_mode()
                    && self.chat_widget.composer_is_empty()
                {
                    self.handle_backtrack_esc_key(tui);
                } else {
                    self.chat_widget.handle_key_event(key_event);
                }
            }
            // Enter confirms backtrack when primed + count > 0. Otherwise pass to widget.
            KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            } if self.backtrack.primed
                && self.backtrack.nth_user_message != usize::MAX
                && self.chat_widget.composer_is_empty() =>
            {
                if let Some(selection) = self.confirm_backtrack_from_main() {
                    self.apply_backtrack_selection(tui, selection);
                }
            }
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                // Any non-Esc key press should cancel a primed backtrack.
                // This avoids stale "Esc-primed" state after the user starts typing
                // (even if they later backspace to empty).
                if key_event.code != KeyCode::Esc && self.backtrack.primed {
                    self.reset_backtrack_state();
                }
                self.chat_widget.handle_key_event(key_event);
            }
            _ => {
                // Ignore Release key events.
            }
        };
    }

    #[cfg(target_os = "windows")]
    fn spawn_world_writable_scan(
        cwd: PathBuf,
        env_map: std::collections::HashMap<String, String>,
        logs_base_dir: PathBuf,
        sandbox_policy: codex_core::protocol::SandboxPolicy,
        tx: AppEventSender,
    ) {
        tokio::task::spawn_blocking(move || {
            let result = codex_windows_sandbox::apply_world_writable_scan_and_denies(
                &logs_base_dir,
                &cwd,
                &env_map,
                &sandbox_policy,
                Some(logs_base_dir.as_path()),
            );
            if result.is_err() {
                // Scan failed: warn without examples.
                tx.send(AppEvent::OpenWorldWritableWarningConfirmation {
                    preset: None,
                    sample_paths: Vec::new(),
                    extra_count: 0usize,
                    failed_scan: true,
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_backtrack::BacktrackState;
    use crate::app_backtrack::user_count;
    use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
    use crate::file_search::FileSearchManager;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::HistoryCell;
    use crate::history_cell::UserHistoryCell;
    use crate::history_cell::new_session_info;
    use codex_core::AuthManager;
    use codex_core::CodexAuth;
    use codex_core::ThreadManager;
    use codex_core::config::ConfigBuilder;
    use codex_core::config::ConfigOverrides;
    use codex_core::models_manager::manager::ModelsManager;
    use codex_core::protocol::AskForApproval;
    use codex_core::protocol::Event;
    use codex_core::protocol::EventMsg;
    use codex_core::protocol::SandboxPolicy;
    use codex_core::protocol::SessionConfiguredEvent;
    use codex_core::protocol::SessionSource;
    use codex_otel::OtelManager;
    use codex_protocol::ThreadId;
    use codex_protocol::user_input::TextElement;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Line;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tempfile::tempdir;

    #[test]
    fn normalize_harness_overrides_resolves_relative_add_dirs() -> Result<()> {
        let temp_dir = tempdir()?;
        let base_cwd = temp_dir.path().join("base");
        std::fs::create_dir_all(&base_cwd)?;

        let overrides = ConfigOverrides {
            additional_writable_roots: vec![PathBuf::from("rel")],
            ..Default::default()
        };
        let normalized = normalize_harness_overrides_for_cwd(overrides, &base_cwd)?;

        assert_eq!(
            normalized.additional_writable_roots,
            vec![base_cwd.join("rel")]
        );
        Ok(())
    }

    async fn make_test_app() -> App {
        let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let server = Arc::new(ThreadManager::with_models_provider(
            CodexAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ));
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let otel_manager = test_otel_manager(&config, model.as_str());

        App {
            server,
            otel_manager,
            app_event_tx,
            chat_widget,
            auth_manager,
            config,
            active_profile: None,
            cli_kv_overrides: Vec::new(),
            harness_overrides: ConfigOverrides::default(),
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            file_search,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            enhanced_keys_supported: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: codex_feedback::CodexFeedback::new(),
            pending_update_action: None,
            windows_sandbox: WindowsSandboxState::default(),
            thread_event_channels: HashMap::new(),
            thread_order: Vec::new(),
            thread_drafts: HashMap::new(),
            closed_thread_ids: HashSet::new(),
            user_agent_aliases: HashMap::new(),
            active_thread_id: None,
            active_thread_rx: None,
            primary_thread_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
            pending_scrollback_clear: false,
        }
    }

    async fn make_test_app_with_channels() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        tokio::sync::mpsc::UnboundedReceiver<Op>,
    ) {
        let (chat_widget, app_event_tx, rx, op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let server = Arc::new(ThreadManager::with_models_provider(
            CodexAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ));
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let otel_manager = test_otel_manager(&config, model.as_str());

        (
            App {
                server,
                otel_manager,
                app_event_tx,
                chat_widget,
                auth_manager,
                config,
                active_profile: None,
                cli_kv_overrides: Vec::new(),
                harness_overrides: ConfigOverrides::default(),
                runtime_approval_policy_override: None,
                runtime_sandbox_policy_override: None,
                file_search,
                transcript_cells: Vec::new(),
                overlay: None,
                deferred_history_lines: Vec::new(),
                has_emitted_history_lines: false,
                enhanced_keys_supported: false,
                commit_anim_running: Arc::new(AtomicBool::new(false)),
                backtrack: BacktrackState::default(),
                backtrack_render_pending: false,
                feedback: codex_feedback::CodexFeedback::new(),
                pending_update_action: None,
                windows_sandbox: WindowsSandboxState::default(),
                thread_event_channels: HashMap::new(),
                thread_order: Vec::new(),
                thread_drafts: HashMap::new(),
                closed_thread_ids: HashSet::new(),
                user_agent_aliases: HashMap::new(),
                active_thread_id: None,
                active_thread_rx: None,
                primary_thread_id: None,
                primary_session_configured: None,
                pending_primary_events: VecDeque::new(),
                pending_scrollback_clear: false,
            },
            rx,
            op_rx,
        )
    }

    fn test_otel_manager(config: &Config, model: &str) -> OtelManager {
        let model_info = ModelsManager::construct_model_info_offline(model, config);
        OtelManager::new(
            ThreadId::new(),
            model,
            model_info.slug.as_str(),
            None,
            None,
            None,
            false,
            "test".to_string(),
            SessionSource::Cli,
        )
    }

    fn all_model_presets() -> Vec<ModelPreset> {
        codex_core::models_manager::model_presets::all_model_presets().clone()
    }

    fn model_migration_copy_to_plain_text(
        copy: &crate::model_migration::ModelMigrationCopy,
    ) -> String {
        if let Some(markdown) = copy.markdown.as_ref() {
            return markdown.clone();
        }
        let mut s = String::new();
        for span in &copy.heading {
            s.push_str(&span.content);
        }
        s.push('\n');
        s.push('\n');
        for line in &copy.content {
            for span in &line.spans {
                s.push_str(&span.content);
            }
            s.push('\n');
        }
        s
    }

    #[tokio::test]
    async fn model_migration_prompt_only_shows_for_deprecated_models() {
        let seen = BTreeMap::new();
        assert!(should_show_model_migration_prompt(
            "gpt-5",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5-codex",
            "gpt-5.1-codex",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5-codex-mini",
            "gpt-5.1-codex-mini",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5.1-codex",
            "gpt-5.1-codex-max",
            &seen,
            &all_model_presets()
        ));
        assert!(!should_show_model_migration_prompt(
            "gpt-5.1-codex",
            "gpt-5.1-codex",
            &seen,
            &all_model_presets()
        ));
    }

    #[tokio::test]
    async fn model_migration_prompt_respects_hide_flag_and_self_target() {
        let mut seen = BTreeMap::new();
        seen.insert("gpt-5".to_string(), "gpt-5.1".to_string());
        assert!(!should_show_model_migration_prompt(
            "gpt-5",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
        assert!(!should_show_model_migration_prompt(
            "gpt-5.1",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
    }

    #[tokio::test]
    async fn model_migration_prompt_skips_when_target_missing() {
        let mut available = all_model_presets();
        let mut current = available
            .iter()
            .find(|preset| preset.model == "gpt-5-codex")
            .cloned()
            .expect("preset present");
        current.upgrade = Some(ModelUpgrade {
            id: "missing-target".to_string(),
            reasoning_effort_mapping: None,
            migration_config_key: HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG.to_string(),
            model_link: None,
            upgrade_copy: None,
            migration_markdown: None,
        });
        available.retain(|preset| preset.model != "gpt-5-codex");
        available.push(current.clone());

        assert!(should_show_model_migration_prompt(
            &current.model,
            "missing-target",
            &BTreeMap::new(),
            &available,
        ));

        assert!(target_preset_for_upgrade(&available, "missing-target").is_none());
    }

    #[tokio::test]
    async fn model_migration_prompt_shows_for_hidden_model() {
        let codex_home = tempdir().expect("temp codex home");
        let config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await
            .expect("config");

        let available_models = all_model_presets();
        let current = available_models
            .iter()
            .find(|preset| preset.model == "gpt-5.1-codex")
            .cloned()
            .expect("gpt-5.1-codex preset present");
        assert!(
            !current.show_in_picker,
            "expected gpt-5.1-codex to be hidden from picker for this test"
        );

        let upgrade = current.upgrade.as_ref().expect("upgrade configured");
        assert!(
            should_show_model_migration_prompt(
                &current.model,
                &upgrade.id,
                &config.notices.model_migrations,
                &available_models,
            ),
            "expected migration prompt to be eligible for hidden model"
        );

        let target = target_preset_for_upgrade(&available_models, &upgrade.id)
            .expect("upgrade target present");
        let target_description =
            (!target.description.is_empty()).then(|| target.description.clone());
        let can_opt_out = true;
        let copy = migration_copy_for_models(
            &current.model,
            &upgrade.id,
            upgrade.model_link.clone(),
            upgrade.upgrade_copy.clone(),
            upgrade.migration_markdown.clone(),
            target.display_name.clone(),
            target_description,
            can_opt_out,
        );

        // Snapshot the copy we would show; rendering is covered by model_migration snapshots.
        assert_snapshot!(
            "model_migration_prompt_shows_for_hidden_model",
            model_migration_copy_to_plain_text(&copy)
        );
    }

    #[tokio::test]
    async fn update_reasoning_effort_updates_collaboration_mode() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn backtrack_selection_with_duplicate_history_targets_unique_turn() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let user_cell = |text: &str,
                         text_elements: Vec<TextElement>,
                         local_image_paths: Vec<PathBuf>|
         -> Arc<dyn HistoryCell> {
            Arc::new(UserHistoryCell {
                message: text.to_string(),
                text_elements,
                local_image_paths,
            }) as Arc<dyn HistoryCell>
        };
        let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(AgentMessageCell::new(
                vec![Line::from(text.to_string())],
                true,
            )) as Arc<dyn HistoryCell>
        };

        let make_header = |is_first| {
            let event = SessionConfiguredEvent {
                session_id: ThreadId::new(),
                forked_from_id: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                approval_policy: AskForApproval::Never,
                sandbox_policy: SandboxPolicy::ReadOnly,
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: Some(PathBuf::new()),
            };
            Arc::new(new_session_info(
                app.chat_widget.config_ref(),
                app.chat_widget.current_model(),
                event,
                is_first,
            )) as Arc<dyn HistoryCell>
        };

        let placeholder = "[Image #1]";
        let edited_text = format!("follow-up (edited) {placeholder}");
        let edited_range = edited_text.len().saturating_sub(placeholder.len())..edited_text.len();
        let edited_text_elements = vec![TextElement::new(edited_range.into(), None)];
        let edited_local_image_paths = vec![PathBuf::from("/tmp/fake-image.png")];

        // Simulate a transcript with duplicated history (e.g., from prior backtracks)
        // and an edited turn appended after a session header boundary.
        app.transcript_cells = vec![
            make_header(true),
            user_cell("first question", Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell("follow-up", Vec::new(), Vec::new()),
            agent_cell("answer follow-up"),
            make_header(false),
            user_cell("first question", Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell(
                &edited_text,
                edited_text_elements.clone(),
                edited_local_image_paths.clone(),
            ),
            agent_cell("answer edited"),
        ];

        assert_eq!(user_count(&app.transcript_cells), 2);

        let base_id = ThreadId::new();
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: base_id,
                forked_from_id: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                approval_policy: AskForApproval::Never,
                sandbox_policy: SandboxPolicy::ReadOnly,
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: Some(PathBuf::new()),
            }),
        });

        app.backtrack.base_id = Some(base_id);
        app.backtrack.primed = true;
        app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

        let selection = app
            .confirm_backtrack_from_main()
            .expect("backtrack selection");
        assert_eq!(selection.nth_user_message, 1);
        assert_eq!(selection.prefill, edited_text);
        assert_eq!(selection.text_elements, edited_text_elements);
        assert_eq!(selection.local_image_paths, edited_local_image_paths);

        app.apply_backtrack_rollback(selection);

        let mut rollback_turns = None;
        while let Ok(op) = op_rx.try_recv() {
            if let Op::ThreadRollback { num_turns } = op {
                rollback_turns = Some(num_turns);
            }
        }

        assert_eq!(rollback_turns, Some(1));
    }

    #[tokio::test]
    async fn new_session_requests_shutdown_for_previous_conversation() {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let thread_id = ThreadId::new();
        let event = SessionConfiguredEvent {
            session_id: thread_id,
            forked_from_id: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::ReadOnly,
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            rollout_path: Some(PathBuf::new()),
        };

        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(event),
        });

        while app_event_rx.try_recv().is_ok() {}
        while op_rx.try_recv().is_ok() {}

        app.shutdown_current_thread().await;

        match op_rx.try_recv() {
            Ok(Op::Shutdown) => {}
            Ok(other) => panic!("expected Op::Shutdown, got {other:?}"),
            Err(_) => panic!("expected shutdown op to be sent"),
        }
    }

    #[tokio::test]
    async fn session_summary_skip_zero_usage() {
        assert!(session_summary(TokenUsage::default(), None).is_none());
    }

    #[tokio::test]
    async fn session_summary_includes_resume_hint() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let summary = session_summary(usage, Some(conversation)).expect("summary");
        assert_eq!(
            summary.usage_line,
            "Token usage: total=12 input=10 output=2"
        );
        assert_eq!(
            summary.resume_command,
            Some("codex resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }
}
