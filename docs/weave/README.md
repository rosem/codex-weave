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

## Service helper (macOS)

If you installed the npm package, you can manage the coordinator with:

```sh
weave-service start
weave-service stop
```

This uses `WEAVE_HOME` (default `~/.weave`) and writes a log file to
`$WEAVE_HOME/weave-service.log`.

## Bundled binary (repo layout)

If you bundle Weave with the CLI, place it under:

```
codex-cli/vendor/<platform>/weave/weave
```

Run that binary directly for manual start/stop, or use `weave-service start`
and `weave-service stop` on macOS.

## Notes

- Multiple Codex instances can share a single coordinator.
- Deleting `~/.weave` removes session state; restart the coordinator to recreate
  the directory and socket.
