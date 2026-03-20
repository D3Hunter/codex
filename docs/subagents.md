# Sub-Agents in Codex (Implementation Notes)

Last updated: 2026-03-20

This document summarizes how sub-agents work in this repository, with pointers to the main code paths.

## What a "sub-agent" is here

In this codebase, a sub-agent is another Codex thread (another `ThreadId`) managed by the same runtime, not a separate service.

- Control plane: `core/src/agent/control.rs`
- Thread spawning/runtime: `core/src/thread_manager.rs`, `core/src/codex.rs`
- Tool API surface: `core/src/tools/spec.rs`, `core/src/tools/handlers/multi_agents/*`

## User-facing tool surface

When collaboration tools are enabled (`Feature::Collab`), the model can call:

- `spawn_agent`
- `send_input`
- `resume_agent`
- `wait_agent`
- `close_agent`

Where this is defined:

- Tool schemas and descriptions: `core/src/tools/spec.rs`
- Registration: `core/src/tools/spec.rs` (`build_specs`)
- Handlers: `core/src/tools/handlers/multi_agents/*.rs`

## Feature flags and defaults

- Collab feature key: `multi_agent` (`Feature::Collab`)
- Collab default: enabled
- Default max concurrent agent threads: `6`
- Default max spawn depth: `1`

Where this is defined:

- `core/src/features.rs`
- `core/src/config/mod.rs`
- Depth and thread guards: `core/src/agent/guards.rs`

## Lifecycle: spawn -> work -> wait -> close/resume

### 1) Spawn

`spawn_agent` handler (`core/src/tools/handlers/multi_agents/spawn.rs`) does:

- Parse/validate input:
  - exactly one of `message` or `items`
  - not empty
- Compute child depth from current `SessionSource`
- Enforce max depth
- Emit `CollabAgentSpawnBeginEvent`
- Build child config from parent turn
  - copies runtime-owned state (approval policy, sandbox policy, cwd, provider/model state)
  - applies requested model/reasoning overrides
  - applies optional role (`agent_type`)
- Ask `AgentControl` to create the child thread and send initial input
- Emit `CollabAgentSpawnEndEvent`
- Return `{ agent_id, nickname }`

### 2) Send additional input

`send_input` handler (`core/src/tools/handlers/multi_agents/send_input.rs`) does:

- Parse target agent id and input
- Optional `interrupt` before sending
- Emit begin event
- Queue input as `Op::UserInput` through `AgentControl`
- Emit end event with latest status
- Return `submission_id`

### 3) Wait

`wait_agent` handler (`core/src/tools/handlers/multi_agents/wait.rs`) does:

- Validate non-empty ids
- Clamp timeout to safe bounds (min 10s, max 1h)
- Emit waiting begin event
- Subscribe to each agent status stream
- Return once at least one target reaches final status or timeout
- Emit waiting end event with per-agent statuses
- Return `{ status: {id -> AgentStatus}, timed_out }`

### 4) Close

`close_agent` handler (`core/src/tools/handlers/multi_agents/close_agent.rs`) does:

- Emit close begin event
- Shutdown target thread (if not already shutdown)
- Remove thread from manager/guard accounting
- Emit close end event
- Return the latest status snapshot

### 5) Resume

`resume_agent` handler (`core/src/tools/handlers/multi_agents/resume_agent.rs`) does:

- If status is already not `NotFound`, it is effectively a no-op status read
- If `NotFound`, reconstruct from rollout with `AgentControl::resume_agent_from_rollout`
- Rehydrate nickname/role from state DB metadata when available
- Emit resume begin/end events

## What `AgentControl` is responsible for

`core/src/agent/control.rs` owns shared multi-agent operations per user session:

- Enforcing max-thread guard (`reserve_spawn_slot`)
- Reserving and tracking nicknames
- Spawning threads (or forked threads when `fork_context=true`)
- Resuming threads from rollout
- Sending input / interrupt / shutdown to child threads
- Status subscriptions
- Completion watcher that notifies parent thread

