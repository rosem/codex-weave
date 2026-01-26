# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

## TUI agent management

The TUI supports explicit, idle agent creation and tab-based switching.

- `/agents` opens the agent picker.
- `/agents new <name>` creates an idle agent with the given alias and switches to it.
- `/agents close` opens the close-agent picker (the main agent cannot be closed; use `/quit`).
- `Tab` cycles agents when the composer is empty and no modal/popup is active.
- `#Alias` expands to `#Alias(<thread_id>)` before sending so the active agent can call `send_input` (auto-complete opens after `#`; no tab switch).
- Agent aliases are session-only (not persisted on resume).

## Session scope

`/new` starts a new session for the active thread only; other agents remain available.
