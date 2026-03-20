use std::io::Result as IoResult;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
#[cfg(test)]
use codex_core::config::ConfigBuilder;
use futures::Stream;
use rmcp::model::ClientJsonRpcMessage;
use rmcp::model::ClientNotification;
use rmcp::model::Extensions;
use rmcp::model::InitializedNotification;
use rmcp::model::ServerJsonRpcMessage;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::ServerSseMessage;
use rmcp::transport::streamable_http_server::session::SessionId;
use rmcp::transport::streamable_http_server::session::SessionManager;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManagerError;
use tokio::net::TcpListener;
use tracing::info;

use crate::session_runtime::SessionRuntime;

pub(crate) async fn run(
    arg0_paths: Arg0DispatchPaths,
    config: Arc<Config>,
    bind_address: SocketAddr,
    path: String,
) -> IoResult<()> {
    let listener = TcpListener::bind(bind_address).await?;
    let local_addr = listener.local_addr()?;
    info!("mcp-server http listening on http://{local_addr}{path}");

    axum::serve(listener, app(&path, arg0_paths, config)).await
}

fn app(mcp_path: &str, arg0_paths: Arg0DispatchPaths, config: Arc<Config>) -> Router {
    let session_manager = Arc::new(CompatibilitySessionManager::default());
    let mcp_http_config = StreamableHttpServerConfig {
        // Some client implementations decode only the first SSE `data:` event for
        // initialize and fail on empty priming events (`EOF` on JSON decode).
        // We keep session stateful mode, but disable retry priming to make the
        // first event always the JSON-RPC initialize response.
        sse_retry: None,
        ..StreamableHttpServerConfig::default()
    };
    let mcp_service = StreamableHttpService::new(
        move || Ok(SessionRuntime::new(arg0_paths.clone(), config.clone())),
        session_manager,
        mcp_http_config,
    );

    Router::new()
        .route("/healthz", get(readiness_handler))
        .route("/readyz", get(readiness_handler))
        .nest_service(mcp_path, mcp_service)
}

async fn readiness_handler() {}

#[derive(Debug, Default)]
struct CompatibilitySessionManager {
    inner: LocalSessionManager,
}

impl SessionManager for CompatibilitySessionManager {
    type Error = LocalSessionManagerError;
    type Transport = <LocalSessionManager as SessionManager>::Transport;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        self.inner.create_session().await
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        let response = self.inner.initialize_session(id, message).await?;

        // MCP lifecycle requires the client to send `notifications/initialized`
        // after `initialize` and before normal requests. Some clients in the
        // wild skip that step, and `rmcp` then tears down the session during
        // handshake (`connection closed: initialize notification`).
        // Injecting it here keeps those clients interoperable while preserving
        // behavior for spec-compliant clients.
        self.inner
            .accept_message(
                id,
                ClientJsonRpcMessage::notification(ClientNotification::InitializedNotification(
                    InitializedNotification {
                        method: Default::default(),
                        extensions: Extensions::default(),
                    },
                )),
            )
            .await?;

        Ok(response)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        self.inner.has_session(id).await
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        self.inner.close_session(id).await
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner.create_stream(id, message).await
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        self.inner.accept_message(id, message).await
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner.create_standalone_stream(id).await
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner.resume(id, last_event_id).await
    }
}

