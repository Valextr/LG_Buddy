use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration as TokioDuration};
use tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};
use rustls::ClientConfig;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::client::danger::{ServerCertVerified, ServerCertVerifier, HandshakeSignatureValid};

use crate::config::HdmiInput;
use crate::tv::{CommandOutput, OledBrightness, TvClient, TvError};

const CONNECTION_TIMEOUT_SECS: u64 = 2;
const TLS_CONNECTION_TIMEOUT_SECS: u64 = 15;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const WS_PORT_PLAIN: u16 = 3000;
const WS_PORT_TLS: u16 = 3001;

// ---- Error types ----

#[derive(Debug)]
pub enum WebOsError {
    ConnectionFailed(String),
    PairingRequired,
    PairingRejected,
    Timeout,
    WebOsError { code: i64, message: String },
}

impl fmt::Display for WebOsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionFailed(msg) => write!(f, "failed to connect to TV: {msg}"),
            Self::PairingRequired => {
                write!(f, "TV pairing required — complete the on-screen prompt and retry")
            }
            Self::PairingRejected => write!(f, "TV pairing rejected by user"),
            Self::Timeout => write!(f, "request timed out after {}s", REQUEST_TIMEOUT_SECS),
            Self::WebOsError { code, message } => {
                write!(f, "webOS error {code}: {message}")
            }
        }
    }
}

impl std::error::Error for WebOsError {}

fn webos_error_to_tv_error(op: &'static str, err: WebOsError) -> TvError {
    match err {
        WebOsError::ConnectionFailed(msg) => TvError::Io {
            command: op,
            source: io::Error::new(io::ErrorKind::ConnectionRefused, msg),
        },
        WebOsError::PairingRequired => TvError::Io {
            command: op,
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "TV pairing required — complete the on-screen prompt and retry",
            ),
        },
        WebOsError::PairingRejected => TvError::Io {
            command: op,
            source: io::Error::new(io::ErrorKind::PermissionDenied, "TV pairing rejected by user"),
        },
        WebOsError::Timeout => TvError::Io {
            command: op,
            source: io::Error::new(
                io::ErrorKind::TimedOut,
                format!("request timed out after {}s", REQUEST_TIMEOUT_SECS),
            ),
        },
        WebOsError::WebOsError { code, message } => TvError::CommandFailed {
            command: op,
            status: Some(code as i32),
            output: CommandOutput::new(
                String::new(),
                format!("errorCode': '{code}': {message}\n"),
            ),
        },
    }
}

// ---- Tokio runtime (singleton for blocking callers) ----

fn get_runtime() -> &'static tokio::runtime::Runtime {
    use std::sync::OnceLock;
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread().worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for webOS client")
    })
}

// ---- Client key persistence ----

fn default_key_file_path() -> PathBuf {
    let config_dir = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        });
    config_dir.join("lg-buddy").join("webos-client-key.json")
}

fn load_client_key(path: &Path) -> Option<String> {
    let s = fs::read_to_string(path).ok()?.trim().to_string();
    if s.is_empty() {
        return None;
    }
    // Try parsing as JSON first ({"client-key": "..."}), fall back to raw string.
    if let Ok(val) = serde_json::from_str::<Value>(&s) {
        if let Some(key) = val.get("client-key").and_then(|k| k.as_str()) {
            return Some(key.to_string());
        }
    }
    // Raw hex key.
    if !s.is_empty() {
        return Some(s);
    }
    None
}

fn save_client_key(path: &Path, key: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        match fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => {
                let _ = serde_json::to_writer(&file, &json!({"client-key": key}));
            }
            Err(_) => {
                // Fallback without mode.
                let _ = fs::write(
                    path,
                    &serde_json::to_string(&json!({"client-key": key}))
                        .unwrap_or_default(),
                );
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = fs::write(
            path,
            &serde_json::to_string(&json!({"client-key": key})).unwrap_or_default(),
        );
    }
}

// ---- Registration handshake constants (from aiowebostv/handshake.py) ----

