use std::cell::RefCell;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::render::renderable::Renderable;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::standard_popup_hint_line;
use super::textarea::TextArea;
use super::textarea::TextAreaState;

const TITLE: &str = "New agent";
const PLACEHOLDER: &str = "Enter an alias and press Enter";
const INPUT_PAD: u16 = 2;

pub(crate) struct AgentAliasView {
    app_event_tx: AppEventSender,
    textarea: TextArea,
    textarea_state: RefCell<TextAreaState>,
    complete: bool,
}

impl AgentAliasView {
    pub(crate) fn new(app_event_tx: AppEventSender) -> Self {
        Self {
            app_event_tx,
            textarea: TextArea::new(),
            textarea_state: RefCell::new(TextAreaState::default()),
            complete: false,
        }
    }

    fn submit(&mut self) {
        let alias = self.textarea.text().trim().to_string();
        self.app_event_tx.send(AppEvent::CreateIdleAgent { alias });
        self.complete = true;
    }

    fn input_height(&self, width: u16) -> u16 {
        let usable_width = width.saturating_sub(INPUT_PAD);
        let text_height = self.textarea.desired_height(usable_width).clamp(1, 4);
        text_height.saturating_add(1).min(5)
    }
}

impl BottomPaneView for AgentAliasView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.on_ctrl_c();
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.submit();
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                self.textarea.input(key_event);
            }
            other => {
                self.textarea.input(other);
            }
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if pasted.is_empty() {
            return false;
        }
        self.textarea.insert_str(&pasted);
        true
    }
}

impl Renderable for AgentAliasView {
    fn desired_height(&self, width: u16) -> u16 {
        1u16 + self.input_height(width) + 2u16
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if area.height < 2 || area.width <= INPUT_PAD {
            return None;
        }
        let text_area_height = self.input_height(area.width).saturating_sub(1);
        if text_area_height == 0 {
            return None;
        }
        let textarea_rect = Rect {
            x: area.x.saturating_add(INPUT_PAD),
            y: area.y.saturating_add(2),
            width: area.width.saturating_sub(INPUT_PAD),
            height: text_area_height,
        };
        let state = *self.textarea_state.borrow();
        self.textarea.cursor_pos_with_state(textarea_rect, state)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let title_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        Paragraph::new(Line::from(TITLE.bold())).render(title_area, buf);

        let input_height = self.input_height(area.width);
        let input_area = Rect {
            x: area.x,
            y: area.y.saturating_add(1),
            width: area.width,
            height: input_height,
        };
        if input_area.width > INPUT_PAD {
            let text_area_height = input_area.height.saturating_sub(1);
            if text_area_height > 0 {
                let blank_rect = Rect {
                    x: input_area.x.saturating_add(INPUT_PAD),
                    y: input_area.y,
                    width: input_area.width.saturating_sub(INPUT_PAD),
                    height: 1,
                };
                Clear.render(blank_rect, buf);

                let textarea_rect = Rect {
                    x: input_area.x.saturating_add(INPUT_PAD),
                    y: input_area.y.saturating_add(1),
                    width: input_area.width.saturating_sub(INPUT_PAD),
                    height: text_area_height,
                };
                let mut state = self.textarea_state.borrow_mut();
                StatefulWidgetRef::render_ref(&(&self.textarea), textarea_rect, buf, &mut state);
                if self.textarea.text().is_empty() {
                    Paragraph::new(Line::from(PLACEHOLDER.dim())).render(textarea_rect, buf);
                }
            }
        }

        let hint_y = input_area.y.saturating_add(input_height).saturating_add(1);
        if hint_y < area.y.saturating_add(area.height) {
            Paragraph::new(standard_popup_hint_line()).render(
                Rect {
                    x: area.x,
                    y: hint_y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        }
    }
}