#[cfg(test)]
async fn serve(listener: TcpListener, mcp_path: &str) -> IoResult<()> {
    let _temp_dir = tempfile::TempDir::new().map_err(std::io::Error::other)?;
    let config = Arc::new(
        ConfigBuilder::default()
            .codex_home(_temp_dir.path().to_path_buf())
            .build()
            .await
            .map_err(std::io::Error::other)?,
    );

    axum::serve(
        listener,
        app(mcp_path, Arg0DispatchPaths::default(), config),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::serve;
    use pretty_assertions::assert_eq;
    use reqwest::StatusCode;
    use rmcp::model::RequestId;
    use serde_json::json;
    use std::time::Duration;

    #[tokio::test]
    async fn http_runtime_serves_health_and_readiness_checks() -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let bind_address = listener.local_addr()?;
        let server = tokio::spawn(serve(listener, "/mcp"));

        let client = reqwest::Client::builder().build()?;
        let healthz_status =
            wait_for_get_status(&client, format!("http://{bind_address}/healthz")).await?;
        let readyz_status =
            wait_for_get_status(&client, format!("http://{bind_address}/readyz")).await?;

        assert_eq!(healthz_status, StatusCode::OK);
        assert_eq!(readyz_status, StatusCode::OK);

        server.abort();
        let _ = server.await;

        Ok(())
    }

    #[tokio::test]
    async fn http_runtime_accepts_initialize_handshake_at_default_mcp_path() -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let bind_address = listener.local_addr()?;
        let server = tokio::spawn(serve(listener, "/mcp"));

        let client = reqwest::Client::builder().build()?;
        let response =
            wait_for_initialize_response(&client, format!("http://{bind_address}/mcp")).await?;
        let status = response.status();
        let session_id = response.headers().get("mcp-session-id").cloned();
        let body = response.text().await?;

        assert_eq!(status, StatusCode::OK);
        assert!(session_id.is_some(), "expected mcp-session-id header");
        assert!(
            body.contains(r#""jsonrpc":"2.0""#),
            "expected JSON-RPC payload in response body, got: {body}"
        );
        assert!(
            body.contains(r#""id":1"#),
            "expected initialize response ID in response body, got: {body}"
        );
        assert!(
            body.contains("serverInfo"),
            "expected initialize result in response body, got: {body}"
        );

        server.abort();
        let _ = server.await;

        Ok(())
    }

    #[tokio::test]
    async fn http_runtime_initialize_does_not_prefix_empty_sse_event() -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let bind_address = listener.local_addr()?;
        let server = tokio::spawn(serve(listener, "/mcp"));

        let client = reqwest::Client::builder().build()?;
        let response =
            wait_for_initialize_response(&client, format!("http://{bind_address}/mcp")).await?;
        let body = response.text().await?;

        let first_data_line = body
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .ok_or_else(|| anyhow::anyhow!("expected at least one SSE data line, got: {body}"))?;

        assert!(
            !first_data_line.trim().is_empty(),
            "expected first SSE data line to contain JSON-RPC payload, got empty data event: {body}"
        );

        server.abort();
        let _ = server.await;

        Ok(())
    }

    #[tokio::test]
    async fn http_runtime_mounts_initialize_handshake_at_configured_mcp_path() -> anyhow::Result<()>
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let bind_address = listener.local_addr()?;
        let server = tokio::spawn(serve(listener, "/custom-mcp"));

        let client = reqwest::Client::builder().build()?;
        let missing_path_status =
            wait_for_initialize_status(&client, format!("http://{bind_address}/mcp")).await?;
        let mounted_response =
            wait_for_initialize_response(&client, format!("http://{bind_address}/custom-mcp"))
                .await?;
        let mounted_status = mounted_response.status();

        assert_eq!(missing_path_status, StatusCode::NOT_FOUND);
        assert_eq!(mounted_status, StatusCode::OK);

        server.abort();
        let _ = server.await;

        Ok(())
    }

    #[tokio::test]
    async fn http_runtime_accepts_tools_list_without_initialized_notification() -> anyhow::Result<()>
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let bind_address = listener.local_addr()?;
        let server = tokio::spawn(serve(listener, "/mcp"));

        let client = reqwest::Client::builder().build()?;
        let initialize_response =
            wait_for_initialize_response(&client, format!("http://{bind_address}/mcp")).await?;
        let session_id = initialize_response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string)
            .ok_or_else(|| anyhow::anyhow!("initialize response should include mcp-session-id"))?;

        let tools_list_response = send_tools_list_request(
            &client,
            format!("http://{bind_address}/mcp"),
            session_id,
            RequestId::Number(2),
        )
        .await?;
        let status = tools_list_response.status();
        let body = tools_list_response.text().await?;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(r#""id":2"#),
            "expected tools/list response ID in response body, got: {body}"
        );
        assert!(
            body.contains(r#""tools""#),
            "expected tools/list result in response body, got: {body}"
        );

        server.abort();
        let _ = server.await;

        Ok(())
    }

    async fn wait_for_get_status(
        client: &reqwest::Client,
        url: String,
    ) -> anyhow::Result<StatusCode> {
        for _ in 0..20 {
            if let Ok(response) = client.get(&url).send().await {
                return Ok(response.status());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        anyhow::bail!("timed out waiting for {url}");
    }

    async fn wait_for_initialize_status(
        client: &reqwest::Client,
        url: String,
    ) -> anyhow::Result<StatusCode> {
        for _ in 0..20 {
            if let Ok(response) = send_initialize_request(client, url.clone()).await {
                return Ok(response.status());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        anyhow::bail!("timed out waiting for initialize status from {url}");
    }

    async fn wait_for_initialize_response(
        client: &reqwest::Client,
        url: String,
    ) -> anyhow::Result<reqwest::Response> {
        for _ in 0..20 {
            if let Ok(response) = send_initialize_request(client, url.clone()).await {
                return Ok(response);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        anyhow::bail!("timed out waiting for initialize response from {url}");
    }

    async fn send_initialize_request(
        client: &reqwest::Client,
        url: String,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {
                    "name": "http-runtime-test",
                    "version": "0.0.0"
                }
            }
        });

        client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&body)
            .send()
            .await
    }

    async fn send_tools_list_request(
        client: &reqwest::Client,
        url: String,
        session_id: String,
        request_id: RequestId,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/list",
            "params": {}
        });

        client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Mcp-Session-Id", session_id)
            .json(&body)
            .send()
            .await
    }
}
