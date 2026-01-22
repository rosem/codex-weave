use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::WidgetRef;

use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height;
use super::selection_popup_common::render_rows;
use crate::render::Insets;
use crate::render::RectExt;
use codex_common::fuzzy_match::fuzzy_match;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WeaveCommand {
    New,
    Interrupt,
    Compact,
    Review,
}

impl WeaveCommand {
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Interrupt => "interrupt",
            Self::Compact => "compact",
            Self::Review => "review",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::New => "start a new context for the mentioned agent",
            Self::Interrupt => "interrupt the agent's current task",
            Self::Compact => "summarize and compact the agent context",
            Self::Review => "ask the agent to review with instructions",
        }
    }
}

pub(crate) struct WeaveCommandPopup {
    query: String,
    commands: Vec<WeaveCommand>,
    state: ScrollState,
}

impl WeaveCommandPopup {
    pub(crate) fn new() -> Self {
        Self {
            query: String::new(),
            commands: Self::default_commands(),
            state: ScrollState::new(),
        }
    }

    pub(crate) fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.clamp_selection();
    }

    pub(crate) fn calculate_required_height(&self, width: u16) -> u16 {
        let rows = self.rows_from_matches(self.filtered());
        measure_rows_height(&rows, &self.state, MAX_POPUP_ROWS, width)
    }

    pub(crate) fn move_up(&mut self) {
        let len = self.filtered_items().len();
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    pub(crate) fn move_down(&mut self) {
        let len = self.filtered_items().len();
        self.state.move_down_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    pub(crate) fn selected_command(&self) -> Option<WeaveCommand> {
        let matches = self.filtered_items();
        let idx = self.state.selected_idx?;
        matches.get(idx).copied()
    }

    fn default_commands() -> Vec<WeaveCommand> {
        vec![
            WeaveCommand::New,
            WeaveCommand::Interrupt,
            WeaveCommand::Compact,
        ]
    }

    fn clamp_selection(&mut self) {
        let len = self.filtered_items().len();
        self.state.clamp_selection(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn filtered_items(&self) -> Vec<WeaveCommand> {
        self.filtered().into_iter().map(|(cmd, _, _)| cmd).collect()
    }

    fn filtered(&self) -> Vec<(WeaveCommand, Option<Vec<usize>>, i32)> {
        let filter = self.query.trim();
        let mut out = Vec::new();
        if filter.is_empty() {
            for cmd in &self.commands {
                out.push((*cmd, None, 0));
            }
            return out;
        }

        for cmd in &self.commands {
            if let Some((indices, score)) = fuzzy_match(cmd.command(), filter) {
                out.push((*cmd, Some(indices), score));
            }
        }

        out.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.command().cmp(b.0.command())));
        out
    }

    fn rows_from_matches(
        &self,
        matches: Vec<(WeaveCommand, Option<Vec<usize>>, i32)>,
    ) -> Vec<GenericDisplayRow> {
        matches
            .into_iter()
            .map(|(cmd, indices, _)| {
                let command = cmd.command();
                let name = format!("/{command}");
                let match_indices =
                    indices.map(|idxs| idxs.into_iter().map(|idx| idx + 1).collect());
                GenericDisplayRow {
                    name,
                    name_style: None,
                    match_indices,
                    display_shortcut: None,
                    description: Some(cmd.description().to_string()),
                    disabled_reason: None,
                    wrap_indent: None,
                }
            })
            .collect()
    }
}

impl WidgetRef for WeaveCommandPopup {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let rows = self.rows_from_matches(self.filtered());
        render_rows(
            area.inset(Insets::tlbr(0, 2, 0, 0)),
            buf,
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            "no commands",
        );
    }
}
