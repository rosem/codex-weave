# Weave Hybrid Plan (Go bus + Rust runtime)

Goal: Keep the Go weave coordinator as the bus, port weave-2 server behavior into
Rust for correctness, and expose an HTTP/SSE facade so the web UI uses the same
runtime as the CLI.

## Full plan (overview)
Architecture layering:
- Bus: Go weave coordinator (Unix socket, session/agent registry, delivery, acks).
- Runtime: Rust orchestration (ownership rules, relay policy, loop control).
- Transports: UDS adapter to the bus, HTTP/SSE facade for web/SDK.
- Clients: codex CLI (tui/tui2), optional web UI.

Data flow:
- CLI -> runtime -> bus -> runtime -> other CLI.
- Web -> HTTP -> runtime -> bus -> runtime -> SSE -> web.

Guiding principles:
- One shared state machine (runtime) for both CLI and web.
- Stable, documented API contract; transports are interchangeable.
- Keep bus minimal; put behavior and policy above it.

Deliverables:
- docs/weave/protocol.md and docs/weave/http-api.md as the shared contract.
- codex-weave-runtime crate with ownership/relay/loop rules.
- UDS + HTTP transports that map into the same runtime events.
- CLI uses runtime client, web uses HTTP/SSE.
- One daemon to start bus + runtime + HTTP.

## Phase 0 - Discovery and alignment
- [ ] Inventory weave-2 server behavior (ownership rules, relay policy, loop control).
- [ ] Inventory current CLI weave usage and gaps vs weave-2 behavior.
- [ ] Identify must-keep semantics in the Go coordinator (acks, ordering, retries).
      - Capture required envelopes and event ordering guarantees.

## Phase 1 - Documentation and contracts
- [ ] Write docs/weave/protocol.md describing:
      - Bus envelopes, message kinds, ids, and ack semantics.
      - Runtime-level concepts: ownership, reply, intent, loop guard.
- [ ] Write docs/weave/http-api.md with the API surface for sessions, agents,
      messages, events, and streaming endpoints.
      - Include auth, pagination, and SSE event names.

## Phase 2 - Rust runtime (codex-rs)
- [ ] Add crate codex-weave-runtime for shared orchestration logic.
- [ ] Implement conversation ownership and relay policy (single owner).
- [ ] Handle ownership state edge case (clear pending reply target and avoid stale
      relay prompt on subsequent turns).
- [ ] Implement loop guard and reply suppression rules.
- [ ] Normalize incoming/outgoing events into a single internal model.
      - Map bus events to runtime events with stable ids.
      - Define message kinds: user, reply, control, system.

## Phase 3 - Transports
- [ ] Add codex-weave-transport-uds to talk to the Go coordinator over
      ~/.weave/coord.sock (or WEAVE_HOME).
- [ ] Add codex-weave-transport-http for HTTP + SSE endpoints.
- [ ] Ensure both transports map to the same runtime events and types.
      - Add reconnection and replay for SSE.

## Phase 4 - CLI integration
- [ ] Replace direct weave client calls with the runtime client.
- [ ] Ensure /weave and # popups use runtime-provided agent/session state.
- [ ] Confirm end-to-end message delivery with correct owner relay behavior.
      - Provide a default agent identity and owner-based relay path.
- [ ] React to bus-driven updates (agent.updated, session closed) so all tabs
      refresh without manual polling.

## Phase 5 - Web integration
- [ ] Move weave-2/web to weave-web/ in this repo.
- [ ] Update UI to target the new HTTP/SSE API (no direct DB access).
- [ ] Ensure sessions and agents reflect CLI state via the shared runtime.
      - Keep UI-only features (sorting, filtering) in the client.

## Phase 6 - Packaging and deployment
- [ ] Provide a single weave-codex daemon that starts:
      - Go weave coordinator (if not already running).
      - Rust runtime + HTTP/SSE facade.
- [ ] Document startup modes (CLI-only vs CLI+web).
      - Provide logs and health endpoints for debugging.

## Phase 7 - Tests and verification
- [ ] Add integration tests for owner relay, loops, and message routing.
- [ ] Add transport compatibility tests (UDS vs HTTP).
- [ ] Add UI smoke tests against a test runtime instance.
      - Add golden tests for protocol versioning.
- [ ] Update legacy insta snapshots after the ownership changes (optional).

## Phase 8 - Optional future
- [ ] Evaluate replacing Go coordinator with Rust bus once parity is proven.
      - Keep transports stable to avoid client changes.
- [ ] Consider exposing queued weave tasks in the UI for visibility.
