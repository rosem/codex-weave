# Weave coordinator

Codex uses a separate Weave coordinator process for agent-to-agent messaging.
The CLI connects to a Unix domain socket at `~/.weave/coord.sock` by default,
or `WEAVE_HOME/coord.sock` if `WEAVE_HOME` is set.

## Manual start/stop

1. Start the coordinator in a separate terminal:

```sh
./weave
```

2. Leave it running while any Codex instances need Weave.
3. Stop it with `Ctrl+C` when you are done.

## Bundled binary (repo layout)

If you bundle Weave with the CLI, place it under:

```
codex-cli/vendor/weave/<platform>/weave
```

Run that binary directly for manual start/stop.

## Notes

- Multiple Codex instances can share a single coordinator.
- Deleting `~/.weave` removes session state; restart the coordinator to recreate
  the directory and socket.
