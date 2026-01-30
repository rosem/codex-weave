# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

Additional commands implemented in this repo:
- `/weave`: Opens a Weave-backed menu to set your agent name, manage sessions, and configure/run local workflow files (requires the local Weave daemon).
- `#`: While a Weave session is selected, typing `#` shows the agent list for mention-style insertion. Messages that include one or more agent mentions are relayed through the local agent (conversation owner), which then forwards to the mentioned agents.
- `#agent /new`, `#agent /compact`, `#agent /interrupt`, or `#agent /review <instructions>`: Send a Weave control command to a mentioned agent without opening a popup. Review instructions are optional; when omitted, the review defaults to uncommitted changes.
- Control commands apply to the agent's current Weave task (conversation owned by the sender). They do not interrupt a task the target started locally.
- Inbound Weave messages are automatically submitted to the model; direct messages send the final response back to the sender.
