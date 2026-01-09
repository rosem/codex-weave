# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

Additional commands implemented in this repo:
- `/weave`: Opens a Weave-backed menu to set your agent name and create/select/close sessions (requires the local Weave daemon).
- `#`: While a Weave session is selected, typing `#` shows the agent list for mention-style insertion. Messages that include one or more agent mentions are relayed through the local agent (conversation owner), which then forwards to the mentioned agents.
- Inbound Weave messages are automatically submitted to the model; direct messages send the final response back to the sender.
