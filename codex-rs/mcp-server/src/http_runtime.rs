use std::io::Error;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::net::SocketAddr;
use std::sync::Arc;

use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;

pub(crate) async fn run(
    _arg0_paths: Arg0DispatchPaths,
    _config: Arc<Config>,
    bind_address: SocketAddr,
    path: String,
) -> IoResult<()> {
    Err(Error::new(
        ErrorKind::Unsupported,
        format!("HTTP MCP runtime is not implemented yet for http://{bind_address}{path}"),
    ))
}
