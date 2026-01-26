use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;

use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::render_rows_single_line;
use crate::key_hint;
use crate::render::Insets;
use crate::render::RectExt;
use crate::skills_helpers::match_skill;

pub(crate) struct AgentMentionPopup {
    query: String,
    aliases: Vec<String>,
    state: ScrollState,
}

impl AgentMentionPopup {
    pub(crate) fn new(aliases: Vec<String>) -> Self {
        Self {
            query: String::new(),
            aliases,
            state: ScrollState::new(),
        }
    }

    pub(crate) fn set_aliases(&mut self, aliases: Vec<String>) {
        self.aliases = aliases;
        self.clamp_selection();
    }

    pub(crate) fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.clamp_selection();
    }

    pub(crate) fn calculate_required_height(&self, _width: u16) -> u16 {
        let rows = self.rows_from_matches(self.filtered());
        let visible = rows.len().clamp(1, MAX_POPUP_ROWS);
        (visible as u16).saturating_add(2)
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

    pub(crate) fn selected_alias(&self) -> Option<&str> {
        let matches = self.filtered_items();
        let idx = self.state.selected_idx?;
        let alias_idx = matches.get(idx)?;
        self.aliases.get(*alias_idx).map(String::as_str)
    }

    fn clamp_selection(&mut self) {
        let len = self.filtered_items().len();
        self.state.clamp_selection(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn filtered_items(&self) -> Vec<usize> {
        self.filtered().into_iter().map(|(idx, _, _)| idx).collect()
    }

    fn rows_from_matches(
        &self,
        matches: Vec<(usize, Option<Vec<usize>>, i32)>,
    ) -> Vec<GenericDisplayRow> {
        matches
            .into_iter()
            .map(|(idx, indices, _score)| {
                let alias = &self.aliases[idx];
                GenericDisplayRow {
                    name: alias.to_string(),
                    match_indices: indices,
                    display_shortcut: None,
                    description: None,
                    disabled_reason: None,
                    wrap_indent: None,
                }
            })
            .collect()
    }

    fn filtered(&self) -> Vec<(usize, Option<Vec<usize>>, i32)> {
        let filter = self.query.trim();
        let mut out: Vec<(usize, Option<Vec<usize>>, i32)> = Vec::new();

        if filter.is_empty() {
            for (idx, _alias) in self.aliases.iter().enumerate() {
                out.push((idx, None, 0));
            }
            return out;
        }

        for (idx, alias) in self.aliases.iter().enumerate() {
            if let Some((indices, score)) = match_skill(filter, alias, alias) {
                out.push((idx, indices, score));
            }
        }

        out.sort_by(|a, b| {
            a.2.cmp(&b.2).then_with(|| {
                let an = &self.aliases[a.0];
                let bn = &self.aliases[b.0];
                an.cmp(bn)
            })
        });

        out
    }
}

impl WidgetRef for &AgentMentionPopup {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let (list_area, hint_area) = if area.height > 2 {
            let [list_area, _spacer_area, hint_area] = Layout::vertical([
                Constraint::Length(area.height - 2),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(area);
            (list_area, Some(hint_area))
        } else {
            (area, None)
        };
        let rows = self.rows_from_matches(self.filtered());
        render_rows_single_line(
            list_area.inset(Insets::tlbr(0, 2, 0, 0)),
            buf,
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            "no agents",
        );
        if let Some(hint_area) = hint_area {
            let hint_area = Rect {
                x: hint_area.x + 2,
                y: hint_area.y,
                width: hint_area.width.saturating_sub(2),
                height: hint_area.height,
            };
            agent_popup_hint_line().render(hint_area, buf);
        }
    }
}

fn agent_popup_hint_line() -> Line<'static> {
    Line::from(vec![
        "Press ".into(),
        key_hint::plain(KeyCode::Enter).into(),
        " to select or ".into(),
        key_hint::plain(KeyCode::Esc).into(),
        " to close".into(),
    ])
}
