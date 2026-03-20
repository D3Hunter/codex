# Codex MCP Server HTTP Transport Plan

## Summary
Add MCP Streamable HTTP transport to `codex mcp-server` while preserving current stdio behavior and defaults.
Implementation will support concurrent HTTP sessions, local-only default binding, and health endpoints, using a shared business-logic path so stdio and HTTP stay behaviorally aligned.

## Execution Note
> "when impl some subtask of the plan, agent should update subtasks status and progress in the related part of plan file"

## Subtask Board

| ID | Subtask | Status | Progress |
| --- | --- | --- | --- |
| ST-1 | Add `--listen` to `codex mcp-server` CLI path | `Completed` | `100%` |
| ST-2 | Implement transport URL parser + parser unit tests | `Completed` | `100%` |
| ST-3 | Wire direct `codex-mcp-server` binary listen input | `Completed` | `100%` |
| ST-4 | Extract current stdio runtime into dedicated module | `Completed` | `100%` |
| ST-5 | Add runtime transport dispatch in `run_main` | `Completed` | `100%` |
| ST-6 | Add HTTP server skeleton + `/healthz` + `/readyz` | `Completed` | `100%` |
| ST-7 | Mount Streamable HTTP MCP endpoint path handling | `Completed` | `100%` |
| ST-8 | Add per-session runtime scaffold + pending map | `Not Started` | `0%` |
| ST-9 | Bridge response/error/notification message flow | `Not Started` | `0%` |
| ST-10 | Bridge approval requests + session cleanup/cancel | `Not Started` | `0%` |
| ST-11 | Update docs + CLI help for HTTP transport usage | `Not Started` | `0%` |
| ST-12 | Add/refresh tests and run required verification | `Not Started` | `0%` |

## Subtasks (Implement In Order)

Target size for each subtask: keep changes around `100-500` LoC where possible (code + tests + docs), and split further if a subtask grows beyond that.

### ST-1: Add `--listen` to `codex mcp-server` CLI path
- **Status:** `Completed`
- **Progress:** `100%`
- **Scope:** Add `--listen <URL>` to the multitool CLI subcommand surface only (`codex mcp-server`), preserving current default behavior.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/cli/src/main.rs`
  - `/Users/jujiajia/code/codex/codex-rs/cli/src/chatgpt_cli.rs` (if option wiring lives here)
- **Checklist:**
  - [x] Add `--listen` option to `mcp-server` command parsing.
  - [x] Preserve default launch behavior when option is omitted.
  - [x] Keep this subtask scoped to CLI plumbing only.
- **Done when:** `codex mcp-server --help` exposes `--listen` and behavior is unchanged without it.

### ST-2: Implement transport URL parser + parser unit tests
- **Status:** `Completed`
- **Progress:** `100%`
- **Scope:** Implement MCP listen transport enum/parser with strict URL validation and defaults.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/lib.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/transport.rs` (new)
- **Checklist:**
  - [x] Parse `stdio://` and `http://IP:PORT[/PATH]` with `/mcp` default path.
  - [x] Reject unsupported schemes and malformed URLs with clear errors.
  - [x] Add parser unit tests for valid/invalid cases.
- **Done when:** parser is independently tested and ready for CLI/runtime consumption.

### ST-3: Wire direct `codex-mcp-server` binary listen input
- **Status:** `Completed`
- **Progress:** `100%`
- **Scope:** Connect binary entrypoint to parsed listen transport with `stdio://` default semantics.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/main.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/lib.rs`
- **Checklist:**
  - [x] Accept parsed listen value from CLI/binary args.
  - [x] Ensure omitted listen still maps to stdio.
  - [x] Keep behavior changes minimal and isolated from runtime internals.
- **Done when:** binary startup path can pass selected transport into runtime.

### ST-4: Extract current stdio runtime into dedicated module
- **Status:** `Completed`
- **Progress:** `100%`
- **Scope:** Move current stdio processing loop into `stdio_runtime` module without behavior changes.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/lib.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/stdio_runtime.rs` (new)
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/main.rs`
- **Checklist:**
  - [x] Extract stdin reader / processor / stdout writer orchestration into module.
  - [x] Keep existing logging and channel behavior unchanged.
  - [ ] Keep tests green with no HTTP logic introduced yet. Blocked by pre-existing `mcp-server/src/message_processor.rs` compile failure for missing `AgentStatus::Interrupted` match coverage.
- **Done when:** stdio-only execution is functionally unchanged after extraction.

### ST-5: Add runtime transport dispatch in `run_main`
- **Status:** `Completed`
- **Progress:** `100%`
- **Scope:** Dispatch between stdio and HTTP runtime entrypoints using parsed transport.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/lib.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/stdio_runtime.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/http_runtime.rs` (stub/new)
- **Checklist:**
  - [x] Add top-level match/dispatch for transport variants.
  - [x] Keep stdio branch as default.
  - [x] Add minimal dispatch tests for runtime selection.
