# Codex MCP Server Interface [experimental]

This document describes Codex's experimental MCP server interface: a JSON-RPC API that runs over the Model Context Protocol (MCP) transport to control a local Codex engine.

- Status: experimental and subject to change without notice
- Server binary: `codex mcp-server` (or `codex-mcp-server`)
- Transport: standard MCP over stdio by default (JSON-RPC 2.0, line-delimited). Optional Streamable HTTP mode is available via `--listen`.

## Overview

Codex exposes MCP-compatible methods to manage threads, turns, accounts, config, and approvals. The types live in `app-server-protocol/src/protocol/{common,v1,v2}.rs` and are consumed by the app server implementation in `app-server/`.

At a glance:

- Primary v2 RPCs
  - `thread/start`, `thread/resume`, `thread/fork`, `thread/read`, `thread/list`
  - `turn/start`, `turn/steer`, `turn/interrupt`
  - `account/read`, `account/login/start`, `account/login/cancel`, `account/logout`, `account/rateLimits/read`
  - `config/read`, `config/value/write`, `config/batchWrite`
  - `model/list`, `app/list`, `collaborationMode/list`
- Remaining v1 compatibility RPCs
  - `getConversationSummary`
  - `getAuthStatus`
  - `gitDiffToRemote`
  - `fuzzyFileSearch`, `fuzzyFileSearch/sessionStart`, `fuzzyFileSearch/sessionUpdate`, `fuzzyFileSearch/sessionStop`
- Notifications
  - v2 typed notifications such as `thread/started`, `turn/completed`, `account/login/completed`
  - `codex/event/*` stream notifications for live agent events
  - `fuzzyFileSearch/sessionUpdated`, `fuzzyFileSearch/sessionCompleted`
- Approvals (server -> client requests)
  - `applyPatchApproval`, `execCommandApproval`

See code for full type definitions and exact shapes: `app-server-protocol/src/protocol/{common,v1,v2}.rs`.

## Starting the server

Run Codex as an MCP server and connect an MCP client.

Default transport is stdio (pipe mode):

```bash
codex mcp-server | your_mcp_client
```

For Streamable HTTP transport, use `--listen`:

```bash
codex mcp-server --listen http://127.0.0.1:8080
```

This starts the MCP endpoint at `http://127.0.0.1:8080/mcp`. To customize the endpoint path:

```bash
codex mcp-server --listen http://127.0.0.1:8080/mcp/custom
```

Optional health probes for Streamable HTTP mode:

```bash
curl http://127.0.0.1:8080/healthz
curl http://127.0.0.1:8080/readyz
```

For a simple inspection UI, you can also try:

```bash
npx @modelcontextprotocol/inspector codex mcp-server
```

If you run HTTP mode for remote or shared-network use, bind explicitly to the desired interface and keep security requirements in mind. For local-only usage, prefer `127.0.0.1`:

```bash
codex mcp-server --listen http://127.0.0.1:8080
```

Use the separate `codex mcp` subcommand to manage configured MCP server launchers in `config.toml`.

## Threads and turns

Use the v2 thread and turn APIs for all new integrations. `thread/start` creates a thread, `turn/start` submits user input, `turn/interrupt` stops an in-flight turn, and `thread/list` / `thread/read` expose persisted history.

`getConversationSummary` remains as a compatibility helper for clients that still need a summary lookup by `conversationId` or `rolloutPath`.

For complete request and response shapes, see the app-server README and the protocol definitions in `app-server-protocol/src/protocol/v2.rs`.

## Models

Fetch the catalog of models available in the current Codex build with `model/list`. The request accepts optional pagination inputs:

- `limit` - number of models to return (defaults to a server-selected value)
- `cursor` - opaque string from the previous response's `nextCursor`

Each response yields:

- `data` - ordered list of models. A model includes:
  - `id`, `model`, `displayName`, `description`
  - `supportedReasoningEfforts` - array of objects with:
    - `reasoningEffort` - one of `none|minimal|low|medium|high|xhigh`
    - `description` - human-friendly label for the effort
  - `defaultReasoningEffort` - suggested effort for the UI
  - `inputModalities` - accepted input types for the model
  - `supportsPersonality` - whether the model supports personality-specific instructions
  - `isDefault` - whether the model is recommended for most users
  - `upgrade` - optional recommended upgrade model id
  - `upgradeInfo` - optional upgrade metadata object with:
    - `model` - recommended upgrade model id
    - `upgradeCopy` - optional display copy for the upgrade recommendation
    - `modelLink` - optional link for the upgrade recommendation
    - `migrationMarkdown` - optional markdown shown when presenting the upgrade
