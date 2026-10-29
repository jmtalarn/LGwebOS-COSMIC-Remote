use std::{fmt, process::Stdio, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use native_tls::TlsConnector as NativeTlsConnector;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpStream, UdpSocket},
    process::Command,
    sync::Mutex,
    time::{Instant, timeout},
};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::Message as WsMessage,
};

const SECRET_SERVICE: &str = "io.github.tvmenubar.LGWebOSRemote";
const REGISTRATION: &str = include_str!("../resources/registration.json");
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type PointerSink = SplitSink<Socket, WsMessage>;

#[derive(Clone, Debug)]
pub struct TvApp {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct TvInput {
    pub id: String,
    pub label: String,
    pub connected: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TvSnapshot {
    pub name: String,
    pub model: String,
    pub volume: Option<u8>,
    pub muted: bool,
    pub apps: Vec<TvApp>,
    pub inputs: Vec<TvInput>,
    pub picture: std::collections::BTreeMap<String, u8>,
    pub apps_error: Option<String>,
    pub inputs_error: Option<String>,
}

pub struct WebOs {
    socket: Mutex<Socket>,
    pointer_socket: Mutex<Option<PointerSink>>,
    host: String,
    fingerprint: String,
}

impl fmt::Debug for WebOs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebOs")
            .field("host", &self.host)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct Probe {
    pub fingerprint: String,
    pub stored_pin: Option<String>,
    pub client_key: Option<String>,
}

fn registration_complete(message: &Value) -> bool {
    message["type"].as_str() == Some("registered")
}

fn registered_client_key(reply: &Value) -> Option<&str> {
    reply
        .pointer("/payload/client-key")
        .or_else(|| reply.get("client-key"))
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
}

fn authorization_guidance(message: &str) -> String {
    format!(
        "{message}. The TV rejected this remote's authorization. Forget the saved pairing in Setup and pair again so the updated app-list, keyboard, and pointer permissions are requested."
    )
}

fn tv_request_error(value: &Value) -> String {
    let code = value
        .get("errorCode")
        .or_else(|| value.pointer("/payload/errorCode"))
        .map(Value::to_string);
    let reason = value
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/payload/errorText").and_then(Value::as_str))
        .unwrap_or("The TV rejected this request.");
    if code.as_deref() == Some("401")
        || reason.contains("401")
        || reason.to_ascii_lowercase().contains("unauthorized")
    {
        return authorization_guidance(&format!("TV authorization denied (401): {reason}"));
    }
    match code {
        Some(code) => format!("TV error {code}: {reason}"),
        None => reason.to_owned(),
    }
}

fn pointer_move_message(dx: i32, dy: i32) -> Option<String> {
    let dx = dx.clamp(-80, 80);
    let dy = dy.clamp(-80, 80);
    (dx != 0 || dy != 0).then(|| format!("type:move\ndx:{dx}\ndy:{dy}\ndown:0\n\n"))
}

fn response_array<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Vec<Value>> {
    names.iter().find_map(|name| {
        value
            .pointer(&format!("/payload/{name}"))
            .or_else(|| value.get(name))
            .and_then(Value::as_array)
    })
}

fn parse_tv_apps(value: &Value) -> Result<Vec<TvApp>, String> {
    let items = response_array(value, &["launchPoints", "apps"])
        .ok_or_else(|| "TV replied, but did not include a launch-point list.".to_owned())?;
    Ok(items
        .iter()
        .filter_map(|app| {
            let id = app.get("id").or_else(|| app.get("appId"))?.as_str()?;
            let title = app.get("title").or_else(|| app.get("appName"))?.as_str()?;
            Some(TvApp {
                id: id.into(),
                title: title.into(),
            })
        })
        .collect())
}

fn parse_tv_inputs(value: &Value) -> Result<Vec<TvInput>, String> {
    let items = response_array(value, &["devices", "inputs"])
        .ok_or_else(|| "TV replied, but did not include an input list.".to_owned())?;
    Ok(items
        .iter()
        .filter_map(|input| {
            let id = input.get("id")?.as_str()?;
            Some(TvInput {
                id: id.into(),
                label: input
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("Input")
                    .into(),
                connected: input
                    .get("connected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect())
}

#[derive(Debug)]
pub enum WebOsError {
    Message(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for WebOsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(message) => formatter.write_str(message),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "Invalid TV response: {error}"),
        }
    }
}

impl From<std::io::Error> for WebOsError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for WebOsError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn valid_local_host(host: &str) -> bool {
    let Ok(ip) = host.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    ip.is_private() || ip.is_link_local()
}

fn ssdp_reply_host(reply: &str, source: std::net::Ipv4Addr) -> Option<String> {
    if !valid_local_host(&source.to_string()) {
        return None;
    }
    let mut service_type = None;
    let mut unique_service = None;
    let mut location = None;
    for line in reply.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "st" => service_type = Some(value.trim()),
            "usn" => unique_service = Some(value.trim()),
            "location" => location = Some(value.trim()),
            _ => {}
        }
    }
    let expected_service = "urn:lge-com:service:webos-second-screen:1";
    let matches_service = service_type
        .is_some_and(|value| value.eq_ignore_ascii_case(expected_service))
        || unique_service
            .is_some_and(|value| value.to_ascii_lowercase().contains(expected_service));
    if !matches_service && (service_type.is_some() || unique_service.is_some()) {
        return None;
    }
    let location = url::Url::parse(location?).ok()?;
    let target_host = location.host_str()?;
    if location.scheme() != "http"
        || location.username() != ""
        || location.password().is_some()
        || !valid_local_host(target_host)
    {
        return None;
    }
    Some(target_host.to_owned())
}

pub async fn discover_tvs() -> Result<Vec<String>, WebOsError> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let query = concat!(
        "M-SEARCH * HTTP/1.1\r\n",
        "HOST: 239.255.255.250:1900\r\n",
        "MAN: \"ssdp:discover\"\r\n",
        "MX: 2\r\n",
        "ST: urn:lge-com:service:webos-second-screen:1\r\n\r\n"
    );
    let target = "239.255.255.250:1900";
    socket.send_to(query.as_bytes(), target).await?;

    let mut found = std::collections::BTreeSet::new();
    let mut buffer = [0_u8; 16_384];
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut next_search = Instant::now() + Duration::from_secs(1);
    loop {
        let wake_at = next_search.min(deadline);
        let wait = wake_at.saturating_duration_since(Instant::now());
        match timeout(wait, socket.recv_from(&mut buffer)).await {
            Ok(Ok((length, sender))) => {
                let std::net::SocketAddr::V4(sender) = sender else {
                    continue;
                };
                let reply = String::from_utf8_lossy(&buffer[..length]);
                if let Some(host) = ssdp_reply_host(&reply, *sender.ip()) {
                    found.insert(host);
                }
            }
            Ok(Err(error)) => return Err(WebOsError::Io(error)),
            Err(_) if Instant::now() >= deadline => break,
            Err(_) => {
                socket.send_to(query.as_bytes(), target).await?;
                next_search += Duration::from_secs(1);
            }
        }
    }
    let hosts: Vec<_> = found.into_iter().collect();
    if hosts.is_empty() {
        return Err(WebOsError::Message(
            "No LG discovery replies arrived. Check that the TV and computer share a non-isolated LAN and that UDP multicast is allowed; manual IP connection is still available.".into(),
        ));
    }
    Ok(hosts)
}

async fn open_socket(host: &str) -> Result<(Socket, String), WebOsError> {
    if !valid_local_host(host) {
        return Err(WebOsError::Message(
            "Enter the TV's private IPv4 address.".into(),
        ));
    }

    open_socket_url(format!("wss://{host}:3001/")).await
}

async fn open_socket_url(url: String) -> Result<(Socket, String), WebOsError> {
    let tls = NativeTlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|error| WebOsError::Message(format!("Could not initialize TLS: {error}")))?;
    let connector = Connector::NativeTls(tls);
    let (socket, _) = timeout(
        Duration::from_secs(8),
        connect_async_tls_with_config(url, None, false, Some(connector)),
    )
    .await
    .map_err(|_| WebOsError::Message("The TV did not respond on port 3001.".into()))?
    .map_err(|error| {
        let detail = error.to_string();
        let message = format!("Could not open encrypted TV connection: {detail}");
        WebOsError::Message(if detail.contains("401") {
            authorization_guidance(&message)
        } else {
            message
        })
    })?;

    let certificate = match socket.get_ref() {
        MaybeTlsStream::NativeTls(stream) => stream
            .get_ref()
            .peer_certificate()
            .map_err(|error| {
                WebOsError::Message(format!("Could not read TV certificate: {error}"))
            })?
            .ok_or_else(|| WebOsError::Message("The TV did not provide a certificate.".into()))?
            .to_der()
            .map_err(|error| {
                WebOsError::Message(format!("Could not read TV certificate: {error}"))
            })?,
        _ => {
            return Err(WebOsError::Message(
                "Unexpected TLS connection type.".into(),
            ));
        }
    };
    let fingerprint = Sha256::digest(certificate)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((socket, fingerprint))
}

pub async fn probe(host: &str) -> Result<Probe, WebOsError> {
    let (mut socket, fingerprint) = open_socket(host).await?;
    let _ = socket.close(None).await;
    let stored_pin = secret_read(host, "certificate").await?;
    if let Some(pin) = &stored_pin
        && pin != &fingerprint
    {
        return Err(WebOsError::Message(
            "The TV certificate changed. Verify the TV before removing its saved pairing.".into(),
        ));
    }
    let client_key = secret_read(host, "client-key").await?;
    Ok(Probe {
        fingerprint,
        stored_pin,
        client_key,
    })
}

pub async fn connect(host: &str, probe: Probe) -> Result<(Arc<WebOs>, TvSnapshot), WebOsError> {
    let (socket, actual_fingerprint) = open_socket(host).await?;
    if actual_fingerprint != probe.fingerprint {
        return Err(WebOsError::Message(
            "The TV certificate changed between review and connection. Nothing was sent.".into(),
        ));
    }
    if probe
        .stored_pin
        .as_ref()
        .is_some_and(|pin| pin != &actual_fingerprint)
    {
        return Err(WebOsError::Message(
            "The TV certificate does not match its saved pin.".into(),
        ));
    }

    let client = Arc::new(WebOs {
        socket: Mutex::new(socket),
        pointer_socket: Mutex::new(None),
        host: host.into(),
        fingerprint: actual_fingerprint.clone(),
    });
    let mut registration: Value = serde_json::from_str(REGISTRATION)?;
    if let Some(key) = &probe.client_key {
        registration["client-key"] = Value::String(key.clone());
    }
    let reply = client
        .exchange(json!({
            "type": "register",
            "id": "register",
            "payload": registration
        }))
        .await?;
    let client_key = registered_client_key(&reply)
        .map(str::to_owned)
        .or(probe.client_key)
        .ok_or_else(|| WebOsError::Message("The TV did not return a pairing key.".into()))?;

    secret_write(host, "certificate", &actual_fingerprint).await?;
    secret_write(host, "client-key", &client_key).await?;

    let snapshot = client.snapshot().await?;
    Ok((client, snapshot))
}

async fn secret_read(host: &str, key: &str) -> Result<Option<String>, WebOsError> {
    let output = Command::new("secret-tool")
        .args([
            "lookup",
            "service",
            SECRET_SERVICE,
            "host",
            host,
            "key",
            key,
        ])
        .output()
        .await
        .map_err(|error| {
            WebOsError::Message(format!(
                "Install libsecret (the secret-tool command) and unlock your keyring: {error}"
            ))
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((!value.is_empty()).then_some(value))
}

async fn secret_write(host: &str, key: &str, value: &str) -> Result<(), WebOsError> {
    let mut child = Command::new("secret-tool")
        .args([
            "store",
            "--label=LG webOS Remote pairing",
            "service",
            SECRET_SERVICE,
            "host",
            host,
            "key",
            key,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            WebOsError::Message(format!("Could not access Secret Service: {error}"))
        })?;
    child
        .stdin
        .take()
        .ok_or_else(|| WebOsError::Message("Could not pass the secret to Secret Service.".into()))?
        .write_all(value.as_bytes())
        .await?;
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        return Err(WebOsError::Message(format!(
            "Secret Service could not save the TV credential: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

pub async fn forget_pairing(host: &str) -> Result<(), WebOsError> {
    for key in ["certificate", "client-key"] {
        let output = Command::new("secret-tool")
            .args(["clear", "service", SECRET_SERVICE, "host", host, "key", key])
            .output()
            .await
            .map_err(|error| {
                WebOsError::Message(format!("Could not access Secret Service: {error}"))
            })?;
        if !output.status.success() {
            return Err(WebOsError::Message(format!(
                "Could not remove the saved {key}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    }
    Ok(())
}

impl WebOs {
    async fn pointer_send(&self, message: String) -> Result<(), WebOsError> {
        let mut pointer = self.pointer_socket.lock().await;
        if pointer.is_none() {
            let result = self
                .request(
                    "com.webos.service.networkinput/getPointerInputSocket",
                    json!({}),
                )
                .await?;
            let path = result
                .pointer("/payload/socketPath")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    WebOsError::Message("TV did not provide a pointer socket.".into())
                })?;
            let url = url::Url::parse(path).map_err(|error| {
                WebOsError::Message(format!("Invalid pointer socket URL: {error}"))
            })?;
            if url.scheme() != "wss"
                || url.host_str() != Some(self.host.as_str())
                || url.port() != Some(3001)
                || url.username() != ""
                || url.password().is_some()
            {
                return Err(WebOsError::Message(
                    "TV returned a pointer socket outside its pinned local connection.".into(),
                ));
            }
            let (mut socket, fingerprint) = open_socket_url(path.to_owned()).await?;
            if fingerprint != self.fingerprint {
                return Err(WebOsError::Message(
                    "Pointer socket certificate does not match the paired TV.".into(),
                ));
            }
            socket
                .send(WsMessage::Ping(Vec::new().into()))
                .await
                .map_err(|error| {
                    WebOsError::Message(format!("Pointer socket ping failed: {error}"))
                })?;
            timeout(Duration::from_secs(3), async {
                loop {
                    match socket.next().await {
                        Some(Ok(WsMessage::Pong(_))) => return Ok(()),
                        Some(Ok(WsMessage::Close(_))) | None => {
                            return Err(WebOsError::Message(
                                "TV closed its pointer socket during setup.".into(),
                            ));
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(error)) => {
                            return Err(WebOsError::Message(format!(
                                "Pointer socket handshake failed: {error}"
                            )));
                        }
                    }
                }
            })
            .await
            .map_err(|_| {
                WebOsError::Message("TV pointer socket did not answer its ping.".into())
            })??;
            let (sink, mut stream) = socket.split();
            tokio::spawn(async move {
                while let Some(frame) = stream.next().await {
                    if frame.is_err() {
                        break;
                    }
                }
            });
            *pointer = Some(sink);
        }
        pointer
            .as_mut()
            .expect("pointer socket initialized")
            .send(WsMessage::Text(message.into()))
            .await
            .map_err(|error| WebOsError::Message(format!("Pointer input failed: {error}")))
    }

    pub async fn pointer_move(&self, dx: i32, dy: i32) -> Result<(), WebOsError> {
        let Some(message) = pointer_move_message(dx, dy) else {
            return Ok(());
        };
        self.pointer_send(message).await
    }

    pub async fn pointer_click(&self) -> Result<(), WebOsError> {
        self.pointer_send("type:click\n\n".into()).await
    }

    async fn exchange(&self, request: Value) -> Result<Value, WebOsError> {
        let id = request["id"]
            .as_str()
            .ok_or_else(|| WebOsError::Message("Invalid request id.".into()))?
            .to_owned();
        let mut socket = self.socket.lock().await;
        socket
            .send(WsMessage::Text(request.to_string().into()))
            .await
            .map_err(|error| WebOsError::Message(format!("Could not send TV request: {error}")))?;

        let response = timeout(Duration::from_secs(90), async {
            loop {
                let message = socket
                    .next()
                    .await
                    .ok_or_else(|| WebOsError::Message("The TV closed the connection.".into()))?
                    .map_err(|error| {
                        WebOsError::Message(format!("TV connection failed: {error}"))
                    })?;
                let WsMessage::Text(text) = message else {
                    continue;
                };
                let value: Value = serde_json::from_str(&text)?;
                if value["id"].as_str() != Some(id.as_str()) {
                    continue;
                }
                if value["type"].as_str() == Some("error")
                    || value.pointer("/payload/returnValue") == Some(&Value::Bool(false))
                {
                    return Err(WebOsError::Message(tv_request_error(&value)));
                }
                if id == "register" && !registration_complete(&value) {
                    continue;
                }
                return Ok(value);
            }
        })
        .await
        .map_err(|_| {
            WebOsError::Message(
                "Timed out waiting for the TV. Accept pairing on the TV if prompted.".into(),
            )
        })??;
        Ok(response)
    }

    pub async fn request(&self, uri: &str, payload: Value) -> Result<Value, WebOsError> {
        self.exchange(json!({
            "type": "request",
            "id": format!("lg-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()),
            "uri": format!("ssap://{uri}"),
            "payload": payload
        }))
        .await
    }

    pub async fn snapshot(&self) -> Result<TvSnapshot, WebOsError> {
        let info = self.request("system/getSystemInfo", json!({})).await?;
        let (apps, apps_error) = match self
            .request("com.webos.applicationManager/listLaunchPoints", json!({}))
            .await
        {
            Ok(value) => match parse_tv_apps(&value) {
                Ok(apps) => (apps, None),
                Err(error) => (Vec::new(), Some(error)),
            },
            Err(error) => (Vec::new(), Some(format!("Could not list TV apps: {error}"))),
        };
        let (inputs, inputs_error) = match self.request("tv/getExternalInputList", json!({})).await
        {
            Ok(value) => match parse_tv_inputs(&value) {
                Ok(inputs) => (inputs, None),
                Err(error) => (Vec::new(), Some(error)),
            },
            Err(error) => (
                Vec::new(),
                Some(format!("Could not list TV inputs: {error}")),
            ),
        };
        let volume = self.request("audio/getVolume", json!({})).await.ok();
        let volume_payload = volume.as_ref().and_then(|value| value.get("payload"));
        let volume_status = volume_payload
            .and_then(|payload| payload.get("volumeStatus"))
            .or(volume_payload);
        let picture = self
            .request(
                "settings/getSystemSettings",
                json!({
                    "category": "picture",
                    "keys": ["backlight", "brightness", "contrast", "color"]
                }),
            )
            .await
            .ok()
            .and_then(|value| {
                value
                    .pointer("/payload/settings")
                    .or_else(|| value.get("settings"))
                    .cloned()
            })
            .and_then(|settings| settings.as_object().cloned())
            .map(|settings| {
                settings
                    .into_iter()
                    .filter_map(|(key, value)| {
                        let number = value
                            .as_u64()
                            .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))?;
                        Some((key, number.min(100) as u8))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(TvSnapshot {
            name: info
                .pointer("/payload/deviceName")
                .and_then(Value::as_str)
                .unwrap_or("LG TV")
                .to_owned(),
            model: info
                .pointer("/payload/modelName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            volume: volume_status
                .and_then(|payload| payload["volume"].as_u64())
                .map(|value| value.min(100) as u8),
            muted: volume_status
                .and_then(|payload| payload["muteStatus"].as_bool())
                .unwrap_or(false),
            apps,
            inputs,
            picture,
            apps_error,
            inputs_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        REGISTRATION, parse_tv_apps, parse_tv_inputs, pointer_move_message, registered_client_key,
        registration_complete, ssdp_reply_host, tv_request_error,
    };
    use serde_json::{Value, json};

    #[test]
    fn registration_acknowledgement_is_not_pairing_completion() {
        let acknowledgement = json!({
            "type": "response",
            "id": "register",
            "payload": { "returnValue": true }
        });
        let registered = json!({
            "type": "registered",
            "id": "register",
            "payload": { "client-key": "test-key" }
        });

        assert!(!registration_complete(&acknowledgement));
        assert!(registration_complete(&registered));
        assert_eq!(registered_client_key(&registered), Some("test-key"));
    }

    #[test]
    fn registration_requests_permissions_for_apps_text_and_pointer() {
        let registration: Value = serde_json::from_str(REGISTRATION).unwrap();
        let permissions = registration
            .pointer("/manifest/permissions")
            .and_then(Value::as_array)
            .unwrap();
        for requested in [
            "READ_INSTALLED_APPS",
            "CONTROL_INPUT_TEXT",
            "CONTROL_MOUSE_AND_KEYBOARD",
        ] {
            assert!(permissions.iter().any(|permission| permission == requested));
        }
    }

    #[test]
    fn authorization_error_preserves_401_and_pairing_guidance() {
        let response = json!({
            "type": "error",
            "errorCode": 401,
            "error": "Unauthorized"
        });
        let message = tv_request_error(&response);
        assert!(message.contains("401"));
        assert!(message.contains("Forget the saved pairing"));
    }

    #[test]
    fn reads_client_key_from_top_level_response_too() {
        let registered = json!({"type": "registered", "client-key": "test-key"});

        assert_eq!(registered_client_key(&registered), Some("test-key"));
    }

    #[test]
    fn accepts_only_local_lg_ssdp_replies_matching_source() {
        let reply = "HTTP/1.1 200 OK\r\nST: urn:lge-com:service:webos-second-screen:1\r\nLOCATION: http://192.168.1.25:1900/description.xml\r\n\r\n";
        assert_eq!(
            ssdp_reply_host(reply, "192.168.1.25".parse().unwrap()).as_deref(),
            Some("192.168.1.25")
        );
        assert_eq!(
            ssdp_reply_host(reply, "192.168.1.26".parse().unwrap()).as_deref(),
            Some("192.168.1.25")
        );
        assert_eq!(
            ssdp_reply_host(
                "HTTP/1.1 200 OK\r\nST: upnp:rootdevice\r\nLOCATION: http://192.168.1.25/desc.xml\r\n",
                "192.168.1.25".parse().unwrap()
            ),
            None
        );
    }

    #[test]
    fn pointer_moves_are_bounded_and_zero_motion_is_skipped() {
        assert_eq!(
            pointer_move_message(250, -140).as_deref(),
            Some("type:move\ndx:80\ndy:-80\ndown:0\n\n")
        );
        assert_eq!(pointer_move_message(0, 0), None);
    }

    #[test]
    fn parses_apps_from_wrapped_or_top_level_launch_point_lists() {
        let wrapped = json!({
            "payload": { "launchPoints": [{ "id": "netflix", "title": "Netflix" }] }
        });
        let direct = json!({
            "launchPoints": [{ "appId": "youtube", "appName": "YouTube" }]
        });
        assert_eq!(parse_tv_apps(&wrapped).unwrap()[0].title, "Netflix");
        assert_eq!(parse_tv_apps(&direct).unwrap()[0].id, "youtube");
        assert!(parse_tv_apps(&json!({"payload": {}})).is_err());
    }

    #[test]
    fn parses_inputs_and_preserves_connected_state() {
        let reply = json!({
            "payload": { "devices": [{ "id": "HDMI_1", "label": "HDMI 1", "connected": true }] }
        });
        let inputs = parse_tv_inputs(&reply).unwrap();
        assert_eq!(inputs[0].id, "HDMI_1");
        assert!(inputs[0].connected);
    }
}
