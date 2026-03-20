# `codex-status` MCP Tool Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a new `codex-status` MCP tool that returns compact per-thread status information focused on token usage and runtime state.

**Architecture:** Extend `codex-mcp-server` with a third tool (`codex-status`) and fetch status directly from loaded `CodexThread` instances through a small `codex-core` accessor for token snapshot data. Return both human-readable text and structured JSON in `CallToolResult`.

**Tech Stack:** Rust, `codex-mcp-server`, `codex-core`, `rmcp`, existing MCP integration tests.

---

## Summary

- Add a new MCP tool named `codex-status` in `codex mcp-server` that returns a compact per-thread status snapshot.
- Scope is token-focused: agent lifecycle status + token usage + context-window usage (when available), intentionally less detailed than `/status`.
- The tool will work for currently loaded threads only (no implicit resume/loading from persisted rollout files).

## Key Implementation Changes

- **Core snapshot accessor:** add a read-only public accessor on `CodexThread` to fetch current token usage info (`Option<TokenUsageInfo>`) so `codex-mcp-server` can query status without maintaining its own cache; implement via `Codex` session state read path in `codex-rs/core/src/codex.rs` and `codex-rs/core/src/codex_thread.rs`.
- **New MCP tool schema:** in `codex-rs/mcp-server/src/codex_tool_config.rs`, add `CodexToolStatusParam` with `threadId` and deprecated `conversationId` alias, plus `create_tool_for_codex_tool_status_param()` and an output schema for structured status content.
- **Tool registration + dispatch:** in `codex-rs/mcp-server/src/message_processor.rs`, include `codex-status` in `tools/list`, add a `tools/call` branch, parse/validate params, resolve thread, and return a `CallToolResult` with:
  - `content`: concise text summary
  - `structuredContent`: `{ threadId, status, tokenUsage, contextWindow }`
  - `is_error: true` for invalid/missing thread inputs.
- **Response shape decisions (decision-complete):**
  - `status`: snake_case lifecycle string (`pending_init`, `running`, `completed`, `errored`, `shutdown`)
  - `tokenUsage`: nullable object with totals (`inputTokens`, `cachedInputTokens`, `outputTokens`, `reasoningOutputTokens`, `totalTokens`)
  - `contextWindow`: nullable object (`maxTokens`, `usedTokens`, `remainingTokens`, `remainingPercent`)
  - no model/cwd/approval/rate-limit fields.
- **Docs/API surface:** re-export the new status param type from `codex-rs/mcp-server/src/lib.rs` and update `codex-rs/docs/codex_mcp_interface.md` to document the third tool and its payload.

## Test Plan

- Add/extend unit tests in `codex-rs/mcp-server/src/codex_tool_config.rs` to snapshot-verify `codex-status` input/output schema.
- Add integration tests in `codex-rs/mcp-server/tests/suite/codex_tool.rs` for:
  - successful `codex-status` lookup after a `codex` call (assert status + token usage fields),
  - unknown thread id returns `is_error: true`,
  - `tools/list` includes `codex-status`.
- If needed, extend test helpers in `codex-rs/mcp-server/tests/common/mcp_process.rs` and `codex-rs/mcp-server/tests/common/responses.rs` to support generic tool calls and non-zero token SSE fixtures.
- Verification commands after implementation:
  - `just fmt` (in `codex-rs`)
  - `cargo test -p codex-core`
  - `cargo test -p codex-mcp-server`
  - because `core` changes are included, ask before running workspace-wide `cargo test` / `just test`.

## Assumptions and Defaults Locked

- Tool name is `codex-status`.
- Input supports `threadId` with backward-compatible `conversationId` alias.
- Lookup is in-memory loaded-thread only; missing thread id returns a clear error result.
- Output is intentionally compact and token-centric, not a full `/status` clone.
