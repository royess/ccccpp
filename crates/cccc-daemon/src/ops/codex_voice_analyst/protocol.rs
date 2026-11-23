use super::AnalystEvent;
#[cfg(test)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex, Weak};
#[path = "protocol_approvals.rs"]
mod approvals;
#[path = "protocol_transport.rs"]
mod transport;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
const EVENT_CAPACITY: usize = 2048;
const COMMAND_CAPACITY: usize = 32;

/// `excludeTurns` omits turn history, but thread metadata such as `preview` can still
/// exceed the default 16 MiB frame limit. Allow a single frame up to the existing
/// 64 MiB message limit: enough for the observed 20 MiB and 31 MB replies, while
/// retaining the same aggregate bound for fragmented messages.
fn app_server_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(64 << 20))
        .max_message_size(Some(64 << 20))
}

#[path = "protocol_turn_scope.rs"]
mod turn_scope;
use turn_scope::{PendingResponse, competing_turn_started};

#[cfg(test)]
pub(super) async fn connect_with_retry(
    endpoint: &str,
) -> io::Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    connect_with_token(endpoint, None).await
}

pub(super) async fn connect_with_environment(
    endpoint: &str,
    environment: &std::collections::BTreeMap<String, String>,
) -> io::Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let token = environment
        .get("CCCC_CODEX_WS_TOKEN_FILE")
        .map(std::fs::read_to_string)
        .transpose()?;
    if token
        .as_ref()
        .is_some_and(|v| v.trim().len() != 64 || !v.trim().bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid native transport credential",
        ));
    }
    connect_with_token(endpoint, token.as_deref().map(str::trim)).await
}

async fn connect_with_token(
    endpoint: &str,
    token: Option<&str>,
) -> io::Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
    loop {
        let mut request = endpoint
            .into_client_request()
            .map_err(|_| io::Error::other("invalid native transport endpoint"))?;
        if let Some(token) = token {
            request.headers_mut().insert(
                "Authorization",
                format!("Bearer {token}")
                    .parse()
                    .map_err(|_| io::Error::other("invalid native transport credential"))?,
            );
        }
        match tokio_tungstenite::connect_async_with_config(
            request,
            Some(app_server_websocket_config()),
            false,
        )
        .await
        {
            Ok((socket, _)) => return Ok(socket),
            Err(error) if tokio::time::Instant::now() < deadline => {
                tracing::debug!(%error, "waiting for Codex app-server websocket");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("could not connect to Codex app-server: {error}"),
                ));
            }
        }
    }
}

struct RpcRequest {
    method: String,
    params: Value,
    response: oneshot::Sender<io::Result<Value>>,
}

enum ProtocolCommand {
    Request(RpcRequest),
    Respond {
        id: Value,
        result: Value,
    },
    RespondError {
        id: Value,
        error: Value,
    },
    Permission {
        request_id: String,
        allow: bool,
        response: oneshot::Sender<io::Result<()>>,
    },
    Close,
}

pub(super) struct ProtocolClient {
    commands: mpsc::Sender<ProtocolCommand>,
    pub(super) events: broadcast::Sender<AnalystEvent>,
    task: Mutex<Option<JoinHandle<()>>>,
    approvals: Arc<Mutex<approvals::Approvals>>,
}

