use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::path::PathBuf;

use codex_core::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use codex_mcp_server::CodexToolCallParam;
use codex_mcp_server::ExecApprovalElicitRequestParams;
use codex_mcp_server::ExecApprovalResponse;
use codex_mcp_server::PatchApprovalElicitRequestParams;
use codex_mcp_server::PatchApprovalResponse;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::ReviewDecision;
use codex_shell_command::parse_command;
use pretty_assertions::assert_eq;
use reqwest::StatusCode;
use rmcp::model::JsonRpcResponse;
use rmcp::model::JsonRpcVersion2_0;
use rmcp::model::RequestId;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

use core_test_support::skip_if_no_network;
use mcp_test_support::HttpMcpProcess;
use mcp_test_support::McpProcess;
use mcp_test_support::create_apply_patch_sse_response;
use mcp_test_support::create_final_assistant_message_sse_response;
use mcp_test_support::create_final_assistant_message_sse_response_with_tokens;
use mcp_test_support::create_mock_responses_server;
use mcp_test_support::create_shell_command_sse_response;
use mcp_test_support::format_with_current_shell;

// Allow ample time on slower CI or under load to avoid flakes.
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Test that a shell command that is not on the "trusted" list triggers an
/// elicitation request to the MCP and that sending the approval runs the
/// command, as expected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_shell_command_approval_triggers_elicitation() {
    if env::var(CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
        );
        return;
    }

    // Apparently `#[tokio::test]` must return `()`, so we create a helper
    // function that returns `Result` so we can use `?` in favor of `unwrap`.
    if let Err(err) = shell_command_approval_triggers_elicitation().await {
        panic!("failure: {err}");
    }
}

async fn shell_command_approval_triggers_elicitation() -> anyhow::Result<()> {
    // Use a simple, untrusted command that creates a file so we can
    // observe a side-effect.
    let workdir_for_shell_function_call = TempDir::new()?;
    let created_filename = "created_by_shell_tool.txt";
    let created_file = workdir_for_shell_function_call
        .path()
        .join(created_filename);

    let shell_command = if cfg!(windows) {
        vec![
            "New-Item".to_string(),
            "-ItemType".to_string(),
            "File".to_string(),
            "-Path".to_string(),
            created_filename.to_string(),
            "-Force".to_string(),
        ]
    } else {
        vec!["touch".to_string(), created_filename.to_string()]
    };
    let expected_shell_command =
        format_with_current_shell(&shlex::try_join(shell_command.iter().map(String::as_str))?);

    let McpHandle {
        process: mut mcp_process,
        server: _server,
        dir: _dir,
    } = create_mcp_process(vec![
        create_shell_command_sse_response(
            shell_command.clone(),
            Some(workdir_for_shell_function_call.path()),
            Some(5_000),
            "call1234",
        )?,
        create_final_assistant_message_sse_response("File created!")?,
    ])
    .await?;

    // Send a "codex" tool request, which should hit the responses endpoint.
    // In turn, it should reply with a tool call, which the MCP should forward
    // as an elicitation.
    let codex_request_id = mcp_process
        .send_codex_tool_call(CodexToolCallParam {
            prompt: "run `git init`".to_string(),
            ..Default::default()
        })
        .await?;
    let elicitation_request = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_request_message(),
    )
    .await??;

    assert_eq!(elicitation_request.jsonrpc, JsonRpcVersion2_0);
    assert_eq!(elicitation_request.request.method, "elicitation/create");

    let elicitation_request_id = elicitation_request.id.clone();
    let params = serde_json::from_value::<ExecApprovalElicitRequestParams>(
        elicitation_request
            .request
            .params
            .clone()
            .ok_or_else(|| anyhow::anyhow!("elicitation_request.params must be set"))?,
    )?;
    assert_eq!(
        elicitation_request.request.params,
        Some(create_expected_elicitation_request_params(
            expected_shell_command,
            workdir_for_shell_function_call.path(),
            codex_request_id.to_string(),
            params.codex_event_id.clone(),
            params.thread_id,
        )?)
    );

    // Accept the `git init` request by responding to the elicitation.
    mcp_process
        .send_response(
            elicitation_request_id,
            serde_json::to_value(ExecApprovalResponse {
                decision: ReviewDecision::Approved,
            })?,
        )
        .await?;

    // Verify task_complete notification arrives before the tool call completes.
    #[expect(clippy::expect_used)]
    let _task_complete = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_legacy_task_complete_notification(),
    )
    .await
    .expect("task_complete_notification timeout")
    .expect("task_complete_notification resp");

    // Verify the original `codex` tool call completes and that the file was created.
    let codex_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id)),
    )
    .await??;
    assert_eq!(
        JsonRpcResponse {
            jsonrpc: JsonRpcVersion2_0,
            id: RequestId::Number(codex_request_id),
            result: json!({
                "content": [
                    {
                        "text": "File created!",
                        "type": "text"
                    }
                ],
                "structuredContent": {
                    "threadId": params.thread_id,
                    "content": "File created!"
                }
            }),
        },
        codex_response
    );

    assert!(created_file.is_file(), "created file should exist");

    Ok(())
}

