use super::*;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

#[tokio::test]
async fn an_abandoned_correlated_start_cannot_defer_every_later_event_forever() {
    for cancelled in [false, true] {
        let (client, server) = tokio::io::duplex(4096);
        let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
            client,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let mut server = tokio_tungstenite::WebSocketStream::from_raw_socket(
            server,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;
        let client = std::sync::Arc::new(ProtocolClient::new(client, "fixture-start".into(), None));
        let mut events = client.subscribe();
        let request = {
            let client = std::sync::Arc::clone(&client);
            tokio::spawn(async move {
                client
                    .request(
                        "turn/start",
                        json!({
                            "threadId":"fixture-thread",
                            "responsesapiClientMetadata":{"cccc_turn_correlation_id":"fixture-input"}
                        }),
                        Duration::from_millis(100),
                    )
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(1), server.next())
            .await
            .expect("start request deadline")
            .expect("start request frame")
            .expect("start request");
        server
            .send(Message::Text(
                json!({"method":"turn/started","params":{"threadId":"fixture-thread","turn":{"id":"unknown-turn"}}})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("deferred event");
        if cancelled {
            request.abort();
            assert!(request.await.expect_err("request cancelled").is_cancelled());
        } else {
            assert_eq!(
                request
                    .await
                    .expect("request task")
                    .expect_err("unconfirmed start")
                    .kind(),
                io::ErrorKind::TimedOut
            );
        }
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("unconfirmed admission must become an explicit disconnect")
            .expect("disconnect event");
        assert_eq!(
            event.message["method"],
            super::super::MANAGED_AGENT_DISCONNECTED_METHOD
        );
        assert_eq!(
            event.message["params"]["diagnostic"]["code"],
            "admission_response_lost"
        );
        assert_eq!(
            client
                .request("thread/read", json!({}), Duration::from_millis(100))
                .await
                .expect_err("unresolved session is not reusable")
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        client.close().await;
    }
}

#[tokio::test]
async fn a_read_timeout_does_not_disconnect_a_healthy_session_or_hold_its_events() {
    let (client, server) = tokio::io::duplex(4096);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        client,
        tokio_tungstenite::tungstenite::protocol::Role::Client,
        None,
    )
    .await;
    let mut server = tokio_tungstenite::WebSocketStream::from_raw_socket(
        server,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    let client = ProtocolClient::new(client, "fixture-read-timeout".into(), None);
    let mut events = client.subscribe();
    let error = client
        .request("thread/read", json!({}), Duration::from_millis(20))
        .await
        .expect_err("fixture omits read reply");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    server
        .send(Message::Text(
            json!({"method":"turn/started","params":{"threadId":"fixture-thread","turn":{"id":"later-turn"}}})
                .to_string()
                .into(),
        ))
        .await
        .expect("later event");
    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("healthy event deadline")
        .expect("healthy event");
    assert_eq!(event.message["method"], "turn/started");
    assert_eq!(event.message["params"]["turn"]["id"], "later-turn");
    assert!(events.try_recv().is_err());
    client.close().await;
}

#[tokio::test]
async fn remote_close_records_code_without_logging_peer_reason() {
    let (client, server) = tokio::io::duplex(4096);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        client,
        tokio_tungstenite::tungstenite::protocol::Role::Client,
        None,
    )
    .await;
    let mut server = tokio_tungstenite::WebSocketStream::from_raw_socket(
        server,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    let client = ProtocolClient::new(client, "fixture-generation".into(), None);
    let mut events = client.subscribe();
    server
        .send(Message::Close(Some(CloseFrame {
            code: CloseCode::Away,
            reason: "private peer reason and token".into(),
        })))
        .await
        .expect("close");
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("event timeout")
        .expect("event");
    let diagnostic = &event.message["params"]["diagnostic"];
    assert_eq!(diagnostic["code"], "remote_close");
    assert_eq!(diagnostic["close_code"], 1001);
    assert_eq!(diagnostic["stage"], "app_server");
    assert_eq!(diagnostic["generation"], "fixture-generation");
    assert!(!diagnostic.to_string().contains("private"));
    assert!(!diagnostic.to_string().contains("token"));
    client.close().await;
}

#[test]
fn io_diagnostics_preserve_os_code_without_private_error_details() {
    let error = tokio_tungstenite::tungstenite::Error::Io(io::Error::from_raw_os_error(10054));
    let diagnostic = transport_diagnostic("read_failed", &error);
    assert_eq!(diagnostic["error_kind"], "io");
    assert_eq!(diagnostic["os_error"], 10054);
    let error = tokio_tungstenite::tungstenite::Error::Io(io::Error::other("private response"));
    assert!(
        !transport_diagnostic("read_failed", &error)
            .to_string()
            .contains("private")
    );
}

#[cfg(unix)]
#[test]
fn exit_observation_precedes_cleanup_and_does_not_terminate_a_live_child() {
    use std::process::Command;
    use std::sync::Arc;
    for should_exit in [false, true] {
        let (child, tree) = cccc_runtime::OwnedProcessTree::spawn(Command::new("sh").args([
            "-c",
            if should_exit {
                "exit 23"
            } else {
                "exec sleep 60"
            },
        ]))
        .expect("fixture");
        let process = Arc::new(super::super::ChildOwner::new(child, tree));
        let started = Instant::now();
        if should_exit {
            while process.running() && started.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let diagnostic = disconnect_diagnostic(
            json!({"code":"read_failed"}),
            "fixture",
            started,
            Some(Arc::downgrade(&process)),
        );
        if should_exit {
            assert_eq!(diagnostic["process_state"], "exited");
            assert_eq!(diagnostic["exit_code"], 23);
        } else {
            assert_eq!(diagnostic["process_state"], "running");
            assert!(process.running());
        }
        process.stop().expect("cleanup");
    }
}

#[tokio::test]
async fn abrupt_peer_eof_is_distinct_from_a_close_frame_or_invalid_protocol() {
    let (client, server) = tokio::io::duplex(4096);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        client,
        tokio_tungstenite::tungstenite::protocol::Role::Client,
        None,
    )
    .await;
    let client = ProtocolClient::new(client, "fixture-eof".into(), None);
    let mut events = client.subscribe();
    drop(server);
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("disconnect deadline")
        .expect("disconnect event");
    let diagnostic = &event.message["params"]["diagnostic"];
    assert_eq!(diagnostic["code"], "read_failed");
    assert_eq!(
        diagnostic["protocol_error"],
        "reset_without_close_handshake"
    );
    let malformed = tokio_tungstenite::tungstenite::Error::Protocol(
        tokio_tungstenite::tungstenite::error::ProtocolError::InvalidOpcode(15),
    );
    assert_eq!(
        transport_diagnostic("read_failed", &malformed)["protocol_error"],
        "invalid_websocket_protocol"
    );
    client.close().await;
}

#[tokio::test]
async fn native_approval_round_trip_and_disconnect_are_fenced() {
    let (client, server) = tokio::io::duplex(8192);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        client,
        tokio_tungstenite::tungstenite::protocol::Role::Client,
        None,
    )
    .await;
    let mut server = tokio_tungstenite::WebSocketStream::from_raw_socket(
        server,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    let client = ProtocolClient::new(client, "new-generation".into(), None);
    client.bind_approval_thread("t").expect("thread");
    let mut events = client.subscribe();
    for message in [
        json!({"method":"turn/started","params":{"threadId":"t","turn":{"id":"u"}}}),
        json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"t","turnId":"u","itemId":"i","command":"pwd","cwd":"/fixture","secret":"omit unrelated frame"}}),
    ] {
        server
            .send(Message::Text(message.to_string().into()))
            .await
            .expect("fixture event");
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("start timeout")
        .expect("start");
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("approval timeout")
        .expect("approval");
    assert_eq!(event.message["method"], "cccc/approvalRequired");
    assert!(!json!(client.permissions()).to_string().contains("secret"));
    assert_eq!(client.permissions().len(), 1);
    client
        .respond_permission("\"approval\"", false)
        .await
        .expect("human denial");
    let frame = server.next().await.expect("frame").expect("response");
    let response: Value = serde_json::from_str(frame.to_text().expect("text")).expect("json");
    assert_eq!(
        response,
        json!({"jsonrpc":"2.0","id":"approval","result":{"decision":"decline"}})
    );
    assert!(
        client
            .respond_permission("\"approval\"", true)
            .await
            .is_err()
    );
    server.close(None).await.expect("disconnect");
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("close timeout")
        .expect("close event");
    assert_eq!(
        event.message["method"],
        super::super::MANAGED_AGENT_DISCONNECTED_METHOD
    );
    assert!(client.permissions().is_empty());
    client.close().await;
}

#[tokio::test]
async fn native_stdio_real_codex_preserves_policy_and_resumes_same_thread() {
    let Ok(binary) = std::env::var("CCCC_CODEX_TEST_BINARY") else {
        return;
    };
    let temp = tempfile::tempdir().expect("owned fixture");
    let work = temp.path().join("workspace");
    let home = temp.path().join("codex-home");
    std::fs::create_dir_all(&work).expect("workspace");
    std::fs::create_dir_all(&home).expect("home");
    let mut environment = std::collections::BTreeMap::from([
        ("CODEX_HOME".into(), home.to_string_lossy().into_owned()),
        ("CCCC_CODEX_PRESERVE_POLICY".into(), "1".into()),
        ("CCCC_CODEX_STDIO".into(), "1".into()),
    ]);
    let command = vec![
        binary,
        "-c".into(),
        "approval_policy=\"on-request\"".into(),
        "-c".into(),
        "sandbox_mode=\"workspace-write\"".into(),
        "app-server".into(),
        "--listen".into(),
        "stdio://".into(),
    ];
    let session = super::super::AnalystSession::launch_prepared(
        super::super::WorkspaceBinding { root: work.clone() },
        Vec::new(),
        command.clone(),
        environment.clone(),
        None,
        super::super::SessionPurpose::Actor,
    )
    .await
    .expect("real stdio start and restrictive-policy assertion");
    assert!(session.structured_only());
    if let super::super::ManagedProtocol::Codex(protocol) = &session.protocol {
        let metadata = protocol
            .request(
                "thread/read",
                json!({"threadId":session.thread_id(),"includeTurns":false}),
                Duration::from_secs(10),
            )
            .await
            .expect("metadata without full history");
        assert_eq!(metadata["thread"]["historyMode"], "paginated");
        let page = protocol
            .request(
                "thread/turns/list",
                json!({"threadId":session.thread_id(),"limit":2,"itemsView":"summary"}),
                Duration::from_secs(10),
            )
            .await
            .expect("bounded native history page");
        assert_eq!(page["data"], json!([]));
    }
    let thread = session.thread_id().to_owned();
    let generation = session.generation().to_owned();
    session.stop(&generation).await.expect("old owner exits");
    let resumed = super::super::AnalystSession::launch_prepared(
        super::super::WorkspaceBinding { root: work },
        Vec::new(),
        command,
        environment.clone(),
        Some(thread.clone()),
        super::super::SessionPurpose::Actor,
    )
    .await
    .expect("same-thread restrictive resume");
    assert_eq!(resumed.thread_id(), thread);
    assert_ne!(resumed.generation(), generation);
    assert!(resumed.permissions().is_empty());
    assert!(
        resumed
            .respond_permission(&generation, "old-request", true)
            .await
            .is_err()
    );
    resumed
        .stop(resumed.generation())
        .await
        .expect("fixture cleanup");
    environment.insert("CCCC_CODEX_INITIAL_THREAD".into(), thread.clone());
    environment.insert("CCCC_CODEX_SCOPED_MCP".into(), "1".into());
    let cccc_home = cccc_core::HomeLayout::from_path(temp.path().join("native-host-home"))
        .expect("native home");
    let store = cccc_core::GroupStore::new(cccc_home.clone()).expect("store");
    let group = store.create("isolated adoption", "").expect("group");
    let actor_config = super::super::ActorLaunchConfig {
        workdir: home.parent().expect("fixture root").join("workspace"),
        group_id: group.group_id.clone(),
        actor_id: "fixture".into(),
        runtime: cccc_contracts::ActorRuntime::Codex,
        runtime_mode: cccc_contracts::RuntimeMode::Default,
        command: vec![
            std::env::var("CCCC_CODEX_TEST_BINARY").expect("test binary"),
            "-c".into(),
            "approval_policy=\"on-request\"".into(),
            "-c".into(),
            "sandbox_mode=\"workspace-write\"".into(),
        ],
        environment,
    };
    let adopted = super::super::AnalystSession::launch_actor(&cccc_home, actor_config)
        .await
        .expect("actual native initial-thread adoption without invented receipt");
    assert_eq!(adopted.thread_id(), thread);
    assert!(adopted.structured_only());
    if let super::super::ManagedProtocol::Codex(protocol) = &adopted.protocol {
        let config = protocol
            .request(
                "config/read",
                json!({"includeLayers":false}),
                Duration::from_secs(10),
            )
            .await
            .expect("effective scoped config");
        assert_eq!(config["config"]["approval_policy"], "on-request");
        assert_eq!(config["config"]["sandbox_mode"], "workspace-write");
    }
    let recorded = crate::ops::runtime_session::snapshot(&cccc_home, &group.group_id, "fixture")
        .expect("native record")
        .expect("real receipt");
    assert_eq!(recorded["provider_thread_id"], thread);
    assert_eq!(recorded["captured_from"], "app_server_thread_resume");
    adopted
        .stop(adopted.generation())
        .await
        .expect("adopted owner exits");
}

#[tokio::test]
async fn native_stdio_partial_frame_survives_outgoing_request() {
    let temp = tempfile::tempdir().expect("fixture");
    let script = "import sys,json\nsys.stdout.write('{\"method\":\"fixture/');sys.stdout.flush()\nr=json.loads(sys.stdin.readline())\nsys.stdout.write('ready\",\"params\":{}}\\n');sys.stdout.flush()\nprint(json.dumps({'id':r['id'],'result':{}}),flush=True)\nsys.stdin.read()\n";
    let (owner, stdin, stdout) = super::super::process::spawn_piped_quiet(
        &[
            "/usr/bin/python3".into(),
            "-u".into(),
            "-c".into(),
            script.into(),
        ],
        temp.path(),
        &Default::default(),
    )
    .expect("owned stdio fixture");
    let owner = std::sync::Arc::new(owner);
    let client = ProtocolClient::new_stdio(
        stdin,
        stdout,
        "stdio-partial".into(),
        Some(std::sync::Arc::downgrade(&owner)),
    );
    let mut events = client.subscribe();
    tokio::time::sleep(Duration::from_millis(50)).await;
    client
        .request("initialize", json!({}), Duration::from_secs(2))
        .await
        .expect("request settled");
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("notification retained")
        .expect("event");
    assert_eq!(event.message["method"], "fixture/ready");
    client.close().await;
    owner.stop().expect("owner cleanup");
}

#[tokio::test]
async fn host_only_credential_authenticates_native_websocket_without_changing_protocol() {
    let directory = tempfile::tempdir().expect("private credential fixture");
    let file = directory.path().join("token");
    let token = "a".repeat(64);
    std::fs::write(&file, format!("{token}\n")).expect("fixture token");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let endpoint = format!("ws://{}", listener.local_addr().expect("fixture endpoint"));
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("host client");
        let mut socket = tokio_tungstenite::accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
             response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                assert_eq!(
                    request
                        .headers()
                        .get("authorization")
                        .expect("host credential"),
                    format!("Bearer {}", "a".repeat(64)).as_str()
                );
                Ok(response)
            },
        )
        .await
        .expect("authenticated native handshake");
        let _ = socket.next().await;
    });
    let environment = std::collections::BTreeMap::from([(
        "CCCC_CODEX_WS_TOKEN_FILE".into(),
        file.to_string_lossy().into_owned(),
    )]);
    let mut socket = connect_with_environment(&endpoint, &environment)
        .await
        .expect("host native connection");
    socket.close(None).await.expect("close fixture");
    server.await.expect("handshake server");
    std::fs::write(&file, "not-a-credential").expect("invalid fixture");
    let error = connect_with_environment(&endpoint, &environment)
        .await
        .expect_err("invalid credential rejected before connecting");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!error.to_string().contains("not-a-credential"));
}
