use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;

use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use rmcp::ServerHandler;
use rmcp::model::CustomNotification;
use rmcp::model::ErrorData;
use rmcp::model::Implementation;
use rmcp::model::InitializeRequestParams;
use rmcp::model::InitializeResult;
use rmcp::model::JsonRpcRequest;
use rmcp::model::ProtocolVersion;
use rmcp::model::RequestId;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::model::ServerNotification;
use rmcp::service::Peer;
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
type SessionPeer = Peer<RoleServer>;

pub(crate) struct SessionRuntime {
    processor: Arc<Mutex<MessageProcessor>>,
    #[allow(dead_code)]
    next_synthetic_request_id: AtomicI64,
    #[allow(dead_code)]
    pending_responses: Arc<Mutex<HashMap<RequestId, PendingResponseSender>>>,
    peer: Arc<Mutex<Option<SessionPeer>>>,
    #[cfg(test)]
    outgoing_tx: mpsc::UnboundedSender<OutgoingMessage>,
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
        let peer = Arc::new(Mutex::new(None));
        let processor = Arc::new(Mutex::new(MessageProcessor::new(
            OutgoingMessageSender::new(outgoing_tx.clone()),
            arg0_paths,
            config,
        )));

        tokio::spawn(run_outgoing_bridge(
            outgoing_rx,
            pending_responses.clone(),
            peer.clone(),
        ));