- **Done when:** runtime selection works and default remains stdio-safe.

### ST-6: Add HTTP server skeleton + `/healthz` + `/readyz`
- **Status:** `Completed`
- **Progress:** `100%`
- **Scope:** Implement HTTP server startup and health/readiness routes, but keep MCP bridge logic minimal.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/http_runtime.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/Cargo.toml` (if deps are added)
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/BUILD.bazel` (if Bazel deps/data update needed)
- **Checklist:**
  - [x] Start `axum` server on configured address.
  - [x] Add `/healthz` and `/readyz` returning HTTP 200.
  - [x] Keep endpoint wiring clean for later MCP handler injection.
- **Done when:** server boots and health endpoints work independently.

### ST-7: Mount Streamable HTTP MCP endpoint path handling
- **Status:** `Completed`
- **Progress:** `100%`
- **Scope:** Add Streamable HTTP MCP route wiring with configured endpoint path and `/mcp` default behavior.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/http_runtime.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/transport.rs`
- **Checklist:**
  - [x] Wire `rmcp::transport::StreamableHttpService` + `LocalSessionManager`.
  - [x] Mount handler at parsed path.
  - [x] Verify path-default logic is respected.
- **Done when:** HTTP MCP endpoint accepts initialize handshake requests.

### ST-8: Add per-session runtime scaffold + pending map
- **Status:** `Not Started`
- **Progress:** `0%`
- **Scope:** For each HTTP session, create isolated processor/outgoing bridge state and pending response bookkeeping.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/http_runtime.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/session_runtime.rs` (new)
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/message_processor.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/outgoing_message.rs`
- **Checklist:**
  - [ ] Create per-session `MessageProcessor` and outgoing channel bridge.
  - [ ] Maintain pending-response map keyed by synthetic request IDs per session.
  - [ ] Keep session state isolated so concurrent traffic cannot collide.
- **Done when:** concurrent sessions are isolated at runtime state level.

### ST-9: Bridge response/error/notification message flow
- **Status:** `Not Started`
- **Progress:** `0%`
- **Scope:** Complete routing for responses/errors/notifications from core runtime to HTTP peer.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/http_runtime.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/session_runtime.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/outgoing_message.rs`
- **Checklist:**
  - [ ] Route `OutgoingMessage::Response/Error` to matching pending waiter.
  - [ ] Route `OutgoingMessage::Notification` to peer custom notification channel.
  - [ ] Add focused tests for these mappings.
- **Done when:** response and notification routing is deterministic and tested.

### ST-10: Bridge approval requests + session cleanup/cancel
- **Status:** `Not Started`
- **Progress:** `0%`
- **Scope:** Handle outbound request/approval round-trips and ensure session shutdown/cancellation cleanup works.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/http_runtime.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/session_runtime.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/message_processor.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/exec_approval.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/patch_approval.rs`
- **Checklist:**
  - [ ] Forward `OutgoingMessage::Request` to client custom request channel and route response back into callback flow.
  - [ ] Preserve existing non-standard approval shape (`{ decision: ... }`) for compatibility.
  - [ ] On session shutdown, bounded-shutdown active session-created threads.
  - [ ] Keep cancellation wired to current `notifications/cancelled` behavior.
- **Done when:** approval-style request flow works over HTTP and session teardown does not leak active work.

### ST-11: Update docs + CLI help for HTTP transport usage
- **Status:** `Not Started`
- **Progress:** `0%`
- **Scope:** Update user-facing docs and command help for `--listen`, defaults, endpoint path, and health routes.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/docs/codex_mcp_interface.md`
  - `/Users/jujiajia/code/codex/codex-rs/README.md`
  - `/Users/jujiajia/code/codex/codex-rs/cli/src/main.rs`
- **Checklist:**
  - [ ] Document `stdio://` default and `http://IP:PORT[/PATH]` usage.
  - [ ] Document `/healthz` and `/readyz`.
  - [ ] Keep docs aligned with final runtime behavior.
