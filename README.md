# weave-codex

weave-codex is a fork of the Codex CLI with built-in Weave agent-to-agent
coordination. It keeps the standard Codex experience and adds Weave sessions,
agent naming, and relay workflows.

## Quickstart (manual weave)

1. Start the Weave coordinator in a separate terminal:

```sh
./weave
```

2. Start the CLI:

```sh
cargo run -p codex-cli -- --enable tui2
```

3. Use `/weave` to join a session and `#agent` to relay tasks.

## Weave coordinator notes

- Default socket: `~/.weave/coord.sock`
- Override with `WEAVE_HOME=/path/to/dir`
- Multiple Codex instances can share one coordinator
- Deleting `~/.weave` clears session state; restart the coordinator afterward

## Bundled Weave binary

If you bundle Weave with the CLI, place it here:

```
codex-cli/vendor/weave/<platform>/weave
```

Run that binary directly for manual start/stop. The CLI does not auto-start
Weave yet.

## Repo layout

- `codex-rs/` - Rust implementation of Codex CLI
- `docs/weave/` - Weave protocol and deployment notes

## Upstream

For base Codex documentation, see the upstream repository:
https://github.com/openai/codex
