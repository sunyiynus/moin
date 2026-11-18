use super::*;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    thread,
    time::Instant,
};

struct Mock {
    url: String,
    requests: mpsc::Receiver<(String, Value)>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Mock {
    fn new(responses: Vec<(u16, &'static str, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (send, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            for (status, content_type, body) in responses {
                let deadline = Instant::now() + Duration::from_secs(15);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        error => panic!("Mock server accept failed: {error:?}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let request = read_request(&mut socket);
                let _ = send.send(request);
                let header = format!(
                    "HTTP/1.1 {status} mock\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(header.as_bytes()).unwrap();
                socket.write_all(body.as_bytes()).unwrap();
            }
        });
        Self {
            url,
            requests,
            worker: Some(worker),
        }
    }

    fn request(&self) -> (String, Value) {
        self.requests.recv_timeout(Duration::from_secs(15)).unwrap()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn read_request(socket: &mut TcpStream) -> (String, Value) {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
        assert!(header.len() < 16 * 1024);
    }
    let header = String::from_utf8(header).unwrap();
    let length = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; length];
    socket.read_exact(&mut body).unwrap();
    (
        header.lines().next().unwrap().to_owned(),
        serde_json::from_slice(&body).unwrap(),
    )
}

fn sse(deltas: Vec<Value>, reason: &str) -> String {
    let mut data = deltas.into_iter().map(|delta| {
        format!("data: {}\n\n", json!({"id":"mock","object":"chat.completion.chunk","created":0,"model":"mock-model","choices":[{"index":0,"delta":delta,"finish_reason":null}]}))
    }).collect::<String>();
    data.push_str(&format!("data: {}\n\ndata: [DONE]\n\n", json!({"id":"mock","object":"chat.completion.chunk","created":0,"model":"mock-model","choices":[{"index":0,"delta":{},"finish_reason":reason}]})));
    data
}

fn compatible(base: &str) -> AiConfig {
    AiConfig {
        provider: AiProvider::OpenAiCompatible,
        base_url: format!("{base}/v1"),
        model: "mock-model".into(),
        api_key_env: String::new(),
        ..Default::default()
    }
}

#[test]
fn concurrent_conversations_keep_prompts_and_histories_isolated() {
    let a = Mock::new(vec![(
        200,
        "text/event-stream",
        sse(
            vec![json!({"role":"assistant","content":"会话 A 的回复"})],
            "stop",
        ),
    )]);
    let b = Mock::new(vec![(
        200,
        "text/event-stream",
        sse(
            vec![json!({"role":"assistant","content":"会话 B 的回复"})],
            "stop",
        ),
    )]);
    let folder = tempfile::tempdir().unwrap();
    let first = start_chat(
        compatible(&a.url),
        folder.path().join("a.sqlite"),
        vec![ChatMessage {
            role: ChatRole::User,
            content: "只属于 A 的历史".into(),
        }],
        "A 的新问题".into(),
    )
    .unwrap();
    let second = start_chat(
        compatible(&b.url),
        folder.path().join("b.sqlite"),
        vec![ChatMessage {
            role: ChatRole::User,
            content: "只属于 B 的历史".into(),
        }],
        "B 的新问题".into(),
    )
    .unwrap();
    assert_eq!(output(&collect(second)), "会话 B 的回复");
    assert_eq!(output(&collect(first)), "会话 A 的回复");
    let (_, first_request) = a.request();
    let (_, second_request) = b.request();
    let first_request = first_request["messages"].to_string();
    let second_request = second_request["messages"].to_string();
    assert!(
        first_request.contains("只属于 A 的历史") && !first_request.contains("只属于 B 的历史")
    );
    assert!(second_request.contains("B 的新问题") && !second_request.contains("A 的新问题"));
}

#[test]
fn agent_executes_real_command_then_uses_result_to_reply() {
    let folder = tempfile::tempdir().unwrap();
    let script = if cfg!(windows) {
        "'moin-agent-你好'; Set-Content -LiteralPath result.txt -Value done"
    } else {
        "printf 'moin-agent-你好'; printf done > result.txt"
    };
    let call = sse(
        vec![
            json!({"role":"assistant","tool_calls":[{"index":0,"id":"call_command","type":"function","function":{"name":"execute_command","arguments":json!({"script":script}).to_string()}}]}),
        ],
        "tool_calls",
    );
    let mock = Mock::new(vec![
        (200, "text/event-stream", call),
        (
            200,
            "text/event-stream",
            sse(
                vec![json!({"role":"assistant","content":"命令已完成。"})],
                "stop",
            ),
        ),
    ]);
    let config = AiConfig {
        allow_commands: true,
        command_working_directory: folder.path().to_string_lossy().into_owned(),
        ..compatible(&mock.url)
    };
    let events = collect(
        start_chat(
            config,
            folder.path().join("index.sqlite3"),
            vec![],
            "执行任务".into(),
        )
        .unwrap(),
    );
    assert!(matches!(events.last(), Some(ChatEvent::Done)), "{events:?}");
    assert!(events.iter().any(|event| matches!(event,ChatEvent::CommandFinished(report) if report.success && report.stdout.contains("moin-agent-你好"))));
    assert!(
        fs::read_to_string(folder.path().join("result.txt"))
            .unwrap()
            .contains("done")
    );
    let (_, first) = mock.request();
    assert!(
        first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "execute_command")
    );
    let (_, second) = mock.request();
    assert!(
        second["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool" && m.to_string().contains("moin-agent-你好"))
    );
}