- **Done when:** docs/help are accurate and runnable as written.

### ST-12: Add/refresh tests and run required verification
- **Status:** `Not Started`
- **Progress:** `0%`
- **Scope:** Complete unit/integration/regression test coverage and required formatting/test commands.
- **Primary files (expected):**
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/tests/suite/codex_tool.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/tests/common/mcp_process.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/tests/all.rs`
  - `/Users/jujiajia/code/codex/codex-rs/mcp-server/src/transport.rs` (unit tests)
  - `/Users/jujiajia/code/codex/codex-rs/cli/src/main.rs` (CLI parse tests)
- **Checklist:**
  - [ ] Add unit tests for transport URL parsing and CLI argument parsing.
  - [ ] Add HTTP integration tests: initialize, `tools/list`, concurrent sessions, `tools/call` happy path, `/healthz`, `/readyz`.
  - [ ] Confirm existing stdio integration tests continue to pass unchanged.
  - [ ] Run required formatting and crate-level tests for changed projects; ask user before full workspace test run if common/core/protocol changes require it.
- **Done when:** all required unit/integration/regression tests and formatting checks for touched crates pass.

## Key Changes
- **Public CLI/API surface**
  - Add `--listen <URL>` to both `codex mcp-server` and direct `codex-mcp-server`.
  - Supported URLs:
    - `stdio://` (default)
    - `http://IP:PORT[/PATH]` (Streamable HTTP endpoint; default path `/mcp` if omitted)
  - Keep stdio as default so existing MCP launchers do not break.
  - Add `/healthz` and `/readyz` HTTP endpoints.

- **Transport architecture**
  - Introduce a transport enum/parser in MCP server crate (similar style to app-server transport parsing).
  - Keep current stdio runtime path intact for regression safety.
  - Add HTTP runtime path using `axum` + `rmcp::transport::StreamableHttpService` + `LocalSessionManager`.
  - Use one per-session runtime instance to support concurrent sessions safely (no cross-session `RequestId` collisions).

- **Session runtime and message routing**
  - Reuse existing `MessageProcessor` + `OutgoingMessageSender` for core behavior.
  - For each HTTP session, create:
    - A `MessageProcessor`
    - A channel-backed outgoing bridge task
    - A pending-response map for synthetic request IDs
  - Bridge behavior:
    - `OutgoingMessage::Response/Error` -> fulfill pending response waiter for the originating HTTP RPC.
    - `OutgoingMessage::Notification` -> forward to MCP client via peer custom notification.
    - `OutgoingMessage::Request` (e.g. approvals) -> forward to client custom request, then feed response back into existing callback flow.
  - Preserve current non-standard approval payload contract (`{ decision: ... }`) in this phase.

- **Lifecycle and cleanup**
  - Add session-shutdown cleanup so active threads created by that session are bounded-shutdown when the HTTP session ends.
  - Keep cancellation handling wired via existing `notifications/cancelled` flow.
  - Keep stdio semantics unchanged.

- **Docs and command help**
  - Update MCP server docs and quickstart:
    - `/Users/jujiajia/code/codex/codex-rs/docs/codex_mcp_interface.md`
    - `/Users/jujiajia/code/codex/codex-rs/README.md`
  - Update CLI help text for MCP server subcommand in:
    - `/Users/jujiajia/code/codex/codex-rs/cli/src/main.rs`

## Test Plan
- **Unit tests**
  - Transport URL parsing for MCP server:
    - default stdio
    - valid HTTP URL with and without explicit path
    - invalid schemes/URLs rejected
  - CLI parse tests for new `mcp-server --listen` option in multitool CLI.
- **Integration tests (mcp-server crate)**
  - HTTP initialize + `tools/list` success through Streamable HTTP endpoint.
  - Concurrent HTTP sessions can both initialize and call `tools/list` without cross-session interference.
  - One end-to-end `tools/call` happy-path over HTTP (no approval branch), validating event/response flow.
  - `/healthz` and `/readyz` return HTTP 200.
- **Regression tests**
  - Existing stdio integration suite continues to pass unchanged.

## Assumptions and Defaults
- Default HTTP exposure is local-only (`127.0.0.1`) with no built-in auth in v1.
- Concurrent sessions are required and will be session-isolated.
- Existing non-standard elicitation response shape is intentionally preserved for compatibility in this transport rollout.
- `serverInfo.user_agent` custom field remains guaranteed on current stdio path; HTTP path should be validated and documented if serialization constraints prevent identical extra-field behavior.
