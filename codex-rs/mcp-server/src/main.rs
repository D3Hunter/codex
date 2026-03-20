use std::ffi::OsString;

use anyhow::Context;
use codex_arg0::Arg0DispatchPaths;
use codex_arg0::arg0_dispatch_or_else;
use codex_mcp_server::McpListenTransport;
use codex_mcp_server::run_main_with_transport;
use codex_utils_cli::CliConfigOverrides;

#[derive(Clone, Debug)]
struct McpServerArgs {
    listen: McpListenTransport,
    config_overrides: CliConfigOverrides,
}

impl McpServerArgs {
    fn parse() -> anyhow::Result<Self> {
        Self::parse_from(std::env::args_os())
    }

    fn parse_from<I, T>(args: I) -> anyhow::Result<Self>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        let mut args = args.into_iter().map(Into::into);
        let _program_name = args.next();
        let mut listen_url = McpListenTransport::DEFAULT_LISTEN_URL.to_string();
        let mut config_overrides = CliConfigOverrides::default();

        while let Some(arg) = args.next() {
            let arg = os_string_to_string(arg)?;
            if let Some(value) = arg.strip_prefix("--listen=") {
                listen_url = value.to_string();
                continue;
            }

            if arg == "--listen" {
                let value = args.next().context("missing value for `--listen`")?;
                listen_url = os_string_to_string(value)?;
                continue;
            }

            if arg == "-c" || arg == "--config" {
                let value = args.next().context("missing value for `-c` / `--config`")?;
                config_overrides
                    .raw_overrides
                    .push(os_string_to_string(value)?);
                continue;
            }

            anyhow::bail!("unexpected argument `{arg}`");
        }

        let listen = McpListenTransport::from_listen_url(&listen_url)?;
        Ok(Self {
            listen,
            config_overrides,
        })
    }
}

fn os_string_to_string(value: OsString) -> anyhow::Result<String> {
    value.into_string().map_err(|value| {
        anyhow::anyhow!("argument `{}` is not valid UTF-8", value.to_string_lossy())
    })
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|arg0_paths: Arg0DispatchPaths| async move {
        let args = McpServerArgs::parse()?;
        run_main_with_transport(arg0_paths, args.config_overrides, args.listen).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn mcp_server_args_from(args: &[&str]) -> McpServerArgs {
        McpServerArgs::parse_from(args).expect("parse")
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
    fn main_args_parse_explicit_listen_with_equals() {
        let args = mcp_server_args_from(
            ["codex-mcp-server", "--listen=http://127.0.0.1:8080/custom"].as_ref(),
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
