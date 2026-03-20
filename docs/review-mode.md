# Review Mode (`/review`) In Codex App

Last updated: 2026-03-20

This note explains what `/review` does in this repository, how to trigger it in the app, and how it differs from ad-hoc prompting.

## Quick answer

- `/review` is an AI review workflow, not a human reviewer.
- It runs a dedicated review sub-agent session with a fixed rubric prompt.

## How to trigger it in the Codex app

1. Type `/review` and press Enter.
2. Pick a preset from the popup:
   - `Review against a base branch` (`PR Style`)
   - `Review uncommitted changes`
   - `Review a commit`
   - `Custom review instructions`
3. Or run inline custom review directly:
   - `/review <your instructions>`

Implementation pointers:
- Slash command registration: `codex-rs/tui/src/slash_command.rs`
- `/review` opens preset popup: `codex-rs/tui/src/chatwidget.rs`
- Inline `/review ...` maps to `ReviewTarget::Custom`: `codex-rs/tui/src/chatwidget.rs`

## What it can review

`/review` supports these targets:

- `UncommittedChanges`: staged, unstaged, and untracked changes
- `BaseBranch { branch }`: PR-style review against a base branch
- `Commit { sha, title }`: one specific commit
- `Custom { instructions }`: free-form instructions

Implementation pointers:
- Review target types: `codex-rs/protocol/src/protocol.rs`
- Target-specific prompt generation: `codex-rs/core/src/review_prompts.rs`

For base-branch mode, the system computes merge-base with `HEAD` and asks the reviewer to inspect `git diff <merge-base>`.

## What "dedicated review rubric prompt" means

Review mode always sets a fixed review rubric as base instructions from:

- `codex-rs/core/review_prompt.md`

Loaded by:

- `codex-rs/core/src/client_common.rs` (`REVIEW_PROMPT`)
- `codex-rs/core/src/tasks/review.rs` (sets `base_instructions` for the review sub-agent)

The rubric includes:

- rules for when something should be flagged as a bug
- comment-writing constraints
- priority semantics (`P0` to `P3`)
- strict JSON output contract with code locations and overall verdict

## Difference vs writing your own checklist prompt

There are three patterns:

1. `/review` with a preset target
- Uses dedicated rubric + target-specific prompt
- Produces structured review output (`findings`, `overall_correctness`, confidence, file/line ranges)

2. `/review <your checklist/instructions>`
- Still uses dedicated rubric
- Your checklist becomes the review target instructions (`ReviewTarget::Custom`)
- Still expects structured output

3. Normal chat message ("please review this with checklist X")
- Does not enter review-mode lifecycle
- No enforced review output contract
- Usually more free-form than `/review`

## Runtime behavior and constraints

The review sub-agent is intentionally constrained:

- review source is `SubAgentSource::Review`
- web search is disabled
- multi-agent delegation (`Collab`) and CSV fanout are disabled
- approval policy is forced to `Never`
- uses `review_model` override when configured, otherwise current model

Implementation pointers:
- Review task and sub-agent setup: `codex-rs/core/src/tasks/review.rs`
- Review model config field: `codex-rs/core/src/config/mod.rs` (`review_model`)

## Output shape

Structured output is represented as `ReviewOutputEvent`:

- `findings: Vec<ReviewFinding>`
- `overall_correctness: String`
- `overall_explanation: String`
- `overall_confidence_score: f32`

Each finding includes:

- `title`, `body`, `confidence_score`, `priority`
- `code_location.absolute_file_path`
- `code_location.line_range.start/end`

Implementation pointers:
- Schema: `codex-rs/protocol/src/protocol.rs`

## What it does not do

- It does not automatically post GitHub PR comments by itself.
- It performs local review over repository state and emits review output in Codex.