const REGISTRATION_SIGNATURE: &str = "eyJhbGdvcml0aG0iOiJSU0EtU0hBMjU2Iiwia2V5SWQiOiJ0ZXN0LXNpZ25pbmctY2VydCIsInNpZ25hdHVyZVZlcnNpb24iOjF9.hrVRgjCwXVvE2OOSpDZ58hR+59aFNwYDyjQgKk3auukd7pcegmE2CzPCa0bJ0ZsRAcKkCTJrWo5iDzNhMBWRyaMOv5zWSrthlf7G128qvIlpMT0YNY+n/FaOHE73uLrS/g7swl3/qH/BGFG2Hu4RlL48eb3lLKqTt2xKHdCs6Cd4RMfJPYnzgvI4BNrFUKsjkcu+WD4OO2A27Pq1n50cMchmcaXadJhGrOqH5YmHdOCj5NSHzJYrsW0HPlpuAx/ECMeIZYDh6RMqaFM2DXzdKX9NmmyqzJ3o/0lkk/N97gfVRLW5hA29yeAwaCViZNCP8iC9aO0q9fQojoa7NQnAtw==";

fn registration_payload(client_key: Option<&str>) -> Value {
    json!({
        "type": "register",
        "id": "register_0",
        "payload": {
            "client-key": client_key,
            "pairingType": "PROMPT",
            "manifest": {
                "appVersion": "1.1",
                "manifestVersion": 1,
                "permissions": [
                    "LAUNCH", "CONTROL_AUDIO", "CONTROL_DISPLAY",
                    "CONTROL_POWER", "CONTROL_TV_SCREEN",
                    "READ_APP_STATUS", "READ_RUNNING_APPS",
                    "READ_SETTINGS", "READ_POWER_STATE"
                ],
                "signatures": [{
                    "signature": REGISTRATION_SIGNATURE,
                    "signatureVersion": 1
                }],
                "signed": {
                    "appId": "com.lge.test",
                    "created": "20140509",
                    "localizedAppNames": {"": "LG Remote App"},
                    "localizedVendorNames": {"": "LG Electronics"},
                    "permissions": ["TEST_SECURE", "CONTROL_POWER", "READ_RUNNING_APPS"],
                    "serial": "2f930e2d2cfe083771f68e4fe7bb07",
                    "vendorId": "com.lge"
                }
            }
        }
    })
}

// ---- WebSocket connection helpers ----

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect_ws(
    tv_ip: Ipv4Addr,
    port: u16,
) -> Result<WsStream, io::Error> {
    let uri = format!("ws://{tv_ip}:{port}/");
    // Use IntoClientRequest so tungstenite injects WebSocket handshake headers
    // (Sec-WebSocket-Key, Connection: Upgrade, etc.). Manual http::Request::builder()
    // skips these headers, causing the TV to reject with 'sec-websocket-key' error.
    let mut request = uri.into_client_request().map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidInput, e)
    })?;
    // Origin header is required by some webOS TV firmwares.
    request.headers_mut().insert(
        "Origin",
        format!("http://{tv_ip}:{port}").parse().unwrap(),
    );

    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?;
    Ok(ws)
}

/// Accepts any server certificate — webOS TVs use self-signed certs.
#[derive(Debug)]
struct NoOpCertVerifier;

impl ServerCertVerifier for NoOpCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message, cert, dss,
            &rustls::crypto::CryptoProvider::get_default().unwrap().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message, cert, dss,
            &rustls::crypto::CryptoProvider::get_default().unwrap().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::CryptoProvider::get_default()
            .unwrap()
            .signature_verification_algorithms
            .mapping
            .iter()
            .map(|item| item.0)
            .collect()
    }
}

