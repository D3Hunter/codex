use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;

use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use rmcp::ServerHandler;
use rmcp::model::ErrorData;
use rmcp::model::Implementation;
use rmcp::model::InitializeRequestParams;
use rmcp::model::InitializeResult;
use rmcp::model::JsonRpcRequest;
use rmcp::model::ProtocolVersion;
use rmcp::model::RequestId;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tracing::debug;
use tracing::warn;

use crate::message_processor::MessageProcessor;
use crate::outgoing_message::OutgoingError;
use crate::outgoing_message::OutgoingMessage;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingResponse;

type PendingResponse = Result<Value, ErrorData>;
type PendingResponseSender = oneshot::Sender<PendingResponse>;

pub(crate) struct SessionRuntime {
    processor: Arc<Mutex<MessageProcessor>>,
    #[allow(dead_code)]
    next_synthetic_request_id: AtomicI64,
    #[allow(dead_code)]
    pending_responses: Arc<Mutex<HashMap<RequestId, PendingResponseSender>>>,
}

#[allow(dead_code)]
pub(crate) struct ReservedPendingResponse {
    pub(crate) id: RequestId,
    pub(crate) receiver: oneshot::Receiver<PendingResponse>,
}

impl SessionRuntime {
    pub(crate) fn new(arg0_paths: Arg0DispatchPaths, config: Arc<Config>) -> Self {
        let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let pending_responses = Arc::new(Mutex::new(HashMap::new()));
        let processor = Arc::new(Mutex::new(MessageProcessor::new(
            OutgoingMessageSender::new(outgoing_tx),
            arg0_paths,
            config,
        )));

        tokio::spawn(run_outgoing_bridge(outgoing_rx, pending_responses.clone()));

        Self {
            processor,
            next_synthetic_request_id: AtomicI64::new(0),
            pending_responses,
        }
    }

    #[allow(dead_code)]
    pub(crate) async fn reserve_pending_response(&self) -> ReservedPendingResponse {
        let id = RequestId::Number(
            self.next_synthetic_request_id
                .fetch_add(1, Ordering::Relaxed),
        );
        let (sender, receiver) = oneshot::channel();
        self.pending_responses
            .lock()
            .await
            .insert(id.clone(), sender);

        ReservedPendingResponse { id, receiver }
    }

    #[allow(dead_code)]
    pub(crate) async fn process_request(
        &self,
        request: JsonRpcRequest<rmcp::model::ClientRequest>,
    ) {
        self.processor.lock().await.process_request(request).await;
    }

    #[cfg(test)]
    async fn pending_request_ids(&self) -> Vec<RequestId> {
        let mut ids = self
            .pending_responses
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        ids.sort_by_key(ToString::to_string);
        ids
    }

    #[cfg(test)]
    async fn pending_request_count(&self) -> usize {
        self.pending_responses.lock().await.len()
    }
}

impl ServerHandler for SessionRuntime {
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, rmcp::ErrorData> {
        let _processor = self.processor.clone();
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }

        Ok(self.get_info())
    }

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

async fn run_outgoing_bridge(
    mut outgoing_rx: mpsc::UnboundedReceiver<OutgoingMessage>,
    pending_responses: Arc<Mutex<HashMap<RequestId, PendingResponseSender>>>,
) {
    while let Some(outgoing_message) = outgoing_rx.recv().await {
        match outgoing_message {
            OutgoingMessage::Response(OutgoingResponse { id, result }) => {
                resolve_pending_response(&pending_responses, id, Ok(result)).await;
            }
            OutgoingMessage::Error(OutgoingError { id, error }) => {
                resolve_pending_response(&pending_responses, id, Err(error)).await;
            }
            OutgoingMessage::Notification(notification) => {
                debug!(
                    method = notification.method,
                    "dropping session notification bridge output"
                );
            }
            OutgoingMessage::Request(request) => {
                debug!(
                    method = request.method,
                    "dropping session request bridge output"
                );
            }
        }
    }
}

async fn resolve_pending_response(
    pending_responses: &Arc<Mutex<HashMap<RequestId, PendingResponseSender>>>,
    id: RequestId,
    response: PendingResponse,
) {
    let pending_response = pending_responses.lock().await.remove(&id);
    match pending_response {
        Some(sender) => {
            if let Err(err) = sender.send(response) {
                warn!("failed to resolve pending response for {id:?}: {err:?}");
            }
        }
        None => {
            debug!("dropping bridge response for unknown synthetic request ID {id:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use codex_arg0::Arg0DispatchPaths;
    use codex_core::config::ConfigBuilder;
    use pretty_assertions::assert_eq;
    use rmcp::model::ClientRequest;
    use rmcp::model::JsonRpcRequest;
    use rmcp::model::ListToolsRequest;
    use rmcp::model::RequestId;
    use tempfile::TempDir;
    use tokio::time::timeout;

    use super::SessionRuntime;

    #[tokio::test]
    async fn session_runtime_uses_per_session_synthetic_request_ids() -> Result<()> {
        let (_temp_dir_one, runtime_one) = create_session_runtime().await?;
        let (_temp_dir_two, runtime_two) = create_session_runtime().await?;

        let pending_one = runtime_one.reserve_pending_response().await;
        let pending_two = runtime_two.reserve_pending_response().await;
        let pending_three = runtime_one.reserve_pending_response().await;

        assert_eq!(pending_one.id, RequestId::Number(0));
        assert_eq!(pending_two.id, RequestId::Number(0));
        assert_eq!(pending_three.id, RequestId::Number(1));
        assert_eq!(runtime_one.pending_request_count().await, 2);
        assert_eq!(runtime_two.pending_request_count().await, 1);

        drop(pending_one.receiver);
        drop(pending_two.receiver);
        drop(pending_three.receiver);

        Ok(())
    }

    #[tokio::test]
    async fn session_runtime_keeps_pending_maps_isolated_across_sessions() -> Result<()> {
        let (_temp_dir_one, runtime_one) = create_session_runtime().await?;
        let (_temp_dir_two, runtime_two) = create_session_runtime().await?;

        let pending_one = runtime_one.reserve_pending_response().await;
        let pending_two = runtime_two.reserve_pending_response().await;

        runtime_one
            .process_request(JsonRpcRequest {
                jsonrpc: rmcp::model::JsonRpcVersion2_0,
                id: pending_one.id.clone(),
                request: ClientRequest::ListToolsRequest(ListToolsRequest::default()),
            })
            .await;

        let response_one = timeout(Duration::from_secs(1), pending_one.receiver).await???;
        let response_two = timeout(Duration::from_millis(100), pending_two.receiver).await;

        assert_eq!(
            response_one
                .get("tools")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(
            runtime_one.pending_request_ids().await,
            Vec::<RequestId>::new()
        );
        assert_eq!(
            runtime_two.pending_request_ids().await,
            vec![RequestId::Number(0)]
        );
        assert!(
            response_two.is_err(),
            "second session should remain pending"
        );

        Ok(())
    }

    async fn create_session_runtime() -> Result<(TempDir, SessionRuntime)> {
        let temp_dir = TempDir::new()?;
        let config = Arc::new(
            ConfigBuilder::default()
                .codex_home(temp_dir.path().to_path_buf())
                .build()
                .await?,
        );

        Ok((
            temp_dir,
            SessionRuntime::new(Arg0DispatchPaths::default(), config),
        ))
    }
}