fn create_expected_elicitation_request_params(
    command: Vec<String>,
    workdir: &Path,
    codex_mcp_tool_call_id: String,
    codex_event_id: String,
    thread_id: codex_protocol::ThreadId,
) -> anyhow::Result<serde_json::Value> {
    let expected_message = format!(
        "Allow Codex to run `{}` in `{}`?",
        shlex::try_join(command.iter().map(std::convert::AsRef::as_ref))?,
        workdir.to_string_lossy()
    );
    let codex_parsed_cmd = parse_command::parse_command(&command);
    let params_json = serde_json::to_value(ExecApprovalElicitRequestParams {
        message: expected_message,
        requested_schema: json!({"type":"object","properties":{}}),
        thread_id,
        codex_elicitation: "exec-approval".to_string(),
        codex_mcp_tool_call_id,
        codex_event_id,
        codex_command: command,
        codex_cwd: workdir.to_path_buf(),
        codex_call_id: "call1234".to_string(),
        codex_parsed_cmd,
    })?;
    Ok(params_json)
}

/// Test that patch approval triggers an elicitation request to the MCP and that
/// sending the approval applies the patch, as expected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_patch_approval_triggers_elicitation() {
    if env::var(CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
        );
        return;
    }

    if let Err(err) = patch_approval_triggers_elicitation().await {
        panic!("failure: {err}");
    }
}

async fn patch_approval_triggers_elicitation() -> anyhow::Result<()> {
    if cfg!(windows) {
        // powershell apply_patch shell calls are not parsed into apply patch approvals

        return Ok(());
    }

    let cwd = TempDir::new()?;
    let test_file = cwd.path().join("destination_file.txt");
    std::fs::write(&test_file, "original content\n")?;

    let patch_content = format!(
        "*** Begin Patch\n*** Update File: {}\n-original content\n+modified content\n*** End Patch",
        test_file.as_path().to_string_lossy()
    );

    let McpHandle {
        process: mut mcp_process,
        server: _server,
        dir: _dir,
    } = create_mcp_process(vec![
        create_apply_patch_sse_response(&patch_content, "call1234")?,
        create_final_assistant_message_sse_response("Patch has been applied successfully!")?,
    ])
    .await?;

    // Send a "codex" tool request that will trigger the apply_patch command
    let codex_request_id = mcp_process
        .send_codex_tool_call(CodexToolCallParam {
            cwd: Some(cwd.path().to_string_lossy().to_string()),
            prompt: "please modify the test file".to_string(),
            ..Default::default()
        })
        .await?;
    let elicitation_request = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_request_message(),
    )
    .await??;

    assert_eq!(elicitation_request.jsonrpc, JsonRpcVersion2_0);
    assert_eq!(elicitation_request.request.method, "elicitation/create");

    let elicitation_request_id = elicitation_request.id.clone();
    let params = serde_json::from_value::<PatchApprovalElicitRequestParams>(
        elicitation_request
            .request
            .params
            .clone()
            .ok_or_else(|| anyhow::anyhow!("elicitation_request.params must be set"))?,
    )?;

    let mut expected_changes = HashMap::new();
    expected_changes.insert(
        test_file.as_path().to_path_buf(),
        FileChange::Update {
            unified_diff: "@@ -1 +1 @@\n-original content\n+modified content\n".to_string(),
            move_path: None,
        },
    );

    assert_eq!(
        elicitation_request.request.params,
        Some(create_expected_patch_approval_elicitation_request_params(
            expected_changes,
            None, // No grant_root expected
            None, // No reason expected
            codex_request_id.to_string(),
            params.codex_event_id.clone(),
            params.thread_id,
        )?)
    );

    // Accept the patch approval request by responding to the elicitation
    mcp_process
        .send_response(
            elicitation_request_id,
            serde_json::to_value(PatchApprovalResponse {
                decision: ReviewDecision::Approved,
            })?,
        )
        .await?;

    // Verify the original `codex` tool call completes
    let codex_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id)),
    )
    .await??;
    assert_eq!(
        JsonRpcResponse {
            jsonrpc: JsonRpcVersion2_0,
            id: RequestId::Number(codex_request_id),
            result: json!({
                "content": [
                    {
                        "text": "Patch has been applied successfully!",
                        "type": "text"
                    }
                ],
                "structuredContent": {
                    "threadId": params.thread_id,
                    "content": "Patch has been applied successfully!"
                }
            }),
        },
        codex_response
    );

    let file_contents = std::fs::read_to_string(test_file.as_path())?;
    assert_eq!(file_contents, "modified content\n");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_codex_tool_passes_base_instructions() {
    skip_if_no_network!();

    // Apparently `#[tokio::test]` must return `()`, so we create a helper
    // function that returns `Result` so we can use `?` in favor of `unwrap`.
    if let Err(err) = codex_tool_passes_base_instructions().await {
        panic!("failure: {err}");
    }
}

