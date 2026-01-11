# Weave Protocol (Hybrid Plan)

Status: draft. This document defines the shared contract between the bus,
runtime, and transports. It should be updated before implementations diverge.

## Scope
- Bus: session/agent registry, message delivery, ordering, action result propagation.
- Runtime: ownership/relay policy, loop control, relay output handling, normalization.
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
- task_id (string)
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
No backcompat: the bus uses a lean v1 envelope for all commands/events.

Envelope fields (v1):
- v, type, id, ts, src
- dst (optional), session (optional)
- seq (optional), idempotency_key (optional)
- corr (optional), payload (optional)
- status (optional), error (optional)

## Runtime concepts
Conversation ownership
- Each weave conversation has a single owner (agent id).
- The owner is the only agent allowed to issue follow-ups or relay tasks.
- Ownership is fixed for now; the schema allows adding transfer later.

Actions (action.submit)
- Issue ordered work with `action.submit`. The payload includes:
  - `group_id` (required)
  - `actions` array with per-target items (`message` or `control`)
  - optional `context` (`context_id`, `owner_id`, `parent_message_id`)
  - optional `task_id`
- Each action must include:
  - `dst` (required, per-action target)
  - `action_id` (required, unique per action)
  - `action_index` (required, reflects the original action order so `action.result` can be correlated
    after per-target fan-out)
  - `kind` (required for `message` actions: `user` or `reply`)
  - `reply_to_action_id` (optional for `message` actions; include when replying to a prior action)
- The coordinator delivers actions in order per target (no timers).
- Control commands: `new`, `compact`, `interrupt`, `review`.
- `/interrupt` should return `status = "completed"` when a task was interrupted and
  `status = "rejected"` when no task was running.

Action results
- Receivers emit `action.result` events:
- `{ "group_id": "...", "action_id": "...", "action_index": 0, "status": "accepted|completed|blocked|queued|rejected|error", "detail": "optional", "data": { "new_context_id": "...", "new_task_id": "..." } }`.
- Treat `completed`, `rejected`, and `error` as terminal; `accepted` is a non-terminal acknowledgment.
- `/new` includes `new_context_id`/`new_task_id` in `data` when available.

Message events
- Messages are emitted as `message.created` events with an inline `payload` object containing a
  non-empty `text`, plus optional `kind`, `context_id`, `owner_id`, `parent_message_id`, `task_id`,
  `reply_to_action_id`, and `sender_name`. `payload_ref` is not allowed for `message.created`.

Task events
- `task.updated`: `{ "task_id": "...", "status": "running|blocked|ready|done" }` (optional context fields).
- `task.done`: `{ "task_id": "...", "summary": "optional" }` (optional context fields).

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

## Relay output (runtime)
When the conversation owner is relaying a weave task, the runtime expects JSON-only relay output
from the model. Non-JSON output is treated as a local response and ends the relay.

Top-level output types:
- `relay_actions`: send one or more relay actions in a single turn.
  - Required: `actions` (array of action objects; must be non-empty).
- `task_done`: end the relay and summarize for the local user.
  - Optional: `summary` (string).

Supported actions:
- Each action targets a single agent via `dst`. Send multiple actions to reach multiple targets.
- `message`: send a message to a target.
  - Required: `dst` (agent id or name), `text`.
  - Optional: `plan` object with `steps` array (strings) and optional `note`.
- `control`: issue a control command to a target.
  - Required: `dst` (agent id or name), `command` (`new`, `interrupt`, `compact`, `review`).
  - Optional: `args` (string) for `/review` instructions.
- Order actions in the array to guarantee control happens before the next message for a target.

Examples:
- `{"type":"relay_actions","actions":[{"type":"message","dst":"worker","text":"...","plan":{"steps":["..."]}}]}`
- `{"type":"relay_actions","actions":[{"type":"control","dst":"worker","command":"new"}]}`
- `{"type":"relay_actions","actions":[{"type":"message","dst":"worker-a","text":"..."},{"type":"message","dst":"worker-b","text":"..."}]}`
- `{"type":"task_done","summary":"..."}`

## Compatibility notes
- UDS and HTTP must expose equivalent event streams and states.
- Runtime must normalize bus events into stable internal types.
- No timers: queued replies release only on explicit action/task events.

## TODO
- Specify exact event types emitted by runtime (e.g., session.update).
- Define idempotency keys and error propagation for HTTP.
- Define how agent rename propagates across transports.