async fn connect_ws_tls_inner(
    tv_ip: Ipv4Addr,
    port: u16,
    include_origin: bool,
) -> Result<WsStream, io::Error> {
    // webOS TV uses self-signed certs on port 3001.
    // Use rustls with a no-op cert verifier to accept them without Developer Mode.
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(NoOpCertVerifier))
        .with_no_client_auth();

    let uri = format!("wss://{tv_ip}:{port}/");
    // Use IntoClientRequest so tungstenite injects WebSocket handshake headers
    // (Sec-WebSocket-Key, Connection: Upgrade, etc.). Manual http::Request::builder()
    // skips these headers, causing the TV to reject with 'sec-websocket-key' error.
    let mut request = uri.into_client_request().map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidInput, e)
    })?;

    // Some webOS firmwares require an Origin header, but newer models (webOS 8+)
    // reject it with "invalid origin". Try without first, fall back to with Origin.
    if include_origin {
        request.headers_mut().insert(
            "Origin",
            format!("https://{tv_ip}:{port}").parse().unwrap(),
        );
    }

    let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        Some(Connector::Rustls(std::sync::Arc::new(config))),
    )
    .await
    .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?;

    Ok(ws)
}

async fn connect_ws_tls(
    tv_ip: Ipv4Addr,
    port: u16,
) -> Result<WsStream, io::Error> {
    // Try without Origin header first (webOS 8+ rejects it),
    // then fall back to with Origin for older firmwares.
    match connect_ws_tls_inner(tv_ip, port, false).await {
        Ok(ws) => Ok(ws),
        Err(e_no_origin) => match connect_ws_tls_inner(tv_ip, port, true).await {
            Ok(ws) => Ok(ws),
            Err(_) => Err(e_no_origin), // Return the without-Origin error for diagnostics.
        },
    }
}

// ---- WebOsTvClient ----

#[derive(Debug)]
pub struct WebOsTvClient {
    key_file_path: PathBuf,
    client_key: Option<String>,
    command_timeout: std::time::Duration,
    ws_port: Option<u16>,
}

