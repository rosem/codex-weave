use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::WidgetRef;

use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::render_rows_single_line;
use crate::render::Insets;
use crate::render::RectExt;
use crate::text_formatting::truncate_text;
use crate::weave_client;
use crate::weave_client::WeaveAgent;
use codex_common::fuzzy_match::fuzzy_match;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WeaveAgentPopupAction {
    Agent { mention: String },
    Rename { name: String },
    Separator,
}

pub(crate) struct WeaveAgentPopup {
    query: String,
    agents: Vec<WeaveAgent>,
    self_agent_id: Option<String>,
    state: ScrollState,
}

impl WeaveAgentPopup {
    pub(crate) fn new(agents: Vec<WeaveAgent>, self_agent_id: Option<String>) -> Self {
        Self {
            query: String::new(),
            agents,
            self_agent_id,
            state: ScrollState::new(),
        }
    }

    pub(crate) fn set_agents(&mut self, agents: Vec<WeaveAgent>) {
        self.agents = agents;
        self.clamp_selection();
    }

    pub(crate) fn set_agent_identity(&mut self, self_agent_id: Option<String>) {
        if self.self_agent_id == self_agent_id {
            return;
        }
        self.self_agent_id = self_agent_id;
        self.clamp_selection();
    }

    pub(crate) fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.clamp_selection();
    }

    pub(crate) fn calculate_required_height(&self, _width: u16) -> u16 {
        let rows = self.rows();
        let visible = rows.len().clamp(1, MAX_POPUP_ROWS);
        visible as u16
    }

    pub(crate) fn move_up(&mut self) {
        let actions = self.actions();
        let len = actions.len();
        self.state.move_up_wrap(len);
        self.skip_non_selectable(&actions, -1);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    pub(crate) fn move_down(&mut self) {
        let actions = self.actions();
        let len = actions.len();
        self.state.move_down_wrap(len);
        self.skip_non_selectable(&actions, 1);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    pub(crate) fn selected_action(&self) -> Option<WeaveAgentPopupAction> {
        let matches = self.actions();
        let idx = self.state.selected_idx?;
        matches.get(idx).cloned().filter(Self::is_selectable)
    }

    fn clamp_selection(&mut self) {
        let actions = self.actions();
        let len = actions.len();
        self.state.clamp_selection(len);
        self.skip_non_selectable(&actions, 1);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn rows_from_matches(
        &self,
        matches: Vec<(usize, Option<Vec<usize>>, i32)>,
    ) -> Vec<GenericDisplayRow> {
        matches
            .into_iter()
            .map(|(idx, indices, _score)| {
                let agent = &self.agents[idx];
                let display_name = agent.display_name();
                let name = truncate_text(&display_name, 24);
                let id = agent.id.as_str();
                let description =
                    (display_name != id).then(|| truncate_text(&format!("id: {id}"), 36));
                GenericDisplayRow {
                    name,
                    name_style: None,
                    match_indices: indices,
                    display_shortcut: None,
                    description,
                    wrap_indent: None,
                }
            })
            .collect()
    }

    fn rename_candidate(&self) -> Option<String> {
        let name = self.query.trim();
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }

    fn is_selectable(action: &WeaveAgentPopupAction) -> bool {
        !matches!(action, WeaveAgentPopupAction::Separator)
    }

    fn skip_non_selectable(&mut self, actions: &[WeaveAgentPopupAction], direction: isize) {
        let len = actions.len();
        if len == 0 {
            self.state.selected_idx = None;
            return;
        }
        let Some(mut idx) = self.state.selected_idx else {
            return;
        };
        if Self::is_selectable(&actions[idx]) {
            return;
        }
        for _ in 0..len {
            idx = if direction >= 0 {
                idx.saturating_add(1) % len
            } else if idx == 0 {
                len.saturating_sub(1)
            } else {
                idx.saturating_sub(1)
            };
            if Self::is_selectable(&actions[idx]) {
                self.state.selected_idx = Some(idx);
                return;
            }
        }
        self.state.selected_idx = None;
    }

    fn actions(&self) -> Vec<WeaveAgentPopupAction> {
        let matches = self.filtered();
        let rename = self.rename_candidate();
        let has_primary = !matches.is_empty() || rename.is_some();
        let mention_tokens = weave_client::weave_mention_tokens(&self.agents);
        let mut actions = Vec::with_capacity(matches.len() + 2);
        for (idx, _, _) in matches {
            let agent = &self.agents[idx];
            let mention = mention_tokens
                .get(&agent.id)
                .cloned()
                .unwrap_or_else(|| agent.mention_text());
            actions.push(WeaveAgentPopupAction::Agent { mention });
        }
        if let Some(name) = rename {
            actions.push(WeaveAgentPopupAction::Rename { name });
        }
        if has_primary {
            actions.push(WeaveAgentPopupAction::Separator);
        }
        actions
    }

    fn rows(&self) -> Vec<GenericDisplayRow> {
        let matches = self.filtered();
        let rename = self.rename_candidate();
        let has_primary = !matches.is_empty() || rename.is_some();
        let mut rows = Vec::with_capacity(matches.len() + 2);
        rows.extend(self.rows_from_matches(matches));
        if let Some(name) = rename {
            let label = format!("rename agent to \"{name}\"");
            rows.push(GenericDisplayRow {
                name: truncate_text(&label, 36),
                name_style: None,
                match_indices: None,
                display_shortcut: None,
                description: None,
                wrap_indent: None,
            });
        }
        if has_primary {
            rows.push(GenericDisplayRow {
                name: String::new(),
                name_style: None,
                match_indices: None,
                display_shortcut: None,
                description: None,
                wrap_indent: None,
            });
        }
        rows
    }

    fn filtered(&self) -> Vec<(usize, Option<Vec<usize>>, i32)> {
        let filter = self.query.trim();
        let mut out: Vec<(usize, Option<Vec<usize>>, i32)> = Vec::new();
        let self_id = self.self_agent_id.as_deref();

        if filter.is_empty() {
            for (idx, agent) in self.agents.iter().enumerate() {
                if self_id == Some(agent.id.as_str()) {
                    continue;
                }
                out.push((idx, None, 0));
            }
            return out;
        }

        for (idx, agent) in self.agents.iter().enumerate() {
            if self_id == Some(agent.id.as_str()) {
                continue;
            }
            let display_name = agent.display_name();
            if let Some((indices, score)) = fuzzy_match(&display_name, filter) {
                out.push((idx, Some(indices), score));
            }
        }

        out.sort_by(|a, b| {
            a.2.cmp(&b.2).then_with(|| {
                let an = self.agents[a.0].display_name();
                let bn = self.agents[b.0].display_name();
                an.cmp(&bn)
            })
        });

        out
    }
}

impl WidgetRef for WeaveAgentPopup {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let rows = self.rows();
        render_rows_single_line(
            area.inset(Insets::tlbr(0, 2, 0, 0)),
            buf,
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            "no agents",
        );
    }
}
