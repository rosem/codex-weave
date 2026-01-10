# Weave

Weave is a fork of the Codex CLI with built-in agent-to-agent coordination. It keeps the standard Codex experience and adds Weave sessions, agent naming, and relay workflows.

## Quickstart (npm, macOS)

1. Install the CLI:

```sh
npm install -g @rosem_soo/weave
```

2. Start the coordinator:

```sh
weave-service start
```

3. Run the CLI:

```sh
weave
```

4. Stop the coordinator when finished:

```sh
weave-service stop
```

## Quickstart (manual weave)

1. Start the Weave coordinator in a separate terminal:

```sh
./weave
```

2. Start the CLI:

```sh
cargo run -p codex-cli --
```

3. Use `/weave` to join a session and `#agent` to relay tasks.

## Weave commands

### `/weave`

Opens the Weave session menu. From there you can:

- Set your agent name (shown to other agents).
- Create a new session.
- Join/leave a session (the active one is marked with a check).
- Close an existing session.

You need to join a session before agent mentions will work.

### `#agent`

Use `#` mentions in a prompt to relay tasks to other agents in the current
session. Type `#` to open the agent picker and insert a mention, or type the
agent ID/name directly:

```text
#alex Please investigate the failing tests.
#alex #bryn Review the PR and summarize changes.
```

Notes:

- Mentions must be standalone tokens (space-separated). Avoid punctuation
  immediately after the mention (use `#alex` not `#alex,`).
- Agent names with spaces require using the agent ID (the picker shows IDs).
- You can rename your agent from the picker by typing a new name after `#`
  and selecting the rename action.

## Weave coordinator notes

- Default socket: `~/.weave/coord.sock`
- Override with `WEAVE_HOME=/path/to/dir`
- Multiple Codex instances can share one coordinator
- Deleting `~/.weave` clears session state; restart the coordinator afterward

## Bundled Weave binary

If you bundle Weave with the CLI, place it here:

```
codex-cli/vendor/<platform>/weave/weave
```

Run that binary directly for manual start/stop, or use `weave-service start`
and `weave-service stop` on macOS. The CLI does not auto-start Weave yet.

## Repo layout

- `codex-rs/` - Rust implementation of Codex CLI
- `docs/weave/` - Weave protocol and deployment notes

## Upstream

For base Codex documentation, see the upstream repository:
https://github.com/openai/codex