impl WebOsTvClient {
    pub fn new(key_file_path: PathBuf) -> Self {
        let client_key = load_client_key(&key_file_path);
        Self {
            key_file_path,
            client_key,
            command_timeout: std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
            ws_port: None,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(default_key_file_path())
    }

    /// Override the default request timeout (used for pre-sleep commands).
    pub fn with_command_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Override the WebSocket plain-port (test-only).

    pub fn with_ws_port(mut self, port: u16) -> Self {
        self.ws_port = Some(port);
        self
    }

    /// Connect to the TV, complete registration handshake, and return an open WebSocket.
    async fn connect_and_register(
        tv_ip: Ipv4Addr,
        client_key: Option<String>,
        key_file_path: &Path,
        ws_port: Option<u16>,
    ) -> Result<WsStream, WebOsError> {
        let conn_timeout = TokioDuration::from_secs(CONNECTION_TIMEOUT_SECS);
        let plain_port = ws_port.unwrap_or(WS_PORT_PLAIN);

        // Try plain WebSocket first, fallback to TLS port.
        let mut ws = match timeout(conn_timeout, connect_ws(tv_ip, plain_port)).await {
            Ok(Ok(ws)) => ws,
            Ok(Err(_)) => {
                let tls_conn_timeout = TokioDuration::from_secs(TLS_CONNECTION_TIMEOUT_SECS);
                match timeout(tls_conn_timeout, connect_ws_tls(tv_ip, WS_PORT_TLS)).await {
                    Ok(Ok(ws)) => ws,
                    Ok(Err(e)) => {
                        return Err(WebOsError::ConnectionFailed(format!(
                            "plain WS failed, TLS also failed: {e}"
                        )));
                    }
                    Err(_) => {
                        return Err(WebOsError::ConnectionFailed("TLS connection timed out".to_string()));
                    }
                }
            },
            Err(_) => {
                return Err(WebOsError::ConnectionFailed(
                    "plain WS connection timed out".to_string(),
                ));
            }
        };

        // Step 1: Send hello.
        let hello_msg = json!({"id": "hello", "type": "hello", "payload": {}});
        ws.send(Message::Text(hello_msg.to_string())).await.map_err(|e| {
            WebOsError::ConnectionFailed(format!("failed to send hello: {e}"))
        })?;

        let hello_resp = timeout(conn_timeout, ws.next())
            .await
            .map_err(|_| WebOsError::ConnectionFailed("hello response timed out".to_string()))?
            .ok_or_else(|| {
                WebOsError::ConnectionFailed("connection closed before hello".to_string())
            })?;

        match hello_resp {
            Ok(Message::Text(_)) => {} // Hello acknowledged.
            Ok(_) => {
                return Err(WebOsError::ConnectionFailed(
                    "unexpected non-text hello response".to_string(),
                ));
            }
            Err(e) => {
                return Err(WebOsError::ConnectionFailed(format!("hello error: {e}")));
            }
        }

        // Step 2: Get pre-registration system info (required on newer webOS versions).
        let sys_info_req = json!({
            "id": "get_sys_info",
            "type": "request",
            "uri": "ssap://system/getSystemInfo",
            "payload": {}
        });
        ws.send(Message::Text(sys_info_req.to_string())).await.ok(); // Best-effort.
        // Drain any response to the system info request so it does not get
        // consumed as the registration response below.
        let _ = timeout(TokioDuration::from_millis(500), async {
            while let Some(msg) = ws.next().await {
                if let Ok(Message::Text(t)) = &msg {
                    let j: Value = serde_json::from_str(t).unwrap_or(json!({}));
                    let id = j.get("id").and_then(|v| v.as_str());
                    if id == Some("get_sys_info") {
                        break;
                    }
                }
            }
        }).await;

        // Step 3: Register with the TV.
        let reg_msg = registration_payload(client_key.as_deref());
        ws.send(Message::Text(reg_msg.to_string())).await.map_err(|e| {
            WebOsError::ConnectionFailed(format!("failed to send registration: {e}"))
        })?;

        // Wait for registration response.
        let reg_resp = timeout(TokioDuration::from_secs(15), ws.next())
            .await
            .map_err(|_| WebOsError::PairingRequired)?
            .ok_or(WebOsError::PairingRequired)?;

        let reg_text = match reg_resp {
            Ok(Message::Text(t)) => t,
            Ok(_) => {
                return Err(WebOsError::ConnectionFailed(
                    "unexpected non-text registration response".to_string(),
                ));
            }
            Err(e) => {
                return Err(WebOsError::ConnectionFailed(format!(
                    "registration error: {e}"
                )));
            }
        };

        let reg_json: Value = serde_json::from_str(&reg_text).unwrap_or_else(|_| json!({}));

        // Parse registration result.
        if let Some(payload) = reg_json.get("payload") {
            // Check for PROMPT pairing type. TV is showing a pairing prompt.
            if payload.get("pairingType").and_then(|v| v.as_str()) == Some("PROMPT") {
                // Wait for the second response (accept/reject).
                let pair_resp = timeout(TokioDuration::from_secs(60), ws.next())
                    .await
                    .map_err(|_| WebOsError::PairingRequired)?
                    .ok_or(WebOsError::PairingRequired)?;

                match pair_resp {
                    Ok(Message::Text(t)) => {
                        let pair_json: Value =
                            serde_json::from_str(&t).unwrap_or_else(|_| json!({}));
                        if pair_json.get("type").and_then(|v| v.as_str()) == Some("error") {
                            return Err(WebOsError::PairingRejected);
                        }
                        // Extract new client key.
                        if let Some(new_key) = pair_json
                            .get("payload")
                            .and_then(|p| p.get("client-key"))
                            .and_then(|k| k.as_str())
                        {
                            save_client_key(key_file_path, new_key);
                        }
                    }
                    Ok(_) => return Err(WebOsError::PairingRejected),
                    Err(_) => return Err(WebOsError::PairingRequired),
                }
            } else if let Some(return_value) = payload.get("returnValue") {
                if !return_value.as_bool().unwrap_or(false) {
                    return Err(WebOsError::ConnectionFailed(
                        "registration failed".to_string(),
                    ));
                }
            }
        }

        // Drain any leftover messages in the WebSocket buffer after registration.
        // The TV sometimes sends extra messages (notifications or duplicate responses)
        // that would otherwise be misread as the response to the next command.
        // Use a short timeout to avoid blocking, we only want pending messages.
        let _ = timeout(TokioDuration::from_millis(100), async {
            while let Some(msg) = ws.next().await {
                // Log leftover messages for debugging but don't error.
                // These are typically notifications or registration echoes.
                if let Ok(Message::Text(t)) = &msg {
                    let j: Value = serde_json::from_str(t).unwrap_or(json!({}));
                    if let Some(id) = j.get("id").and_then(|v| v.as_str()) {
                        // Skip registration echoes, not command responses.
                        if id.starts_with("register_") || id == "hello" {
                            continue;
                        }
                    }
                }
                break; // First non-registration message might be a real response, stop.
            }
        }).await;

        Ok(ws)
    }

    /// Send an SSAP request over a fresh connection.
    async fn ssap_request(
        &self,
        tv_ip: Ipv4Addr,
        uri: &str,
        payload: Value,
    ) -> Result<Value, WebOsError> {
        // Reload client key from disk before each connection.
        // This picks up keys saved by a previous pairing session,
        // preventing an infinite pairing loop.
        let client_key = load_client_key(&self.key_file_path).or(self.client_key.clone());
        let mut ws = Self::connect_and_register(tv_ip, client_key, &self.key_file_path, self.ws_port).await?;

        let id = uuid::Uuid::new_v4().to_string();
        let request = json!({
            "id": id,
            "type": "request",
            "uri": uri,
            "payload": payload
        });

        ws.send(Message::Text(request.to_string())).await.map_err(|e| {
            WebOsError::ConnectionFailed(format!("failed to send request: {e}"))
        })?;

        let resp = timeout(self.command_timeout, ws.next())
            .await
            .map_err(|_| WebOsError::Timeout)?
            .ok_or(WebOsError::Timeout)?;

        let text = match resp {
            Ok(Message::Text(t)) => t,
            Ok(_) => {
                return Err(WebOsError::WebOsError {
                    code: -1,
                    message: "unexpected non-text response".to_string(),
                });
            }
            Err(e) => {
                return Err(WebOsError::ConnectionFailed(format!("response error: {e}")));
            }
        };

        let json_resp: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));

