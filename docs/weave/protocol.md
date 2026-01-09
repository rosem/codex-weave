# Weave Protocol (Hybrid Plan)

Status: draft. This document defines the shared contract between the bus,
runtime, and transports. It should be updated before implementations diverge.

## Scope
- Bus: session/agent registry, message delivery, acking, ordering.
- Runtime: ownership/relay policy, loop control, intent handling, normalization.
- Transports: UDS and HTTP are two faces of the same protocol.

## Core entities
Session
- id (string)
- name (string, optional)
- status (open, closed)
- created_at, updated_at (ISO-8601)

Agent
- id (string)
- name (string, optional)
- status (optional)

Message
- id (string)
- session_id (string)
- src (agent id)
- dst (agent id, optional)
- kind (user, reply, control, system)
- text (string)
- conversation_id (string)
- conversation_owner (agent id)
- parent_message_id (optional)
- created_at (ISO-8601)
- reply_to (optional)

Event
- id (string)
- session_id (string)
- type (string)
- payload (object)
- created_at (ISO-8601)

## Bus envelope (UDS)
The bus uses the Go weave envelope. Runtime should not change bus semantics.

Envelope fields (existing):
- v, type, id, ts, src
- dst, topic, session, corr
- payload, ack, status, error

## Runtime concepts
Conversation ownership
- Each weave conversation has a single owner (agent id).
- The owner is the only agent allowed to issue follow-ups or relay tasks.
- Ownership is fixed for now; the schema allows adding transfer later.

Relay policy
- User messages can be relayed to targets.
- Replies should not be re-relayed by default to avoid loops.

Agent updates
- Agents may update their own display metadata (e.g., name) without reconnecting.
- Updates should be emitted as events so UIs can refresh live agent lists.
- Bus command: `agent.update` -> `agent.updated`.
  - Payload: `{ "id": "agent_id", "name": "new display name" }`
  - `id` MUST match `src`; name must be non-empty.

Loop guard
- Runtime should track reply chains and stop ping-pong loops.
- Prefer deterministic rules (kind-based suppression) over retries.

## Relay intents (runtime)
When the conversation owner is relaying a weave task, the runtime expects JSON-only intents from
the model. Non-JSON output is treated as a local response and ends the relay.

Supported intents:
- `message_agent`: send a message to a target.
  - Required: `dst` (agent id or name), `payload.input.text`.
  - For a new task, include a short `plan` array (strings). The plan should be
    included alongside the first `message_agent` payload, not as a standalone
    response.
- `task_done`: end the relay and summarize for the local user.
  - Optional: `summary` (string).

Examples:
- `{"intent":"message_agent","dst":"worker","payload":{"input":{"text":"..."}},"plan":["..."]}`
- `{"intent":"task_done","summary":"..."}`

## Compatibility notes
- UDS and HTTP must expose equivalent event streams and states.
- Runtime must normalize bus events into stable internal types.

## TODO
- Specify exact event types emitted by runtime (e.g., session.update).
- Define idempotency keys and ack behavior for HTTP.
- Define how agent rename propagates across transports.