impl ProtocolClient {
    pub(super) fn new<S>(
        socket: tokio_tungstenite::WebSocketStream<S>,
        generation: String,
        process: Option<Weak<super::ChildOwner>>,
    ) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        Self::from_transport(
            transport::Transport::WebSocket(Box::new(socket)),
            generation,
            process,
        )
    }
    #[cfg(unix)]
    pub(super) fn new_stdio(
        stdin: std::process::ChildStdin,
        stdout: std::process::ChildStdout,
        generation: String,
        process: Option<Weak<super::ChildOwner>>,
    ) -> Self {
        Self::from_transport(
            transport::Transport::stdio(stdin, stdout),
            generation,
            process,
        )
    }
    fn from_transport(
        socket: transport::Transport,
        generation: String,
        process: Option<Weak<super::ChildOwner>>,
    ) -> Self {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let approvals = Arc::new(Mutex::new(approvals::Approvals::default()));
        let task = tokio::spawn(protocol_loop(
            socket,
            receiver,
            events.clone(),
            generation,
            process,
            Arc::clone(&approvals),
        ));
        Self {
            commands,
            events,
            task: Mutex::new(Some(task)),
            approvals,
        }
    }

    pub(super) fn bind_approval_thread(&self, thread: &str) -> io::Result<()> {
        self.approvals
            .lock()
            .map_err(|_| io::Error::other("approval lock failed"))?
            .bind_thread(thread);
        Ok(())
    }
    pub(super) fn permissions(&self) -> Vec<Value> {
        self.approvals
            .lock()
            .map(|a| a.visible())
            .unwrap_or_default()
    }
    pub(super) async fn respond_permission(&self, request_id: &str, allow: bool) -> io::Result<()> {
        let (response, receiver) = oneshot::channel();
        self.commands
            .send(ProtocolCommand::Permission {
                request_id: request_id.into(),
                allow,
                response,
            })
            .await
            .map_err(|_| io::Error::other("app-server is closed"))?;
        receiver
            .await
            .map_err(|_| io::Error::other("app-server response outcome unknown"))?
    }
    pub(super) fn subscribe(&self) -> broadcast::Receiver<AnalystEvent> {
        self.events.subscribe()
    }

    pub(super) async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> io::Result<Value> {
        let work = self.request_inner(method, params, timeout);
        match super::lifecycle_timing::request_phase(method) {
            Some(phase) => super::lifecycle_timing::run(phase, work).await,
            None => work.await,
        }
    }

    async fn request_inner(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> io::Result<Value> {
        let (sender, receiver) = oneshot::channel();
        self.commands
            .send(ProtocolCommand::Request(RpcRequest {
                method: method.into(),
                params,
                response: sender,
            }))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "app-server is closed"))?;
        tokio::time::timeout(timeout, receiver)
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("Codex app-server request timed out: {method}"),
                )
            })?
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "app-server is closed"))?
    }

    pub(super) async fn respond(&self, id: Value, result: Value) -> io::Result<()> {
        self.commands
            .send(ProtocolCommand::Respond { id, result })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "app-server is closed"))
    }

    pub(super) async fn respond_error(&self, id: Value, error: Value) -> io::Result<()> {
        self.commands
            .send(ProtocolCommand::RespondError { id, error })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "app-server is closed"))
    }

    pub(super) async fn close(&self) {
        let _ = self.commands.send(ProtocolCommand::Close).await;
        let task = self.task.lock().ok().and_then(|mut task| task.take());
        if let Some(mut task) = task
            && tokio::time::timeout(STOP_TIMEOUT, &mut task).await.is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for ProtocolClient {
    fn drop(&mut self) {
        if let Ok(mut task) = self.task.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}

async fn protocol_loop(
    mut socket: transport::Transport,
    mut commands: mpsc::Receiver<ProtocolCommand>,
    events: broadcast::Sender<AnalystEvent>,
    generation: String,
    process: Option<Weak<super::ChildOwner>>,
    approvals: Arc<Mutex<approvals::Approvals>>,
) {
    let started = Instant::now();
    let mut next_id = 1_u64;
    let mut pending: HashMap<u64, PendingResponse> = HashMap::new();
    let mut pending_turn_start = None;
    let mut deferred_events = VecDeque::new();
    let mut turn_delegations = HashMap::new();
    let (terminal_error, diagnostic) = loop {
        tokio::select! {
            _ = async {
                if let Some(response) = pending_turn_start.and_then(|id| pending.get_mut(&id)) {
                    response.response.closed().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                // A timed-out or cancelled owner can no longer resolve the admission
                // barrier. Do not silently buffer every subsequent event forever,
                // guess the turn's owner, or replay a possibly accepted request.
                break ("correlated Codex turn/start lost its response owner".into(), json!({"code":"admission_response_lost"}));
            }
            command = commands.recv() => match command {
                Some(ProtocolCommand::Request(request)) => {
                    let id = next_id;
                    next_id = next_id.saturating_add(1);
                    let turn_delegation_id = (request.method == "turn/start")
                        .then(|| {
                            let metadata = request.params.get("responsesapiClientMetadata")?;
                            metadata
                                .get(super::CODEX_TURN_CORRELATION_KEY)
                                .or_else(|| metadata.get("cccc_voice_delegation_id"))?
                                .as_str()
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(str::to_owned)
                        })
                        .flatten();
                    let message = json!({
                        "jsonrpc":"2.0", "id":id,
                        "method":request.method, "params":request.params,
                    });
                    if turn_delegation_id.is_some() {
                        if pending_turn_start.is_some() {
                            let _ = request.response.send(Err(io::Error::new(
                                io::ErrorKind::WouldBlock,
                                "another correlated Codex turn/start request is unresolved",
                            )));
                            continue;
                        }
                        pending_turn_start = Some(id);
                    }
                    pending.insert(id, PendingResponse::new(request.response, turn_delegation_id, &request.params));
                    if let Err(error) = socket.send(Message::Text(message.to_string().into())).await {
                        break (format!("failed to write app-server request: {error}"), transport_diagnostic("request_write_failed", &error));
                    }
                }
                Some(ProtocolCommand::Permission { request_id, allow, response }) => {
                    let decision = approvals.lock().map_err(|_|io::Error::other("approval lock failed")).and_then(|mut a|a.take(&request_id,allow));
                    let (id,result)=match decision { Ok(v)=>v,Err(e)=>{let _=response.send(Err(e));continue;} };
                    if let Err(error)=socket.send(Message::Text(json!({"jsonrpc":"2.0","id":id,"result":result}).to_string().into())).await {
                        let _=response.send(Err(io::Error::other("approval write outcome unknown; do not replay")));
                        break ("approval response write failed".into(),transport_diagnostic("response_write_failed",&error));
                    }
                    let _=response.send(Ok(()));
                }
                Some(ProtocolCommand::Respond { id, result }) => {
                    let message = json!({"jsonrpc":"2.0","id":id,"result":result});
                    if let Err(error) = socket.send(Message::Text(message.to_string().into())).await {
                        break (format!("failed to write app-server response: {error}"), transport_diagnostic("response_write_failed", &error));
                    }
                }
                Some(ProtocolCommand::RespondError { id, error }) => {
                    let message = json!({"jsonrpc":"2.0","id":id,"error":error});
                    if let Err(error) = socket.send(Message::Text(message.to_string().into())).await {
                        break (format!("failed to write app-server response: {error}"), transport_diagnostic("response_write_failed", &error));
                    }
                }
                Some(ProtocolCommand::Close) | None => {
                    let _ = socket.close().await;
                    break ("app-server client closed".to_owned(), json!({"code":"client_closed"}));
                }
            },
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let Ok(message) = serde_json::from_str::<Value>(&text) else { continue };
                    if message.get("method").is_some() {
                        if let Ok(mut a)=approvals.lock() { a.observe(&message); }
                        if approvals::Approvals::supported(&message) {
                            let inserted=approvals.lock().map_err(|_|io::Error::other("approval lock failed")).and_then(|mut a|a.insert(&message));
                            match inserted {
                                Ok(true)=> { let _=events.send(AnalystEvent { generation:generation.clone(),message:json!({"method":"cccc/approvalRequired","params":{"threadId":message["params"]["threadId"],"turnId":message["params"]["turnId"]}}),requested_delegation_id:None }); }
                                Ok(false)=>{}
                                Err(_)=> { break ("invalid native approval scope".into(),json!({"code":"approval_scope_mismatch"})); }
                            }
                            continue;
                        }
                        if pending_turn_start.is_some() {
                            if deferred_events.len() >= EVENT_CAPACITY {
                                break ("Codex app-server produced too many events before turn/start settled".into(), json!({"code":"admission_event_overflow"}));
                            }
                            deferred_events.push_back(message);
                        } else {
                            publish_event(
                                &events,
                                &generation,
                                message,
                                &mut turn_delegations,
                            );
                        }
                        continue;
                    }
                    let Some(id) = message.get("id").and_then(Value::as_u64) else { continue };
                    let Some(pending_response) = pending.remove(&id) else { continue };
                    let result = if let Some(error) = message.get("error") {
                        Err(io::Error::other(format!(
                            "Codex app-server request failed: {error}"
                        )))
                    } else {
                        Ok(message.get("result").cloned().unwrap_or(Value::Null))
                    };
                    if pending_turn_start == Some(id) {
                        pending_turn_start = None;
                        let response_turn_id = result
                            .as_ref()
                            .ok()
                            .and_then(|result| result
                                .get("turn")
                                .and_then(|turn| turn.get("id"))
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|value| !value.is_empty()));
                        if let Some(response_turn_id) = response_turn_id
                            && competing_turn_started(
                                &deferred_events,
                                response_turn_id,
                                pending_response.thread_id.as_deref(),
                            )
                        {
                            break ("a competing terminal turn started while a correlated Codex request was pending".into(), json!({"code":"competing_turn"}));
                        }
                        if let (Some(delegation_id), Some(turn_id)) =
                            (pending_response.turn_delegation_id.as_ref(), response_turn_id)
                        {
                            turn_delegations.insert(turn_id.to_owned(), delegation_id.clone());
                        }
                        while let Some(event) = deferred_events.pop_front() {
                            publish_event(
                                &events,
                                &generation,
                                event,
                                &mut turn_delegations,
                            );
                        }
                    }
                    let _ = pending_response.response.send(result);
                }
                Some(Ok(Message::Ping(payload))) => {
                    if let Err(error) = socket.send(Message::Pong(payload)).await {
                        break (format!("failed to answer app-server ping: {error}"), transport_diagnostic("pong_write_failed", &error));
                    }
                }
                Some(Ok(Message::Close(frame))) => break (
                    "app-server websocket closed".to_owned(),
                    json!({"code":"remote_close","close_code":frame.map(|frame| u16::from(frame.code))}),
                ),
                None => break ("app-server websocket closed".to_owned(), json!({"code":"stream_ended"})),
                Some(Ok(_)) => {}
                Some(Err(error)) => break (format!("app-server websocket failed: {error}"), transport_diagnostic("read_failed", &error)),
            }
        }
    };
    if let Ok(mut a) = approvals.lock() {
        a.clear();
    }
    let diagnostic = disconnect_diagnostic(diagnostic, &generation, started, process);
    if diagnostic["code"] != "client_closed" {
        // Packaged launches do not require a tracing subscriber. Keep this one-shot
        // report structural: no close reason, arbitrary error, protocol body or path.
        cccc_runtime::diagnostic(format_args!(
            "[cccc] Managed Codex disconnected: {diagnostic}"
        ));
    }
    for (_, pending_response) in pending {
        let _ = pending_response.response.send(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            terminal_error.clone(),
        )));
    }
    let _ = events.send(AnalystEvent {
        generation,
        message: json!({"method":super::MANAGED_AGENT_DISCONNECTED_METHOD,"params":{"reason":terminal_error,"diagnostic":diagnostic}}),
        requested_delegation_id: None,
    });
}

