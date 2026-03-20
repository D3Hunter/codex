use std::io::Result as IoResult;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use rmcp::ServerHandler;
use rmcp::model::Implementation;
use rmcp::model::ProtocolVersion;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use tokio::net::TcpListener;
use tracing::info;

pub(crate) async fn run(
    _arg0_paths: Arg0DispatchPaths,
    _config: Arc<Config>,
    bind_address: SocketAddr,
    path: String,
) -> IoResult<()> {
    let listener = TcpListener::bind(bind_address).await?;
    let local_addr = listener.local_addr()?;
    info!("mcp-server http listening on http://{local_addr}{path}");

    axum::serve(listener, app(&path)).await
}

fn app(mcp_path: &str) -> Router {
    let mcp_service = StreamableHttpService::new(
        || Ok(HttpInitializeService),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    Router::new()
        .route("/healthz", get(readiness_handler))
        .route("/readyz", get(readiness_handler))
        .nest_service(mcp_path, mcp_service)
}

async fn readiness_handler() {}

#[derive(Clone, Copy, Debug, Default)]
struct HttpInitializeService;

impl ServerHandler for HttpInitializeService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_03_26,
            capabilities: ServerCapabilities::default(),
            server_info: Implementation {
                name: "codex-mcp-server".to_string(),
                title: Some("Codex".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: None,
                icons: None,
                website_url: None,
            },
            instructions: None,
        }
    }
}

#[cfg(test)]
async fn serve(listener: TcpListener, mcp_path: &str) -> IoResult<()> {
    axum::serve(listener, app(mcp_path)).await
}

#[cfg(test)]
mod tests {
    use super::serve;
    use pretty_assertions::assert_eq;
    use reqwest::StatusCode;
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
}