async fn codex_tool_passes_base_instructions() -> anyhow::Result<()> {
    #![expect(clippy::expect_used, clippy::unwrap_used)]

    let server =
        create_mock_responses_server(vec![create_final_assistant_message_sse_response("Enjoy!")?])
            .await;

    // Run `codex mcp` with a specific config.toml.
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;
    let mut mcp_process = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp_process.initialize()).await??;

    // Send a "codex" tool request, which should hit the responses endpoint.
    let codex_request_id = mcp_process
        .send_codex_tool_call(CodexToolCallParam {
            prompt: "How are you?".to_string(),
            base_instructions: Some("You are a helpful assistant.".to_string()),
            developer_instructions: Some("Foreshadow upcoming tool calls.".to_string()),
            ..Default::default()
        })
        .await?;

    let codex_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id)),
    )
    .await??;
    assert_eq!(codex_response.jsonrpc, JsonRpcVersion2_0);
    assert_eq!(codex_response.id, RequestId::Number(codex_request_id));
    assert_eq!(
        codex_response.result,
        json!({
            "content": [
                {
                    "text": "Enjoy!",
                    "type": "text"
                }
            ],
            "structuredContent": {
                "threadId": codex_response
                    .result
                    .get("structuredContent")
                    .and_then(|v| v.get("threadId"))
                    .and_then(serde_json::Value::as_str)
                    .expect("codex tool response should include structuredContent.threadId"),
                "content": "Enjoy!"
            }
        })
    );

    let requests = server.received_requests().await.unwrap();
    let request = requests[0].body_json::<serde_json::Value>()?;
    let instructions = request["instructions"]
        .as_str()
        .expect("responses request should include instructions");
    assert!(instructions.starts_with("You are a helpful assistant."));

    let developer_messages: Vec<&serde_json::Value> = request["input"]
        .as_array()
        .expect("responses request should include input items")
        .iter()
        .filter(|msg| msg.get("role").and_then(|role| role.as_str()) == Some("developer"))
        .collect();
    let developer_contents: Vec<&str> = developer_messages
        .iter()
        .filter_map(|msg| msg.get("content").and_then(serde_json::Value::as_array))
        .flat_map(|content| content.iter())
        .filter(|span| span.get("type").and_then(serde_json::Value::as_str) == Some("input_text"))
        .filter_map(|span| span.get("text").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        developer_contents
            .iter()
            .any(|content| content.contains("`sandbox_mode`")),
        "expected permissions developer message, got {developer_contents:?}"
    );
    assert!(
        developer_contents.contains(&"Foreshadow upcoming tool calls."),
        "expected developer instructions in developer messages, got {developer_contents:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_codex_status_tool_returns_status_and_token_usage() {
    skip_if_no_network!();

    if let Err(err) = codex_status_tool_returns_status_and_token_usage().await {
        panic!("failure: {err}");
    }
}