- `nextCursor` - pass into the next request to continue paging (optional)

## Collaboration modes (experimental)

Fetch the built-in collaboration mode presets with `collaborationMode/list`. This endpoint does not accept pagination and returns the full list in one response:

- `data` - ordered list of collaboration mode masks (partial settings to apply on top of the base mode)
  - For tri-state fields like `reasoning_effort` and `developer_instructions`, omit the field to keep the current value, set it to `null` to clear it, or set a concrete value to update it.

When sending `turn/start` with `collaborationMode`, `settings.developer_instructions: null` means "use built-in instructions for the selected mode".

## Event stream

While a conversation runs, the server sends notifications:

- `codex/event` with the serialized Codex event payload. The shape matches `core/src/protocol.rs`'s `Event` and `EventMsg` types. Some notifications include a `_meta.requestId` to correlate with the originating request.
- `fuzzyFileSearch/sessionUpdated` and `fuzzyFileSearch/sessionCompleted` for the legacy fuzzy search flow.

Clients should render events and, when present, surface approval requests (see next section).

## Tool responses

The server currently exposes three tools:

- `codex` - start a new Codex thread with an initial prompt.
- `codex-reply` - continue an existing thread by `threadId` (or deprecated `conversationId`).
- `codex-status` - fetch compact runtime status for a loaded thread with token and context-window usage.

The `codex` and `codex-reply` tools return standard MCP `CallToolResult` payloads. For compatibility with MCP clients that prefer `structuredContent`, Codex mirrors the content blocks inside `structuredContent` alongside the `threadId`.

Example:

```json
{
  "content": [{ "type": "text", "text": "Hello from Codex" }],
  "structuredContent": {
    "threadId": "019bbed6-1e9e-7f31-984c-a05b65045719",
    "content": "Hello from Codex"
  }
}
```

The `codex-status` tool also returns a normal `CallToolResult`, with status-focused structured content:

```json
{
  "content": [
    {
      "type": "text",
      "text": "status=completed; tokens total=321 input=321 cached_input=0 output=0 reasoning_output=0; context unavailable"
    }
  ],
  "structuredContent": {
    "threadId": "019bbed6-1e9e-7f31-984c-a05b65045719",
    "status": "completed",
    "tokenUsage": {
      "inputTokens": 321,
      "cachedInputTokens": 0,
      "outputTokens": 0,
      "reasoningOutputTokens": 0,
      "totalTokens": 321
    },
    "contextWindow": null
  }
}
```

Semantics:

- `tokenUsage` is cumulative for the loaded in-memory thread.
- `contextWindow.usedTokens` reflects the latest turn's context usage (the same value surfaced by the Codex app context window indicator), not the cumulative session total.

`codex-status` only inspects currently loaded in-memory threads; it does not implicitly resume threads from rollout files.

## Approvals (server -> client)

When Codex needs approval to apply changes or run commands, the server issues JSON-RPC requests to the client:

- `applyPatchApproval { conversationId, callId, fileChanges, reason?, grantRoot? }`
- `execCommandApproval { conversationId, callId, approvalId?, command, cwd, reason? }`

The client must reply with `{ decision: "allow" | "deny" }` for each request.

## Auth helpers

For the complete request/response shapes and flow examples, see the [Auth endpoints (v2) section in the app-server README](../app-server/README.md#auth-endpoints-v2).

## Legacy compatibility methods

The server still accepts a narrow v1 compatibility surface for existing app clients:

- `getConversationSummary`
- `getAuthStatus`
- `gitDiffToRemote`
- `fuzzyFileSearch`, `fuzzyFileSearch/sessionStart`, `fuzzyFileSearch/sessionUpdate`, `fuzzyFileSearch/sessionStop`

## Compatibility and stability

This interface is experimental. Method names, fields, and event shapes may evolve. For the authoritative schema, consult `app-server-protocol/src/protocol/{common,v1,v2}.rs` and the corresponding server wiring in `app-server/`.