        Self {
            processor,
            next_synthetic_request_id: AtomicI64::new(0),
            pending_responses,
            peer,
            #[cfg(test)]
            outgoing_tx,
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
    async fn send_outgoing_message(&self, message: OutgoingMessage) {
        let _ = self.outgoing_tx.send(message);
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
        *self.peer.lock().await = Some(context.peer.clone());

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
    peer: Arc<Mutex<Option<SessionPeer>>>,
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
                forward_notification(&peer, notification).await;
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

async fn forward_notification(
    peer: &Arc<Mutex<Option<SessionPeer>>>,
    notification: crate::outgoing_message::OutgoingNotification,
) {
    let session_peer = peer.lock().await.clone();
    let Some(session_peer) = session_peer else {
        debug!(
            method = notification.method,
            "dropping session notification before peer initialization"
        );
        return;
    };

    let server_notification = ServerNotification::CustomNotification(CustomNotification::new(
        notification.method.clone(),
        notification.params,
    ));
    if let Err(err) = session_peer.send_notification(server_notification).await {
        warn!(
            method = notification.method,
            "failed to forward session notification to peer: {err}"
        );
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
    use rmcp::RoleServer;
    use rmcp::model::ClientCapabilities;
    use rmcp::model::ClientJsonRpcMessage;
    use rmcp::model::ClientNotification;
    use rmcp::model::ClientRequest;
    use rmcp::model::ErrorData;
    use rmcp::model::Extensions;
    use rmcp::model::Implementation;
    use rmcp::model::InitializeRequestParams;
    use rmcp::model::InitializedNotification;
    use rmcp::model::JsonRpcRequest;
    use rmcp::model::JsonRpcVersion2_0;
    use rmcp::model::ListToolsRequest;
    use rmcp::model::ProtocolVersion;
    use rmcp::model::Request;
    use rmcp::model::RequestId;
    use rmcp::model::ServerJsonRpcMessage;
    use rmcp::model::ServerNotification;
    use rmcp::model::ServerResult;
    use rmcp::serve_server;
    use rmcp::transport::Transport;
    use rmcp::transport::TransportAdapterIdentity;
    use tempfile::TempDir;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    use crate::outgoing_message::OutgoingError;
    use crate::outgoing_message::OutgoingMessage;
    use crate::outgoing_message::OutgoingNotification;
    use crate::outgoing_message::OutgoingResponse;

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
    async fn session_runtime_routes_response_and_error_to_matching_pending_waiters() -> Result<()> {
        let (_temp_dir, runtime) = create_session_runtime().await?;

        let pending_response = runtime.reserve_pending_response().await;
        let pending_error = runtime.reserve_pending_response().await;

        runtime
            .send_outgoing_message(OutgoingMessage::Response(OutgoingResponse {
                id: pending_response.id.clone(),
                result: serde_json::json!({ "ok": true }),
            }))
            .await;
        runtime
            .send_outgoing_message(OutgoingMessage::Error(OutgoingError {
                id: pending_error.id.clone(),
                error: ErrorData::invalid_request("bad request", None),
            }))
            .await;

        let response = timeout(Duration::from_secs(1), pending_response.receiver).await??;
        let error = timeout(Duration::from_secs(1), pending_error.receiver).await??;

        assert_eq!(
            response,
            Ok(serde_json::json!({
                "ok": true,
            }))
        );
        assert_eq!(error, Err(ErrorData::invalid_request("bad request", None)));
        assert_eq!(runtime.pending_request_ids().await, Vec::<RequestId>::new());

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
                jsonrpc: JsonRpcVersion2_0,
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

    #[tokio::test]
    async fn session_runtime_forwards_notifications_to_peer_custom_notification_channel()
    -> Result<()> {
        let (_temp_dir, runtime) = create_session_runtime().await?;
        let (transport, mut handle) = test_server_transport();

        handle.send_initialize_request().await?;
        handle.send_initialized_notification().await?;
        let running_service =
            serve_server::<_, _, _, TransportAdapterIdentity>(runtime, transport).await?;

        let initialize_response = timeout(Duration::from_secs(1), handle.recv()).await??;
        match initialize_response {
            ServerJsonRpcMessage::Response(response) => {
                assert_eq!(response.id, RequestId::Number(1));
                match response.result {
                    ServerResult::InitializeResult(_) => {}
                    ServerResult::EmptyResult(_)
                    | ServerResult::CompleteResult(_)
                    | ServerResult::GetPromptResult(_)
                    | ServerResult::ListPromptsResult(_)
                    | ServerResult::ListResourcesResult(_)
                    | ServerResult::ListResourceTemplatesResult(_)
                    | ServerResult::ReadResourceResult(_)
                    | ServerResult::CallToolResult(_)
                    | ServerResult::ListToolsResult(_)
                    | ServerResult::CreateElicitationResult(_)
                    | ServerResult::CustomResult(_)
                    | ServerResult::CreateTaskResult(_)
                    | ServerResult::ListTasksResult(_)
                    | ServerResult::GetTaskInfoResult(_)
                    | ServerResult::TaskResult(_) => anyhow::bail!("expected initialize result"),
                }
            }
            ServerJsonRpcMessage::Request(_)
            | ServerJsonRpcMessage::Notification(_)
            | ServerJsonRpcMessage::Error(_) => {
                anyhow::bail!("expected initialize response")
            }
        }

        running_service
            .service()
            .send_outgoing_message(OutgoingMessage::Notification(OutgoingNotification {
                method: "codex/event".to_string(),
                params: Some(serde_json::json!({
                    "id": "event-1",
                    "msg": {
                        "type": "agent_message",
                    },
                })),
            }))
            .await;

        let notification = timeout(Duration::from_secs(1), handle.recv()).await??;
        match notification {
            ServerJsonRpcMessage::Notification(notification) => match notification.notification {
                ServerNotification::CustomNotification(custom) => {
                    assert_eq!(custom.method, "codex/event");
                    assert_eq!(
                        custom.params,
                        Some(serde_json::json!({
                            "id": "event-1",
                            "msg": {
                                "type": "agent_message",
                            },
                        }))
                    );
                }
                ServerNotification::CancelledNotification(_)
                | ServerNotification::ProgressNotification(_)
                | ServerNotification::LoggingMessageNotification(_)
                | ServerNotification::ResourceUpdatedNotification(_)
                | ServerNotification::ResourceListChangedNotification(_)
                | ServerNotification::ToolListChangedNotification(_)
                | ServerNotification::PromptListChangedNotification(_)
                | ServerNotification::ElicitationCompletionNotification(_) => {
                    anyhow::bail!("expected custom notification")
                }
            },
            ServerJsonRpcMessage::Request(_)
            | ServerJsonRpcMessage::Response(_)
            | ServerJsonRpcMessage::Error(_) => anyhow::bail!("expected notification"),
        }

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

    struct TestServerTransport {
        incoming: mpsc::Receiver<ClientJsonRpcMessage>,
        outgoing: mpsc::Sender<ServerJsonRpcMessage>,
    }

    struct TestServerTransportHandle {
        incoming: mpsc::Sender<ClientJsonRpcMessage>,
        outgoing: mpsc::Receiver<ServerJsonRpcMessage>,
    }

    fn test_server_transport() -> (TestServerTransport, TestServerTransportHandle) {
        let (incoming_tx, incoming_rx) = mpsc::channel(8);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(8);
        (
            TestServerTransport {
                incoming: incoming_rx,
                outgoing: outgoing_tx,
            },
            TestServerTransportHandle {
                incoming: incoming_tx,
                outgoing: outgoing_rx,
            },
        )
    }

    impl Transport<RoleServer> for TestServerTransport {
        type Error = tokio::sync::mpsc::error::SendError<ServerJsonRpcMessage>;

        fn send(
            &mut self,
            item: ServerJsonRpcMessage,
        ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
            let outgoing = self.outgoing.clone();
            async move { outgoing.send(item).await }
        }

        async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
            self.incoming.recv().await
        }

        fn close(&mut self) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
            std::future::ready(Ok(()))
        }
    }

    impl TestServerTransportHandle {
        async fn send_initialize_request(&self) -> Result<()> {
            self.incoming
                .send(ClientJsonRpcMessage::request(
                    ClientRequest::InitializeRequest(Request {
                        method: Default::default(),
                        params: InitializeRequestParams {
                            meta: None,
                            capabilities: ClientCapabilities::default(),
                            client_info: Implementation {
                                name: "session-runtime-test".to_string(),
                                title: Some("Session Runtime Test".to_string()),
                                version: "0.0.0".to_string(),
                                description: None,
                                icons: None,
                                website_url: None,
                            },
                            protocol_version: ProtocolVersion::V_2025_03_26,
                        },
                        extensions: Extensions::default(),
                    }),
                    RequestId::Number(1),
                ))
                .await?;
            Ok(())
        }

        async fn send_initialized_notification(&self) -> Result<()> {
            self.incoming
                .send(ClientJsonRpcMessage::notification(
                    ClientNotification::InitializedNotification(InitializedNotification {
                        method: Default::default(),
                        extensions: Extensions::default(),
                    }),
                ))
                .await?;
            Ok(())
        }

        async fn recv(&mut self) -> Result<ServerJsonRpcMessage> {
            self.outgoing
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("transport closed"))
        }
    }
}
