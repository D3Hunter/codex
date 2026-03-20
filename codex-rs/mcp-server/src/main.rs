use clap::Parser;
use codex_arg0::Arg0DispatchPaths;
use codex_arg0::arg0_dispatch_or_else;
use codex_mcp_server::McpListenTransport;
use codex_mcp_server::run_main_with_transport;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, Parser)]
struct McpServerArgs {
    /// Transport endpoint URL. Supported values: `stdio://` (default),
    /// `http://IP:PORT[/PATH]`.
    #[arg(
        long = "listen",
        value_name = "URL",
        default_value = McpListenTransport::DEFAULT_LISTEN_URL
    )]
    listen: McpListenTransport,

    #[clap(flatten)]
    config_overrides: CliConfigOverrides,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|arg0_paths: Arg0DispatchPaths| async move {
        let args = McpServerArgs::parse();
        run_main_with_transport(arg0_paths, args.config_overrides, args.listen).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn mcp_server_args_from(args: &[&str]) -> McpServerArgs {
        McpServerArgs::try_parse_from(args).expect("parse")
    }

    #[test]
    fn main_args_default_listen_transport_is_stdio() {
        let args = mcp_server_args_from(["codex-mcp-server"].as_ref());

        assert_eq!(args.listen, McpListenTransport::Stdio);
    }

    #[test]
    fn main_args_parse_explicit_listen_transport() {
        let args = mcp_server_args_from(
            [
                "codex-mcp-server",
                "--listen",
                "http://127.0.0.1:8080/custom",
            ]
            .as_ref(),
        );

        assert_eq!(
            args.listen,
            McpListenTransport::Http {
                bind_address: "127.0.0.1:8080".parse().expect("socket address"),
                path: "/custom".to_string(),
            }
        );
    }

    #[test]
    fn main_args_parse_config_overrides() {
        let args = mcp_server_args_from(["codex-mcp-server", "-c", "model=\"gpt-5\""].as_ref());

        assert_eq!(
            args.config_overrides.raw_overrides,
            vec!["model=\"gpt-5\"".to_string()]
        );
    }
}