fn transport_diagnostic(
    code: &'static str,
    error: &tokio_tungstenite::tungstenite::Error,
) -> Value {
    use tokio_tungstenite::tungstenite::Error;
    let (kind, os_error) = match error {
        Error::Io(error) => ("io", error.raw_os_error()),
        Error::Protocol(_) => ("protocol", None),
        Error::ConnectionClosed | Error::AlreadyClosed => ("closed", None),
        Error::Capacity(_) => ("capacity", None),
        _ => ("other", None),
    };
    let mut diagnostic = json!({"code":code,"error_kind":kind,"os_error":os_error});
    if let Error::Protocol(error) = error {
        use tokio_tungstenite::tungstenite::error::ProtocolError;
        diagnostic["protocol_error"] = json!(match error {
            ProtocolError::ResetWithoutClosingHandshake => "reset_without_close_handshake",
            ProtocolError::SendAfterClosing => "send_after_closing",
            ProtocolError::ReceivedAfterClosing => "received_after_closing",
            _ => "invalid_websocket_protocol",
        });
    }
    diagnostic
}

fn disconnect_diagnostic(
    mut diagnostic: Value,
    generation: &str,
    started: Instant,
    process: Option<Weak<super::ChildOwner>>,
) -> Value {
    diagnostic["generation"] = json!(generation);
    diagnostic["stage"] = json!("app_server");
    diagnostic["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
    diagnostic["time_unix_ms"] = json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    );
    diagnostic["process_state"] = json!("unavailable");
    if let Some(process) = process.and_then(|process| process.upgrade()) {
        diagnostic["process_id"] = json!(process.id());
        match process.exit_status() {
            Ok(None) => diagnostic["process_state"] = json!("running"),
            Ok(Some(status)) => {
                diagnostic["process_state"] = json!("exited");
                diagnostic["exit_code"] = json!(status.code());
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    diagnostic["exit_signal"] = json!(status.signal());
                }
            }
            Err(error) => diagnostic["process_status_os_error"] = json!(error.raw_os_error()),
        }
    }
    diagnostic
}

fn started_turn_id(message: &Value) -> Option<&str> {
    (message.get("method").and_then(Value::as_str) == Some("turn/started"))
        .then(|| {
            message
                .get("params")?
                .get("turn")?
                .get("id")?
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .flatten()
}

fn publish_event(
    events: &broadcast::Sender<AnalystEvent>,
    generation: &str,
    message: Value,
    turn_delegations: &mut HashMap<String, String>,
) {
    let method = message.get("method").and_then(Value::as_str);
    let turn_id = message
        .get("params")
        .and_then(|params| params.get("turn"))
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let requested_delegation_id = if method == Some("turn/started") {
        turn_id.and_then(|turn_id| turn_delegations.remove(turn_id))
    } else {
        if method == Some("turn/completed")
            && let Some(turn_id) = turn_id
        {
            turn_delegations.remove(turn_id);
        }
        None
    };
    let _ = events.send(AnalystEvent {
        generation: generation.to_owned(),
        message,
        requested_delegation_id,
    });
}

#[cfg(test)]
#[path = "protocol_disconnect_tests.rs"]
mod disconnect_tests;

#[cfg(test)]
#[path = "protocol_ws_config_tests.rs"]
mod ws_config_tests;
