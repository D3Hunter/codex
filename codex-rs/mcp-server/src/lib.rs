//! Prototype MCP server.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::sync::Arc;

use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use codex_utils_cli::CliConfigOverrides;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod codex_tool_config;
mod codex_tool_runner;
mod exec_approval;
pub(crate) mod message_processor;
mod outgoing_message;
mod patch_approval;
mod stdio_runtime;
mod transport;

pub use crate::codex_tool_config::CodexToolCallParam;
pub use crate::codex_tool_config::CodexToolCallReplyParam;
pub use crate::codex_tool_config::CodexToolStatusParam;
pub use crate::exec_approval::ExecApprovalElicitRequestParams;
pub use crate::exec_approval::ExecApprovalResponse;
pub use crate::patch_approval::PatchApprovalElicitRequestParams;
pub use crate::patch_approval::PatchApprovalResponse;
pub use crate::transport::McpListenTransport;
pub use crate::transport::McpListenTransportParseError;

const DEFAULT_ANALYTICS_ENABLED: bool = true;
const OTEL_SERVICE_NAME: &str = "codex_mcp_server";

pub async fn run_main(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
) -> IoResult<()> {
    run_main_with_transport(arg0_paths, cli_config_overrides, McpListenTransport::Stdio).await
}

pub async fn run_main_with_transport(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
    _listen_transport: McpListenTransport,
) -> IoResult<()> {
    // Parse CLI overrides once and derive the base Config eagerly so later
    // components do not need to work with raw TOML values.
    let cli_kv_overrides = cli_config_overrides.parse_overrides().map_err(|e| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("error parsing -c overrides: {e}"),
        )
    })?;
    let config = Config::load_with_cli_overrides(cli_kv_overrides)
        .await
        .map_err(|e| {
            std::io::Error::new(ErrorKind::InvalidData, format!("error loading config: {e}"))
        })?;

    let otel = codex_core::otel_init::build_provider(
        &config,
        env!("CARGO_PKG_VERSION"),
        Some(OTEL_SERVICE_NAME),
        DEFAULT_ANALYTICS_ENABLED,
    )
    .map_err(|e| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!("error loading otel config: {e}"),
        )
    })?;

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::from_default_env());
    let otel_logger_layer = otel.as_ref().and_then(|provider| provider.logger_layer());
    let otel_tracing_layer = otel.as_ref().and_then(|provider| provider.tracing_layer());

    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(otel_logger_layer)
        .with(otel_tracing_layer)
        .try_init();

    stdio_runtime::run(arg0_paths, Arc::new(config)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_core::config::ConfigBuilder;
    use codex_core::config::types::OtelExporterKind;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn mcp_server_defaults_analytics_to_enabled() {
        assert_eq!(DEFAULT_ANALYTICS_ENABLED, true);
    }

    #[tokio::test]
    async fn mcp_server_builds_otel_provider_with_logs_traces_and_metrics() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let mut config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await?;
        let exporter = OtelExporterKind::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            headers: HashMap::new(),
            tls: None,
        };
        config.otel.exporter = exporter.clone();
        config.otel.trace_exporter = exporter.clone();
        config.otel.metrics_exporter = exporter;
        config.analytics_enabled = None;

        let provider = codex_core::otel_init::build_provider(
            &config,
            "0.0.0-test",
            Some(OTEL_SERVICE_NAME),
            DEFAULT_ANALYTICS_ENABLED,
        )
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .expect("otel provider");

        assert!(provider.logger.is_some(), "expected log exporter");
        assert!(
            provider.tracer_provider.is_some(),
            "expected trace exporter"
        );
        assert!(provider.metrics().is_some(), "expected metrics exporter");
        provider.shutdown();

        Ok(())
    }
}
