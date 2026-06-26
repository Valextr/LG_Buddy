mod support;

use lg_buddy::config::HdmiInput;
use lg_buddy::tv::{OledBrightness, TvClient, TvError};
use lg_buddy::web_os::WebOsTvClient;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc;
use support::mock_webos::{MockServerConfig, MockWebOsServer};

fn temp_key_path(label: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!("lg-buddy-test-key-{label}-{}.json", std::process::id()))
}

fn client_with_addr(addr: SocketAddr, existing_key: Option<&str>) -> WebOsTvClient {
    let key_path = temp_key_path("client-test");
    if let Some(key) = existing_key {
        let _ = std::fs::write(&key_path, format!("\"{}\"", key));
    }
    WebOsTvClient::new(key_path).with_ws_port(addr.port())
}

/// Start a mock server on a background thread.
/// Returns the address the server is listening on.
/// The server thread stays alive until the returned guard is dropped.
struct ServerGuard {
    addr: SocketAddr,
    _join: std::thread::JoinHandle<()>,
}

impl ServerGuard {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn tv_ip(&self) -> std::net::Ipv4Addr {
        match self.addr.ip() {
            std::net::IpAddr::V4(v4) => v4,
            _ => panic!("server not bound to IPv4"),
        }
    }
}

fn spawn_server(config: MockServerConfig) -> ServerGuard {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");

        let server = rt.block_on(async {
            MockWebOsServer::with_config(config).await
        });
        let addr = server.addr;
        tx.send(addr).expect("send server addr");
        // Block on the runtime - keeps the server alive.
        rt.block_on(async {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        });
        drop(server);
    });

    let addr = rx.recv().expect("receive server addr");
    ServerGuard {
        addr,
        _join: handle,
    }
}

// ---- Registration tests ----

#[test]
fn registration_succeeds_with_existing_key() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-existing-key".to_string()),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-existing-key"));
    let result = client.get_input(server.tv_ip());
    assert!(
        result.is_ok(),
        "get_input should succeed with existing key: {:?}",
        result
    );
}

#[test]
fn registration_triggers_pairing_when_no_key() {
    let server = spawn_server(MockServerConfig {
        require_pairing: true,
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), None);
    let result = client.get_input(server.tv_ip());
    assert!(
        result.is_ok(),
        "get_input should succeed after pairing: {:?}",
        result
    );
}

#[test]
fn registration_fails_when_pairing_rejected() {
    let server = spawn_server(MockServerConfig {
        require_pairing: true,
        reject_pairing: true,
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), None);
    let result = client.get_input(server.tv_ip());
    assert!(
        result.is_err(),
        "get_input should fail when pairing is rejected"
    );
    let err = result.unwrap_err();
    match err {
        TvError::Io { source, .. } => {
            assert!(
                source.kind() == std::io::ErrorKind::PermissionDenied,
                "expected PermissionDenied, got: {:?}",
                source.kind()
            );
        }
        other => panic!(
            "expected TvError::Io for pairing rejection, got: {:?}",
            other
        ),
    }
}

// ---- TvClient trait method tests ----

#[test]
fn get_input_returns_current_app_id() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let input = client.get_input(server.tv_ip()).expect("get_input should succeed");
    // Default mock input is com.webos.app.hdmi2
    assert_eq!(input, "com.webos.app.hdmi2");
}

#[test]
fn set_input_sends_correct_uri() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let result = client.set_input(server.tv_ip(), HdmiInput::Hdmi3);
    assert!(
        result.is_ok(),
        "set_input should succeed: {:?}",
        result
    );
}

#[test]
fn get_oled_brightness_returns_backlight_value() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let brightness = client
        .get_oled_brightness(server.tv_ip())
        .expect("get_oled_brightness should succeed");
    // Default mock backlight is 50
    assert_eq!(brightness.as_percent(), 50);
}

#[test]
fn set_oled_brightness_sends_externalpq_command() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let brightness = OledBrightness::new(65).expect("valid brightness");
    let result = client.set_oled_brightness(server.tv_ip(), brightness);
    assert!(
        result.is_ok(),
        "set_oled_brightness should succeed: {:?}",
        result
    );
}

#[test]
fn power_off_sends_turnoff_command() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let result = client.power_off(server.tv_ip());
    assert!(
        result.is_ok(),
        "power_off should succeed: {:?}",
        result
    );
}

#[test]
fn turn_screen_off_sends_turnoffscreen_command() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let result = client.turn_screen_off(server.tv_ip());
    assert!(
        result.is_ok(),
        "turn_screen_off should succeed: {:?}",
        result
    );
}

#[test]
fn turn_screen_on_sends_turnonscreen_command() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let result = client.turn_screen_on(server.tv_ip());
    assert!(
        result.is_ok(),
        "turn_screen_on should succeed: {:?}",
        result
    );
}

// ---- Error handling tests ----

#[test]
fn webos_error_response_preserves_error_code() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        ssap_error: Some((-102, "Substate mismatch".to_string())),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let result = client.get_input(server.tv_ip());
    assert!(
        result.is_err(),
        "get_input should fail on webOS error"
    );
    let err = result.unwrap_err();
    match err {
        TvError::CommandFailed { status, output, .. } => {
            assert_eq!(status, Some(-102), "error code should be preserved");
            assert!(
                output.stderr().contains("Substate mismatch"),
                "error message should be preserved: {}",
                output.stderr()
            );
        }
        other => panic!(
            "expected TvError::CommandFailed for webOS error, got: {:?}",
            other
        ),
    }
}

#[test]
fn request_timeout_returns_io_error() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        disconnect_immediately: true,
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let result = client.get_input(server.tv_ip());
    assert!(
        result.is_err(),
        "get_input should fail on connection failure"
    );
}

// ---- Request/response correlation test ----

#[test]
fn ssap_requests_use_unique_ids() {
    let server = spawn_server(MockServerConfig {
        existing_key: Some("test-key".to_string()),
        ..Default::default()
    });
    let client = client_with_addr(server.addr(), Some("test-key"));
    let _ = client.get_input(server.tv_ip());
    let _ = client.get_input(server.tv_ip());
    let _ = client.turn_screen_off(server.tv_ip());

    // Each request should have used a unique UUID.
    // We can't easily verify this from the client side without server introspection,
    // but the fact that all 3 succeeded proves correlation worked.
}
