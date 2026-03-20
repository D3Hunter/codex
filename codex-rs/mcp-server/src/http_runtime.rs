use std::io::Result as IoResult;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
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

fn app(_mcp_path: &str) -> Router {
    // Keep the router path-aware so ST-7 can mount the MCP handler here
    // without reshaping the runtime entrypoint or test harness.
    Router::new()
        .route("/healthz", get(readiness_handler))
        .route("/readyz", get(readiness_handler))
}

async fn readiness_handler() {}

#[cfg(test)]
async fn serve(listener: TcpListener, mcp_path: &str) -> IoResult<()> {
    axum::serve(listener, app(mcp_path)).await
}

#[cfg(test)]
mod tests {
    use super::serve;
    use pretty_assertions::assert_eq;
    use reqwest::StatusCode;
    use std::time::Duration;

    #[tokio::test]
    async fn http_runtime_serves_health_and_readiness_checks() -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let bind_address = listener.local_addr()?;
        let server = tokio::spawn(serve(listener, "/mcp"));

        let client = reqwest::Client::builder().build()?;
        let healthz_status =
            wait_for_status(&client, format!("http://{bind_address}/healthz")).await?;
        let readyz_status =
            wait_for_status(&client, format!("http://{bind_address}/readyz")).await?;

        assert_eq!(healthz_status, StatusCode::OK);
        assert_eq!(readyz_status, StatusCode::OK);

        server.abort();
        let _ = server.await;

        Ok(())
    }

    async fn wait_for_status(client: &reqwest::Client, url: String) -> anyhow::Result<StatusCode> {
        for _ in 0..20 {
            if let Ok(response) = client.get(&url).send().await {
                return Ok(response.status());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        anyhow::bail!("timed out waiting for {url}");
    }
}
