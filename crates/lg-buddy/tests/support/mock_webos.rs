// Mock webOS WebSocket server for testing WebOsTvClient.
// Simulates an LG webOS TV's WebSocket API.

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::protocol::Message;

static SERVER_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct MockTvState {
    pub power_on: bool,
    pub screen_on: bool,
    pub current_input: String,
    pub backlight: u8,
}

impl Default for MockTvState {
    fn default() -> Self {
        Self {
            power_on: true,
            screen_on: true,
            current_input: "com.webos.app.hdmi2".to_string(),
            backlight: 50,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MockServerConfig {
    pub existing_key: Option<String>,
    pub require_pairing: bool,
    pub reject_pairing: bool,
    pub ssap_error: Option<(i64, String)>,
    pub disconnect_immediately: bool,
}

#[derive(Debug, Default)]
pub struct ServerState {
    pub registration_key: Option<String>,
    pub received_messages: Vec<Value>,
}

pub struct MockWebOsServer {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<ServerState>>,
    pub tv_state: Arc<Mutex<MockTvState>>,
    pub config: Arc<Mutex<MockServerConfig>>,
    _handle: Option<tokio::task::JoinHandle<()>>,
}

impl MockWebOsServer {
    pub async fn new() -> Self {
        Self::with_config(MockServerConfig::default()).await
    }

    pub async fn with_config(config: MockServerConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind to random port");
        let addr = listener.local_addr().expect("local_addr");
        let server_id = SERVER_ID.fetch_add(1, Ordering::Relaxed);

        let state = Arc::new(Mutex::new(ServerState::default()));
        let tv_state = Arc::new(Mutex::new(MockTvState::default()));
        let config = Arc::new(Mutex::new(config));

        let handle = tokio::spawn(Self::run(
            listener,
            state.clone(),
            tv_state.clone(),
            config.clone(),
            server_id,
        ));

        Self {
            addr,
            state,
            tv_state,
            config,
            _handle: Some(handle),
        }
    }

    async fn run(
        listener: TcpListener,
        state: Arc<Mutex<ServerState>>,
        tv_state: Arc<Mutex<MockTvState>>,
        config: Arc<Mutex<MockServerConfig>>,
        server_id: u64,
    ) {
        let mut listener = listener;
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(ok) => ok,
                Err(_) => break,
            };
            let state = Arc::clone(&state);
            let tv_state = Arc::clone(&tv_state);
            let config = Arc::clone(&config);
            tokio::spawn(async move {
                if let Err(e) =
                    Self::handle_client(stream, peer, state, tv_state, config, server_id).await
                {
                    eprintln!("[mock-webos-{server_id}] error from {peer}: {e}");
                }
            });
        }
    }

    async fn handle_client(
        stream: tokio::net::TcpStream,
        _peer: SocketAddr,
        state: Arc<Mutex<ServerState>>,
        tv_state: Arc<Mutex<MockTvState>>,
        config: Arc<Mutex<MockServerConfig>>,
        server_id: u64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ws = tokio_tungstenite::accept_async(stream).await?;
        let (mut outgoing, mut incoming) = ws.split();

        // Read config once at connection time.
        let disconnect_immediately = {
            let c = config.lock().unwrap();
            c.disconnect_immediately
        };

        if disconnect_immediately {
            drop(outgoing);
            drop(incoming);
            return Ok(());
        }

        while let Some(result) = incoming.next().await {
            let msg = result?;
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => break,
                _ => continue,
            };

            let json: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
            let msg_id = json
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let msg_type = json
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let uri = json
                .get("uri")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let payload = json.get("payload").cloned().unwrap_or(json!({}));

            {
                let mut s = state.lock().unwrap();
                s.received_messages.push(json.clone());
            }

            // --- Hello ---
            if msg_type == "hello" {
                let resp = json!({
                    "id": "hello",
                    "type": "hello",
                    "payload": { "version": "1.0" }
                });
                outgoing.send(Message::Text(resp.to_string())).await?;
                continue;
            }

            // --- Registration ---
            if msg_type == "register" {
                let (require_pairing, reject_pairing, existing_key) = {
                    let c = config.lock().unwrap();
                    (c.require_pairing, c.reject_pairing, c.existing_key.is_some())
                };

                if require_pairing {
                    let prompt = json!({
                        "id": "register_0",
                        "type": "response",
                        "payload": {
                            "pairingType": "PROMPT",
                            "deviceName": "OLED55CX3PU"
                        }
                    });
                    outgoing.send(Message::Text(prompt.to_string())).await?;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                    if reject_pairing {
                        let err = json!({
                            "id": "register_0",
                            "type": "error",
                            "payload": {
                                "errorCode": -100,
                                "errorMessage": "Pairing rejected by user"
                            }
                        });
                        outgoing.send(Message::Text(err.to_string())).await?;
                    } else {
                        let new_key = format!("mock-pairing-key-{:016x}", server_id);
                        let accept = json!({
                            "id": "register_0",
                            "type": "response",
                            "payload": {
                                "returnValue": true,
                                "client-key": new_key,
                                "pairingType": "PROMPT",
                                "deviceName": "OLED55CX3PU"
                            }
                        });
                        outgoing.send(Message::Text(accept.to_string())).await?;
                        {
                            let mut s = state.lock().unwrap();
                            s.registration_key = Some(new_key);
                        }
                    }
                } else if existing_key {
                    let resp = json!({
                        "id": "register_0",
                        "type": "response",
                        "payload": {
                            "returnValue": true,
                            "deviceName": "OLED55CX3PU"
                        }
                    });
                    outgoing.send(Message::Text(resp.to_string())).await?;
                } else {
                    let resp = json!({
                        "id": "register_0",
                        "type": "response",
                        "payload": {
                            "returnValue": false,
                            "errorCode": -2,
                            "errorMessage": "No valid client key"
                        }
                    });
                    outgoing.send(Message::Text(resp.to_string())).await?;
                }
                continue;
            }

            // --- SSAP requests ---
            if msg_type == "request" && uri.starts_with("ssap://") {
                let ssap_err = {
                    let c = config.lock().unwrap();
                    c.ssap_error.clone()
                };
                if let Some((code, message)) = ssap_err {
                    let resp = json!({
                        "id": msg_id,
                        "type": "response",
                        "uri": uri,
                        "payload": {
                            "returnValue": false,
                            "errorCode": code,
                            "errorMessage": message
                        }
                    });
                    outgoing.send(Message::Text(resp.to_string())).await?;
                    continue;
                }

                // Read TV state and build response.
                // MutexGuard must be dropped before .await.
                let resp: Value;
                {
                    let tv = tv_state.lock().unwrap();
                    if uri.contains("getForegroundAppInfo") {
                        resp = json!({
                            "id": msg_id, "type": "response", "uri": uri,
                            "payload": { "returnValue": true, "appId": tv.current_input.clone() }
                        });
                    } else if uri.contains("getSystemSettings") {
                        resp = json!({
                            "id": msg_id, "type": "response", "uri": uri,
                            "payload": { "returnValue": true, "settings": { "backlight": tv.backlight } }
                        });
                    } else {
                        // All other SSAP commands succeed by default.
                        resp = json!({ "id": msg_id, "type": "response", "uri": uri, "payload": { "returnValue": true } });
                    }
                } // tv guard dropped

                outgoing.send(Message::Text(resp.to_string())).await?;
                continue;
            }
        }
        Ok(())
    }

    pub fn tv_ip(&self) -> std::net::Ipv4Addr {
        match self.addr.ip() {
            std::net::IpAddr::V4(v4) => v4,
            _ => panic!("server not bound to IPv4"),
        }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn set_power_on(&self, value: bool) {
        self.tv_state.lock().unwrap().power_on = value;
    }

    pub fn set_screen_on(&self, value: bool) {
        self.tv_state.lock().unwrap().screen_on = value;
    }

    pub fn set_input(&self, value: &str) {
        self.tv_state.lock().unwrap().current_input = value.to_string();
    }

    pub fn set_backlight(&self, value: u8) {
        self.tv_state.lock().unwrap().backlight = value;
    }

    pub fn registration_key(&self) -> Option<String> {
        self.state.lock().unwrap().registration_key.clone()
    }

    pub fn received_messages(&self) -> Vec<Value> {
        self.state.lock().unwrap().received_messages.clone()
    }
}

impl Drop for MockWebOsServer {
    fn drop(&mut self) {
        if let Some(handle) = self._handle.take() {
            handle.abort();
        }
    }
}