fn collect(session: ChatSession) -> Vec<ChatEvent> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut events = Vec::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(15), session.receiver.recv())
                .await
                .expect("Agent timed out")
                .expect("Agent closed before terminal event");
            let terminal = matches!(event, ChatEvent::Done | ChatEvent::Error(_));
            events.push(event);
            if terminal {
                return events;
            }
        }
    })
}

fn output(events: &[ChatEvent]) -> String {
    events
        .iter()
        .filter_map(|event| {
            if let ChatEvent::Text(text) = event {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn settings_round_trip_and_reject_credentials_in_urls() {
    use crate::config::Config;

    let folder = tempfile::tempdir().unwrap();
    let mut config = Config::load(folder.path()).unwrap();
    let mut ai = config.ai.clone();
    assert!(!ai.use_local_documents);
    assert!(ai.validate().is_err());
    ai.model = "local-installed-model".into();
    ai.system_prompt = "用中文回答".into();
    config.ai = ai.clone();
    config.save(folder.path()).unwrap();
    assert_eq!(Config::load(folder.path()).unwrap().ai, ai);
    ai.model = "second-model".into();
    config.ai = ai.clone();
    config.save(folder.path()).unwrap();
    assert_eq!(Config::load(folder.path()).unwrap().ai, ai);
    for base in [
        "file:///tmp",
        "http://user:secret@localhost",
        "http://localhost?api_key=secret",
        "http://localhost#secret",
        "ftp://example.org",
    ] {
        ai.base_url = base.into();
        assert!(ai.validate().is_err(), "{base}");
    }
    ai.base_url = "http://127.0.0.1:11434/api".into();
    assert!(ai.validate().is_err());
    ai.base_url = "http://127.0.0.1:11434".into();
    ai.api_key_env = "sk-actual-key".into();
    assert!(ai.validate().is_err());
}

#[test]
fn compatible_agent_streams_unicode_with_history_and_without_document_tools() {
    let mock = Mock::new(vec![(
        200,
        "text/event-stream",
        sse(
            vec![
                json!({"role":"assistant","content":"你好"}),
                json!({"content":"，世界！"}),
            ],
            "stop",
        ),
    )]);
    let history = vec![
        ChatMessage {
            role: ChatRole::User,
            content: "记住数字42".into(),
        },
        ChatMessage {
            role: ChatRole::Assistant,
            content: "已记住".into(),
        },
    ];
    let events = collect(
        start_chat(
            compatible(&mock.url),
            PathBuf::from("unused.sqlite3"),
            history,
            "你好".into(),
        )
        .unwrap(),
    );
    assert_eq!(output(&events), "你好，世界！", "{events:?}");
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ChatEvent::Sources(_)))
    );
    let (route, request) = mock.request();
    assert_eq!(route, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(request["model"], "mock-model");
    assert_eq!(request["stream"], true);
    assert!(
        request
            .get("tools")
            .is_none_or(|tools| tools.is_null() || tools.as_array().is_some_and(Vec::is_empty))
    );
    let messages = request["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "user" && message["content"] == "记住数字42")
    );
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "assistant" && message["content"] == "已记住")
    );
}