Important detail:

- For `fork_context=true`, parent rollout is materialized/flushed first, then child starts from forked rollout history plus a synthetic marker response item.

## Session source and metadata model

Sub-agent origin is captured in `SessionSource::SubAgent(SubAgentSource::...)`.

Important variants:

- `ThreadSpawn { parent_thread_id, depth, agent_nickname, agent_role }`
- `Review`
- `MemoryConsolidation`
- `Other(String)` (used by internal/background workers)

Where this is defined:

- `protocol/src/protocol.rs`

This metadata flows into:

- persisted thread/session metadata
- app-server thread summaries (`agentNickname`, `agentRole`)
- UI labels

## Status model

Agent lifecycle statuses:

- `PendingInit`
- `Running`
- `Completed(Option<String>)`
- `Errored(String)`
- `Shutdown`
- `NotFound`

Status transitions are event-driven (see `core/src/agent/status.rs`), and `wait_agent` treats all non-running/non-pending states as final.

## Event model and app-server projection

Core emits collab begin/end events for each operation:

- spawn begin/end
- interaction begin/end
- waiting begin/end
- close begin/end
- resume begin/end

Where this is defined:

- Event structs: `protocol/src/protocol.rs`
- Event -> thread item projection: `app-server-protocol/src/protocol/thread_history.rs`
- Live notifications mapping: `app-server/src/bespoke_event_handling.rs`

In app-server v2, these appear as `ThreadItem::CollabAgentToolCall` with:

- tool (`SpawnAgent`, `SendInput`, `Wait`, `CloseAgent`, `ResumeAgent`)
- lifecycle state (`InProgress`, `Completed`, `Failed`)
- sender/receiver thread ids
- optional prompt/model/reasoning metadata
- per-agent terminal status map (`agentsStates`)

## Parent-thread awareness of children

Two mechanisms help parent context:

- Environment context includes a `<subagents>` section listing active child ids/nicknames
  - `core/src/codex.rs`
  - `core/src/agent/control.rs` (`format_environment_context_subagents`)
- Child completion watcher injects a contextual `<subagent_notification>` user fragment to parent history
  - `core/src/agent/control.rs`
  - `core/src/session_prefix.rs`
  - `core/src/contextual_user_message.rs`

## Internal sub-agent usage (not only user tool calls)

Sub-agent runtime is also used internally:

- Review task spawns a review sub-agent with stricter settings
  - `core/src/tasks/review.rs`
- Memory phase 2 spawns a consolidation sub-agent
  - `core/src/memories/phase2.rs`
- Guardian approval reviewer uses a dedicated sub-agent session
  - `core/src/guardian/review_session.rs`

## Batch fanout variant

`spawn_agents_on_csv` (`core/src/tools/handlers/agent_jobs.rs`) is a higher-level fanout tool:

- Spawns one worker sub-agent per CSV row
- Tracks progress and emits `agent_job_progress:*` updates
- Expects each worker to call `report_agent_job_result` exactly once
- Exports combined results to CSV

This path uses sub-agents with `SessionSource::SubAgent(SubAgentSource::Other("agent_job:<id>"))`.

## Practical lookup map

If you need to modify behavior, start here:

- Tool contract text/schemas: `core/src/tools/spec.rs`
- Collab handler behavior: `core/src/tools/handlers/multi_agents/*.rs`
- Shared spawn/runtime config inheritance: `core/src/tools/handlers/multi_agents.rs`
- Core agent control plane: `core/src/agent/control.rs`
- Limits/guards: `core/src/agent/guards.rs`, `core/src/config/mod.rs`
- Protocol event types/statuses: `protocol/src/protocol.rs`
- App-server item projection: `app-server-protocol/src/protocol/thread_history.rs`
- App-server live notifications: `app-server/src/bespoke_event_handling.rs`
- TUI rendering for collab events: `tui/src/multi_agents.rs`