async fn codex_status_tool_returns_status_and_token_usage() -> anyhow::Result<()> {
    let McpHandle {
        process: mut mcp_process,
        server: _server,
        dir: _dir,
    } = create_mcp_process(vec![
        create_final_assistant_message_sse_response_with_tokens("Status test response", 321)?,
    ])
    .await?;

    let codex_request_id = mcp_process
        .send_codex_tool_call(CodexToolCallParam {
            prompt: "Say hello".to_string(),
            ..Default::default()
        })
        .await?;
    let codex_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id)),
    )
    .await??;
    let thread_id = codex_response
        .result
        .get("structuredContent")
        .and_then(|v| v.get("threadId"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("codex tool response should include threadId"))?
        .to_string();

    let codex_status_request_id = mcp_process
        .send_tool_call("codex-status", Some(json!({ "threadId": thread_id })))
        .await?;
    let status_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_status_request_id)),
    )
    .await??;

    assert_eq!(status_response.jsonrpc, JsonRpcVersion2_0);
    assert_eq!(
        status_response.id,
        RequestId::Number(codex_status_request_id)
    );
    assert_eq!(status_response.result.get("isError"), None);
    let structured_content = status_response
        .result
        .get("structuredContent")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("codex-status should include structuredContent"))?;
    assert_eq!(structured_content.get("threadId"), Some(&json!(thread_id)));
    assert_eq!(structured_content.get("status"), Some(&json!("completed")));
    assert_eq!(
        structured_content.get("tokenUsage"),
        Some(&json!({
            "inputTokens": 321,
            "cachedInputTokens": 0,
            "outputTokens": 0,
            "reasoningOutputTokens": 0,
            "totalTokens": 321,
        }))
    );
    let context_window = structured_content
        .get("contextWindow")
        .ok_or_else(|| anyhow::anyhow!("codex-status should include contextWindow"))?;
    if let Some(context_window) = context_window.as_object() {
        let max_tokens = context_window
            .get("maxTokens")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("contextWindow.maxTokens should be an integer"))?;
        assert_eq!(context_window.get("usedTokens"), Some(&json!(321)));
        assert_eq!(
            context_window.get("remainingTokens"),
            Some(&json!(max_tokens.saturating_sub(321)))
        );
    } else {
        assert_eq!(context_window, &json!(null));
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_codex_status_tool_context_window_uses_last_usage() {
    skip_if_no_network!();

    if let Err(err) = codex_status_tool_context_window_uses_last_usage().await {
        panic!("failure: {err}");
    }
}

async fn codex_status_tool_context_window_uses_last_usage() -> anyhow::Result<()> {
    let McpHandle {
        process: mut mcp_process,
        server: _server,
        dir: _dir,
    } = create_mcp_process(vec![
        create_final_assistant_message_sse_response_with_tokens("First response", 321)?,
        create_final_assistant_message_sse_response_with_tokens("Second response", 87)?,
    ])
    .await?;

    let mut config = HashMap::new();
    config.insert("model_context_window".to_string(), json!(1_000));
    let codex_request_id = mcp_process
        .send_codex_tool_call(CodexToolCallParam {
            prompt: "Say hello".to_string(),
            config: Some(config),
            ..Default::default()
        })
        .await?;
    let codex_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id)),
    )
    .await??;
    let thread_id = codex_response
        .result
        .get("structuredContent")
        .and_then(|v| v.get("threadId"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("codex tool response should include threadId"))?
        .to_string();

    let codex_reply_request_id = mcp_process
        .send_tool_call(
            "codex-reply",
            Some(json!({
                "threadId": thread_id,
                "prompt": "Continue",
            })),
        )
        .await?;
    let _codex_reply_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_reply_request_id)),
    )
    .await??;

    let codex_status_request_id = mcp_process
        .send_tool_call("codex-status", Some(json!({ "threadId": thread_id })))
        .await?;
    let status_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_status_request_id)),
    )
    .await??;

    let structured_content = status_response
        .result
        .get("structuredContent")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("codex-status should include structuredContent"))?;
    assert_eq!(
        structured_content.get("tokenUsage"),
        Some(&json!({
            "inputTokens": 408,
            "cachedInputTokens": 0,
            "outputTokens": 0,
            "reasoningOutputTokens": 0,
            "totalTokens": 408,
        }))
    );
    let context_window = structured_content
        .get("contextWindow")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("codex-status should include contextWindow object"))?;
    let max_tokens = context_window
        .get("maxTokens")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("contextWindow.maxTokens should be an integer"))?;
    assert_eq!(context_window.get("usedTokens"), Some(&json!(87)));
    assert_eq!(
        context_window.get("remainingTokens"),
        Some(&json!(max_tokens.saturating_sub(87)))
    );
    let remaining_percent = context_window
        .get("remainingPercent")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("contextWindow.remainingPercent should be a float"))?;
    let expected_remaining_percent =
        (max_tokens.saturating_sub(87) as f64 / max_tokens as f64) * 100.0;
    assert!(
        (remaining_percent - expected_remaining_percent).abs() < 1e-9,
        "remainingPercent should match remaining/max math, got: {remaining_percent}"
    );

    let text_summary = status_response
        .result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .and_then(|content| content.first())
        .and_then(|entry| entry.get("text"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("codex-status should include summary text"))?;
    assert!(
        text_summary.contains(&format!(
            "context max={max_tokens} used=87 remaining={}",
            max_tokens.saturating_sub(87)
        )),
        "summary should use last turn usage for context window, got: {text_summary}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_codex_status_tool_unknown_thread_returns_error() {
    skip_if_no_network!();

    if let Err(err) = codex_status_tool_unknown_thread_returns_error().await {
        panic!("failure: {err}");
    }
}

async fn codex_status_tool_unknown_thread_returns_error() -> anyhow::Result<()> {
    let McpHandle {
        process: mut mcp_process,
        server: _server,
        dir: _dir,
    } = create_mcp_process(vec![]).await?;

    let unknown_thread_id = "019bbed6-1e9e-7f31-984c-a05b65045719";
    let request_id = mcp_process
        .send_tool_call(
            "codex-status",
            Some(json!({
                "threadId": unknown_thread_id,
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(request_id)),
    )
    .await??;

    assert_eq!(response.result.get("isError"), Some(&json!(true)));
    assert_eq!(
        response
            .result
            .get("structuredContent")
            .and_then(|content| content.get("threadId"))
            .and_then(serde_json::Value::as_str),
        Some(unknown_thread_id)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tools_list_includes_codex_status() {
    skip_if_no_network!();

    if let Err(err) = tools_list_includes_codex_status().await {
        panic!("failure: {err}");
    }
}

async fn tools_list_includes_codex_status() -> anyhow::Result<()> {
    let McpHandle {
        process: mut mcp_process,
        server: _server,
        dir: _dir,
    } = create_mcp_process(vec![]).await?;

    let request_id = mcp_process.send_list_tools_request().await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(request_id)),
    )
    .await??;
    let tool_names: Vec<String> = response
        .result
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("tools/list should return tools"))?
        .iter()
        .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect();

    assert_eq!(
        tool_names,
        vec![
            "codex".to_string(),
            "codex-reply".to_string(),
            "codex-status".to_string(),
        ]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_server_serves_health_endpoints_and_lists_tools() {
    if env::var(CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
        );
        return;
    }

    if let Err(err) = http_server_serves_health_endpoints_and_lists_tools_impl().await {
        panic!("failure: {err}");
    }
}

async fn http_server_serves_health_endpoints_and_lists_tools_impl() -> anyhow::Result<()> {
    let HttpMcpHandle {
        process,
        server: _server,
        dir: _dir,
    } = create_http_mcp_process(vec![]).await?;

    let client = reqwest::Client::new();
    assert_eq!(
        client.get(process.healthz_url()).send().await?.status(),
        StatusCode::OK
    );
    assert_eq!(
        client.get(process.readyz_url()).send().await?.status(),
        StatusCode::OK
    );

    let session = process.initialize_session().await?;
    assert!(!session.session_id().is_empty());
    assert_eq!(
        tool_names(&session.list_tools().await?)?,
        vec![
            "codex".to_string(),
            "codex-reply".to_string(),
            "codex-status".to_string(),
        ]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_server_supports_concurrent_sessions() {
    if env::var(CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
        );
        return;
    }

    if let Err(err) = http_server_supports_concurrent_sessions_impl().await {
        panic!("failure: {err}");
    }
}

async fn http_server_supports_concurrent_sessions_impl() -> anyhow::Result<()> {
    let HttpMcpHandle {
        process,
        server: _server,
        dir: _dir,
    } = create_http_mcp_process(vec![]).await?;

    let (session_a, session_b) =
        tokio::join!(process.initialize_session(), process.initialize_session());
    let session_a = session_a?;
    let session_b = session_b?;

    assert_ne!(session_a.session_id(), session_b.session_id());

    let (tools_a, tools_b) = tokio::join!(session_a.list_tools(), session_b.list_tools());
    assert_eq!(
        tool_names(&tools_a?)?,
        vec![
            "codex".to_string(),
            "codex-reply".to_string(),
            "codex-status".to_string(),
        ]
    );
    assert_eq!(
        tool_names(&tools_b?)?,
        vec![
            "codex".to_string(),
            "codex-reply".to_string(),
            "codex-status".to_string(),
        ]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_server_codex_tool_call_happy_path() {
    if env::var(CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
        );
        return;
    }

    if let Err(err) = http_server_codex_tool_call_happy_path_impl().await {
        panic!("failure: {err}");
    }
}

async fn http_server_codex_tool_call_happy_path_impl() -> anyhow::Result<()> {
    let HttpMcpHandle {
        process,
        server: _server,
        dir: _dir,
    } = create_http_mcp_process(vec![create_final_assistant_message_sse_response("Enjoy!")?])
        .await?;

    let session = process.initialize_session().await?;
    let response = session
        .call_tool(
            "codex",
            serde_json::to_value(CodexToolCallParam {
                prompt: "How are you?".to_string(),
                base_instructions: Some("You are a helpful assistant.".to_string()),
                developer_instructions: Some("Foreshadow upcoming tool calls.".to_string()),
                ..Default::default()
            })?,
        )
        .await?;

    let thread_id = response
        .get("structuredContent")
        .and_then(|content| content.get("threadId"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("threadId missing from structuredContent"))?;
    assert_eq!(
        response,
        json!({
            "content": [
                {
                    "text": "Enjoy!",
                    "type": "text"
                }
            ],
            "structuredContent": {
                "threadId": thread_id,
                "content": "Enjoy!"
            }
        })
    );

    Ok(())
}

fn create_expected_patch_approval_elicitation_request_params(
    changes: HashMap<PathBuf, FileChange>,
    grant_root: Option<PathBuf>,
    reason: Option<String>,
    codex_mcp_tool_call_id: String,
    codex_event_id: String,
    thread_id: codex_protocol::ThreadId,
) -> anyhow::Result<serde_json::Value> {
    let mut message_lines = Vec::new();
    if let Some(r) = &reason {
        message_lines.push(r.clone());
    }
    message_lines.push("Allow Codex to apply proposed code changes?".to_string());
    let params_json = serde_json::to_value(PatchApprovalElicitRequestParams {
        message: message_lines.join("\n"),
        requested_schema: json!({"type":"object","properties":{}}),
        thread_id,
        codex_elicitation: "patch-approval".to_string(),
        codex_mcp_tool_call_id,
        codex_event_id,
        codex_reason: reason,
        codex_grant_root: grant_root,
        codex_changes: changes,
        codex_call_id: "call1234".to_string(),
    })?;

    Ok(params_json)
}

/// This handle is used to ensure that the MockServer and TempDir are not dropped while
/// the McpProcess is still running.
pub struct McpHandle {
    pub process: McpProcess,
    /// Retain the server for the lifetime of the McpProcess.
    #[allow(dead_code)]
    server: MockServer,
    /// Retain the temporary directory for the lifetime of the McpProcess.
    #[allow(dead_code)]
    dir: TempDir,
}

/// This handle keeps the HTTP server and its backing temp dir alive for the duration of the test.
pub struct HttpMcpHandle {
    pub process: HttpMcpProcess,
    #[allow(dead_code)]
    server: MockServer,
    #[allow(dead_code)]
    dir: TempDir,
}

async fn create_mcp_process(responses: Vec<String>) -> anyhow::Result<McpHandle> {
    let server = create_mock_responses_server(responses).await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;
    let mut mcp_process = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp_process.initialize()).await??;
    Ok(McpHandle {
        process: mcp_process,
        server,
        dir: codex_home,
    })
}

async fn create_http_mcp_process(responses: Vec<String>) -> anyhow::Result<HttpMcpHandle> {
    let server = create_mock_responses_server(responses).await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;
    let process = HttpMcpProcess::new(codex_home.path()).await?;
    Ok(HttpMcpHandle {
        process,
        server,
        dir: codex_home,
    })
}

fn tool_names(response: &serde_json::Value) -> anyhow::Result<Vec<String>> {
    let tools = response
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("tools/list should return tools"))?;

    Ok(tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect())
}

/// Create a Codex config that uses the mock server as the model provider.
/// It also uses `approval_policy = "untrusted"` so that we exercise the
/// elicitation code path for shell commands.
fn create_config_toml(codex_home: &Path, server_uri: &str) -> std::io::Result<()> {
    let config_toml = codex_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "untrusted"
sandbox_policy = "workspace-write"

model_provider = "mock_provider"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[features]
"#
        ),
    )
}