#[test]
fn ollama_uses_native_chat_and_streams_ndjson() {
    let records = [
        json!({"model":"mock-model","message":{"role":"assistant","content":"本地"},"done":false}),
        json!({"model":"mock-model","message":{"role":"assistant","content":"回答"},"done":false}),
        json!({"model":"mock-model","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","eval_count":2}),
    ];
    let body = records.iter().map(|record| format!("{record}\n")).collect();
    let mock = Mock::new(vec![(200, "application/x-ndjson", body)]);
    let config = AiConfig {
        base_url: mock.url.clone(),
        model: "mock-model".into(),
        ..Default::default()
    };
    let events = collect(
        start_chat(
            config,
            PathBuf::from("unused.sqlite3"),
            vec![],
            "你好".into(),
        )
        .unwrap(),
    );
    assert_eq!(output(&events), "本地回答", "{events:?}");
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
    assert_eq!(mock.request().0, "POST /api/chat HTTP/1.1");
}

#[test]
fn agent_executes_lancedb_retrieval_tool_and_returns_sources() {
    let folder = tempfile::tempdir().unwrap();
    let docs = folder.path().join("docs");
    fs::create_dir(&docs).unwrap();
    let document = docs.join("finch.txt");
    fs::write(
        &document,
        "The finch migration project ships on Friday. Owner is Ada.",
    )
    .unwrap();
    let database = folder.path().join("index.sqlite3");
    crate::search::index::rebuild(&database, &[docs]).unwrap();
    let call = sse(
        vec![
            json!({"role":"assistant","tool_calls":[{"index":0,"id":"call_local","type":"function","function":{"name":"search_local_documents","arguments":"{\"query\":\"finch\"}"}}]}),
        ],
        "tool_calls",
    );
    let answer = sse(
        vec![json!({"role":"assistant","content":"Finch 项目周五发布。"})],
        "stop",
    );
    let mock = Mock::new(vec![
        (200, "text/event-stream", call),
        (200, "text/event-stream", answer),
    ]);
    let config = AiConfig {
        use_local_documents: true,
        ..compatible(&mock.url)
    };
    let events =
        collect(start_chat(config, database, vec![], "查一下 Finch 发布计划".into()).unwrap());
    assert_eq!(output(&events), "Finch 项目周五发布。", "{events:?}");
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
    assert!(events.iter().any(|event| matches!(event,ChatEvent::Sources(sources) if sources.iter().any(|source|source.path==document && source.chunk==1))));
    let (_, first) = mock.request();
    assert_eq!(
        first["tools"][0]["function"]["name"],
        "search_local_documents"
    );
    let (_, second) = mock.request();
    assert!(
        second["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("Owner is Ada")))
    );
}

#[test]
fn cancels_a_stalled_connection_without_waiting_for_the_next_token() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (ready, accepted) = mpsc::channel();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        read_request(&mut socket);
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        ready.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(
            socket.read(&mut byte).unwrap(),
            0,
            "cancel must drop HTTP stream"
        );
    });
    let session = start_chat(
        compatible(&base),
        PathBuf::from("unused.sqlite3"),
        vec![],
        "等待".into(),
    )
    .unwrap();
    accepted.recv_timeout(Duration::from_secs(5)).unwrap();
    let start = Instant::now();
    session.cancel.cancel();
    let events = collect(session);
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(matches!(events.last(), Some(ChatEvent::Done)));
    assert!(output(&events).is_empty());
    worker.join().unwrap();
}

#[test]
fn request_failures_are_terminal_and_prompt_history_are_bounded() {
    let mock = Mock::new(vec![(
        400,
        "application/json",
        json!({"error":{"message":"unknown model","type":"invalid_request_error"}}).to_string(),
    )]);
    let events =
        collect(start_chat(compatible(&mock.url), PathBuf::new(), vec![], "hi".into()).unwrap());
    assert!(
        matches!(events.last(), Some(ChatEvent::Error(_))),
        "{events:?}"
    );
    assert!(output(&events).is_empty());
    mock.request();
    assert!(
        start_chat(
            compatible("http://localhost"),
            PathBuf::new(),
            vec![],
            "a".repeat(MAX_PROMPT_CHARS + 1)
        )
        .is_err()
    );
    let history = (0..100)
        .map(|i| ChatMessage {
            role: if i % 2 == 0 {
                ChatRole::User
            } else {
                ChatRole::Assistant
            },
            content: format!("message {i}"),
        })
        .collect();
    let bounded = bounded_history(history);
    assert_eq!(bounded.len(), MAX_HISTORY_MESSAGES);
    assert!(matches!(bounded.first(), Some(Message::User { .. })));
    let message = safe_error(
        &anyhow::anyhow!("provider echoed secret-key-value"),
        "secret-key-value",
    );
    assert!(!message.contains("secret-key-value"));
    assert!(message.contains("***"));
}