        // Check for error in response.
        if let Some(payload) = json_resp.get("payload") {
            if let Some(return_value) = payload.get("returnValue") {
                if !return_value.as_bool().unwrap_or(true) {
                    let code = payload
                        .get("errorCode")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(-1);
                    let message = payload
                        .get("errorMessage")
                        .or_else(|| payload.get("errorText"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    return Err(WebOsError::WebOsError { code, message });
                }
            }
        }

        Ok(json_resp)
    }
}

impl TvClient for WebOsTvClient {
    fn get_input(&self, tv_ip: Ipv4Addr) -> Result<String, TvError> {
        let result = get_runtime().block_on(async {
            self.ssap_request(
                tv_ip,
                "ssap://com.webos.applicationManager/getForegroundAppInfo",
                json!({}),
            )
            .await
        });

        match result {
            Ok(resp) => {
                let app_id = resp
                    .get("payload")
                    .and_then(|p| p.get("appId"))
                    .or_else(|| {
                        // Some TV versions return it as `input`.
                        resp.get("payload").and_then(|p| p.get("input"))
                    })
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| TvError::InvalidOutput {
                        command: "get_input",
                        output: CommandOutput::new(resp.to_string(), String::new()),
                        message: "expected appId or input in response payload",
                    })?
                    .to_string();

                Ok(app_id)
            }
            Err(e) => Err(webos_error_to_tv_error("get_input", e)),
        }
    }

