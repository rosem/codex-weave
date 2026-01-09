# Weave HTTP/SSE API (Hybrid Plan)

Status: draft. This is the proposed API surface for the runtime + HTTP facade.
The web UI and CLI (if desired) should rely on this contract.

## Base
- Default: http://localhost:<port>/api
- Port configured by a runtime env var (TBD).

## Conventions
- JSON request/response bodies.
- Idempotent create where possible (client-supplied id supported).
- SSE for streams; reconnect with Last-Event-ID where supported.
- Stub endpoints may return 501 until the runtime wiring is complete.

## Health
- GET /health

## Sessions
- GET /sessions
- POST /sessions
- GET /sessions/stream (SSE)
- GET /sessions/{id}
- POST /sessions/{id}/close

## Agents
- GET /sessions/{id}/agents
- POST /sessions/{id}/agents
- DELETE /sessions/{id}/agents/{agent_id}
- PATCH /sessions/{id}/agents/{agent_id}
- POST /sessions/{id}/agents/order

## Messages
- POST /sessions/{id}/messages
  - body: { text, agent_ids }
  - accepts a list of target agent ids, similar to weave-2.

## Events
- GET /sessions/{id}/events?limit=...
- GET /sessions/{id}/events/stream (SSE)

## SSE event types (proposed)
- sessions: list of sessions with agent counts
- events: session event payloads
- agents: agent list changes
- messages: inbound/outbound message events

## TODO
- Define auth (local-only vs token).
- Define pagination cursors for events and sessions.
- Define exact SSE event payload schema.
