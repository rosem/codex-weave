use std::collections::HashSet;
use std::fmt::Write;

use codex_protocol::models::DeveloperInstructions;
use codex_protocol::models::ResponseItem;
use codex_protocol::user_input::UserInput;

pub(crate) fn build_agent_mention_instructions(inputs: &[UserInput]) -> Option<ResponseItem> {
    let mut seen = HashSet::new();
    let mut mentions = Vec::new();

    for input in inputs {
        if let UserInput::AgentMention { alias, thread_id } = input {
            if seen.insert(*thread_id) {
                mentions.push((alias.as_str(), *thread_id));
            }
        }
    }

    if mentions.is_empty() {
        return None;
    }

    let mut text = String::from("<agent_mentions>\n");
    text.push_str(
        "Use send_input as needed to satisfy the user's instructions for the following agent thread IDs:\n",
    );
    for (alias, thread_id) in &mentions {
        let _ = writeln!(&mut text, "- {thread_id} (alias: {alias})");
    }
    text.push_str(
        "You may send multiple messages to the same ID if the prompt requires it. Do not spawn agents to satisfy these mentions. If send_input fails, report the error.\n",
    );
    text.push_str("</agent_mentions>");

    Some(ResponseItem::from(DeveloperInstructions::new(text)))
}

#[cfg(test)]
mod tests {
    use super::build_agent_mention_instructions;
    use codex_protocol::ThreadId;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::user_input::UserInput;
    use pretty_assertions::assert_eq;

    #[test]
    fn builds_instructions_for_mentions() {
        let first =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").expect("thread id");
        let second =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").expect("thread id");

        let inputs = vec![
            UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            },
            UserInput::AgentMention {
                alias: "Writer".to_string(),
                thread_id: first,
            },
            UserInput::AgentMention {
                alias: "Writer".to_string(),
                thread_id: first,
            },
            UserInput::AgentMention {
                alias: "Critic".to_string(),
                thread_id: second,
            },
        ];

        let item = build_agent_mention_instructions(&inputs).expect("instructions");
        let ResponseItem::Message { role, content, .. } = item else {
            panic!("expected developer message");
        };
        assert_eq!(role, "developer");
        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("expected single InputText content item");
        };
        let expected = "\
<agent_mentions>
Use send_input as needed to satisfy the user's instructions for the following agent thread IDs:
- 00000000-0000-0000-0000-000000000001 (alias: Writer)
- 00000000-0000-0000-0000-000000000002 (alias: Critic)
You may send multiple messages to the same ID if the prompt requires it. Do not spawn agents to satisfy these mentions. If send_input fails, report the error.
</agent_mentions>";
        assert_eq!(text, expected);
    }

    #[test]
    fn skips_when_no_mentions() {
        let inputs = vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }];
        assert_eq!(build_agent_mention_instructions(&inputs), None);
    }
}