    fn get_oled_brightness(&self, tv_ip: Ipv4Addr) -> Result<OledBrightness, TvError> {
        let result = get_runtime().block_on(async {
            self.ssap_request(
                tv_ip,
                "ssap://settings/getSystemSettings",
                json!({
                    "category": "picture",
                    "keys": ["backlight"]
                }),
            )
            .await
        });

        match result {
            Ok(resp) => {
                // Response from settings/getSystemSettings (wrapped in payload):
                // { "id": "...", "type": "response", "payload": { "settings": { "backlight": 72 }, "returnValue": true } }
                let backlight_u64 = resp
                    .get("payload")
                    .and_then(|p| p.get("settings"))
                    .and_then(|s| s.get("backlight"))
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| TvError::InvalidOutput {
                        command: "get_oled_brightness",
                        output: CommandOutput::new(resp.to_string(), String::new()),
                        message: "expected backlight value in settings response",
                    })?;

                let backlight = backlight_u64 as u8;

                OledBrightness::new(backlight).map_err(|_| TvError::InvalidOutput {
                    command: "get_oled_brightness",
                    output: CommandOutput::new(resp.to_string(), String::new()),
                    message: "brightness value out of range 0-100",
                })
            }
            Err(e) => Err(webos_error_to_tv_error("get_oled_brightness", e)),
        }
    }

    fn set_input(&self, tv_ip: Ipv4Addr, input: HdmiInput) -> Result<CommandOutput, TvError> {
        let input_id = input.as_str().to_string();

        let result = get_runtime().block_on(async {
            self.ssap_request(
                tv_ip,
                "ssap://tv/switchInput",
                json!({"inputId": input_id}),
            )
            .await
        });

        match result {
            Ok(_) => Ok(CommandOutput::new(
                "{\"returnValue\": true}\n".to_string(),
                String::new(),
            )),
            Err(e) => Err(webos_error_to_tv_error("set_input", e)),
        }
    }

    fn set_oled_brightness(
        &self,
        tv_ip: Ipv4Addr,
        brightness: OledBrightness,
    ) -> Result<CommandOutput, TvError> {
        // Use the externalpq calibration API (Luna Service) instead of SSAP settings,
        // which is read-only for backlight on OLED panels.
        let value = brightness.as_percent() as u16;
        let data_bytes = value.to_le_bytes();
        let data_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &data_bytes,
        );

        let result = get_runtime().block_on(async {
            self.ssap_request(
                tv_ip,
                "ssap://externalpq/setExternalPqData",
                json!({
                    "command": "BACKLIGHT_UI_DATA",
                    "profileNo": 0,
                    "programID": 1,
                    "data": data_b64,
                    "dataCount": 1,
                    "dataType": "unsigned integer16",
                    "dataOpt": 1,
                }),
            )
            .await
        });

        match result {
            Ok(resp) => Ok(CommandOutput::new(resp.to_string(), String::new())),
            Err(e) => Err(webos_error_to_tv_error("set_oled_brightness", e)),
        }
    }
    fn power_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
        let result = get_runtime().block_on(async {
            self.ssap_request(tv_ip, "ssap://system/turnOff", json!({})).await
        });

        match result {
            Ok(_) => Ok(CommandOutput::new(
                "{\"returnValue\": true}\n".to_string(),
                String::new(),
            )),
            Err(e) => Err(webos_error_to_tv_error("power_off", e)),
        }
    }

    fn turn_screen_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
        let result = get_runtime().block_on(async {
            self.ssap_request(
                tv_ip,
                "ssap://com.webos.service.tvpower/power/turnOffScreen",
                json!({}),
            )
            .await
        });

        match result {
            Ok(_) => Ok(CommandOutput::new(
                "{\"returnValue\": true}\n".to_string(),
                String::new(),
            )),
            Err(e) => Err(webos_error_to_tv_error("turn_screen_off", e)),
        }
    }

    fn turn_screen_on(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
        let result = get_runtime().block_on(async {
            self.ssap_request(
                tv_ip,
                "ssap://com.webos.service.tvpower/power/turnOnScreen",
                json!({}),
            )
            .await
        });

        match result {
            Ok(_) => Ok(CommandOutput::new(
                "{\"returnValue\": true}\n".to_string(),
                String::new(),
            )),
            Err(e) => Err(webos_error_to_tv_error("turn_screen_on", e)),
        }
    }
}
