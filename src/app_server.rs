//! Private WebSocket-over-Unix transport for one Run worker.

use crate::jcs::{canonicalize, parse};
use base64::Engine as _;
use serde_json::Value;
use sha1::{Digest as _, Sha1};
use sha2::Sha256;
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

pub const MAX_HTTP_UPGRADE_BYTES: usize = 16 * 1024;
pub const MAX_WEBSOCKET_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RESPONSE_ID_PREFIX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CORRELATED_MESSAGES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
    Io(String),
    UpgradeRejected,
    InvalidFrame(&'static str),
    MessageTooLarge,
    InvalidUtf8,
    InvalidJson,
    CorrelationMismatch,
    RemoteError { code: i64, message: String },
    Closed,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(reason) => write!(formatter, "transport I/O failed: {reason}"),
            Self::UpgradeRejected => formatter.write_str("WebSocket upgrade was rejected"),
            Self::InvalidFrame(reason) => write!(formatter, "invalid WebSocket frame: {reason}"),
            Self::MessageTooLarge => formatter.write_str("WebSocket message exceeds its bound"),
            Self::InvalidUtf8 => formatter.write_str("WebSocket text is not UTF-8"),
            Self::InvalidJson => formatter.write_str("app-server message is not valid JSON"),
            Self::CorrelationMismatch => formatter.write_str("JSON-RPC correlation failed closed"),
            Self::RemoteError { code, message } => {
                write!(formatter, "app-server error {code}: {message}")
            }
            Self::Closed => formatter.write_str("app-server closed the WebSocket"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<std::io::Error> for TransportError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

pub trait Wire: Read + Write {}
impl<T: Read + Write> Wire for T {}

#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Text(Vec<u8>),
    Close { code: Option<u16> },
}

pub struct WebSocket<T> {
    wire: T,
    fragmented_opcode: Option<u8>,
    fragmented: Vec<u8>,
    closed: bool,
}

impl<T: Wire> WebSocket<T> {
    #[must_use]
    pub fn new(wire: T) -> Self {
        Self {
            wire,
            fragmented_opcode: None,
            fragmented: Vec::new(),
            closed: false,
        }
    }

    #[must_use]
    pub fn into_inner(self) -> T {
        self.wire
    }

    pub fn send_text(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        std::str::from_utf8(payload).map_err(|_| TransportError::InvalidUtf8)?;
        self.send_frame(true, 0x1, payload)
    }

    pub fn send_close(&mut self, code: u16) -> Result<(), TransportError> {
        if self.closed {
            return Ok(());
        }
        self.send_frame(true, 0x8, &code.to_be_bytes())?;
        self.closed = true;
        Ok(())
    }

    pub fn receive(&mut self) -> Result<Incoming, TransportError> {
        loop {
            let (final_frame, opcode, payload) = self.read_frame()?;
            match opcode {
                0x0 => {
                    if self.fragmented_opcode.is_none() {
                        return Err(TransportError::InvalidFrame("unexpected continuation"));
                    }
                    self.extend_fragment(&payload)?;
                    if final_frame {
                        return self.finish_fragment();
                    }
                }
                0x1 => {
                    if self.fragmented_opcode.is_some() {
                        return Err(TransportError::InvalidFrame("nested data frame"));
                    }
                    if final_frame {
                        std::str::from_utf8(&payload).map_err(|_| TransportError::InvalidUtf8)?;
                        return Ok(Incoming::Text(payload));
                    }
                    self.fragmented_opcode = Some(opcode);
                    self.extend_fragment(&payload)?;
                }
                0x8 => {
                    let code = match payload.len() {
                        0 => None,
                        2.. => Some(u16::from_be_bytes([payload[0], payload[1]])),
                        _ => return Err(TransportError::InvalidFrame("invalid close payload")),
                    };
                    if !self.closed {
                        self.send_frame(true, 0x8, &payload)?;
                    }
                    self.closed = true;
                    return Ok(Incoming::Close { code });
                }
                0x9 => self.send_frame(true, 0xA, &payload)?,
                0xA => {}
                _ => return Err(TransportError::InvalidFrame("unsupported opcode")),
            }
        }
    }

    fn finish_fragment(&mut self) -> Result<Incoming, TransportError> {
        let opcode = self
            .fragmented_opcode
            .take()
            .ok_or(TransportError::InvalidFrame("missing fragmented opcode"))?;
        let payload = std::mem::take(&mut self.fragmented);
        if opcode != 0x1 {
            return Err(TransportError::InvalidFrame(
                "binary messages are unsupported",
            ));
        }
        std::str::from_utf8(&payload).map_err(|_| TransportError::InvalidUtf8)?;
        Ok(Incoming::Text(payload))
    }

    fn extend_fragment(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        if self.fragmented.len().saturating_add(payload.len()) > MAX_WEBSOCKET_MESSAGE_BYTES {
            return Err(TransportError::MessageTooLarge);
        }
        self.fragmented.extend_from_slice(payload);
        Ok(())
    }

    fn read_frame(&mut self) -> Result<(bool, u8, Vec<u8>), TransportError> {
        let mut prefix = [0_u8; 2];
        self.wire.read_exact(&mut prefix)?;
        if prefix[0] & 0x70 != 0 {
            return Err(TransportError::InvalidFrame("reserved bits are set"));
        }
        if prefix[1] & 0x80 != 0 {
            return Err(TransportError::InvalidFrame("server frame is masked"));
        }
        let final_frame = prefix[0] & 0x80 != 0;
        let opcode = prefix[0] & 0x0f;
        let control = opcode & 0x08 != 0;
        if control && !final_frame {
            return Err(TransportError::InvalidFrame("fragmented control frame"));
        }
        let marker = prefix[1] & 0x7f;
        let length = match marker {
            0..=125 => u64::from(marker),
            126 => {
                let mut bytes = [0_u8; 2];
                self.wire.read_exact(&mut bytes)?;
                let value = u64::from(u16::from_be_bytes(bytes));
                if value < 126 {
                    return Err(TransportError::InvalidFrame("non-minimal length"));
                }
                value
            }
            127 => {
                let mut bytes = [0_u8; 8];
                self.wire.read_exact(&mut bytes)?;
                let value = u64::from_be_bytes(bytes);
                if value < 65_536 || value >> 63 != 0 {
                    return Err(TransportError::InvalidFrame("invalid 64-bit length"));
                }
                value
            }
            _ => unreachable!(),
        };
        let length = usize::try_from(length).map_err(|_| TransportError::MessageTooLarge)?;
        if length > MAX_WEBSOCKET_FRAME_BYTES || (control && length > 125) {
            return Err(TransportError::MessageTooLarge);
        }
        let mut payload = vec![0; length];
        self.wire.read_exact(&mut payload)?;
        Ok((final_frame, opcode, payload))
    }

    fn send_frame(
        &mut self,
        final_frame: bool,
        opcode: u8,
        payload: &[u8],
    ) -> Result<(), TransportError> {
        if payload.len() > MAX_WEBSOCKET_FRAME_BYTES || (opcode & 0x08 != 0 && payload.len() > 125)
        {
            return Err(TransportError::MessageTooLarge);
        }
        let mut header = vec![(if final_frame { 0x80 } else { 0 }) | opcode];
        match payload.len() {
            length @ 0..=125 => header.push(0x80 | u8::try_from(length).expect("bounded")),
            length @ 126..=65_535 => {
                header.push(0x80 | 126);
                header.extend(u16::try_from(length).expect("bounded").to_be_bytes());
            }
            length => {
                header.push(0x80 | 127);
                header.extend(u64::try_from(length).expect("bounded").to_be_bytes());
            }
        }
        let digest = Sha256::digest(Uuid::now_v7().as_bytes());
        let mask = [digest[0], digest[1], digest[2], digest[3]];
        header.extend(mask);
        self.wire.write_all(&header)?;
        for (index, chunk) in payload.chunks(16 * 1024).enumerate() {
            let offset = index * 16 * 1024;
            let masked = chunk
                .iter()
                .enumerate()
                .map(|(inner, byte)| byte ^ mask[(offset + inner) % 4])
                .collect::<Vec<_>>();
            self.wire.write_all(&masked)?;
        }
        self.wire.flush()?;
        Ok(())
    }
}

pub struct JsonRpcConnection<T> {
    websocket: WebSocket<T>,
    next_request_id: u64,
    pending: BTreeSet<u64>,
    notifications: VecDeque<Value>,
}

impl JsonRpcConnection<UnixStream> {
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, TransportError> {
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let raw_key = Uuid::now_v7();
        let key = base64::engine::general_purpose::STANDARD.encode(raw_key.as_bytes());
        write!(
            stream,
            "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        stream.flush()?;
        let response = read_upgrade(&mut stream)?;
        validate_upgrade(&response, &key)?;
        Ok(Self::new(stream))
    }
}

impl<T: Wire> JsonRpcConnection<T> {
    #[must_use]
    pub fn new(wire: T) -> Self {
        Self {
            websocket: WebSocket::new(wire),
            next_request_id: 1,
            pending: BTreeSet::new(),
            notifications: VecDeque::new(),
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), TransportError> {
        validate_method(method)?;
        self.send_json(&serde_json::json!({"method": method, "params": params}))
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, TransportError> {
        validate_method(method)?;
        let id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(TransportError::CorrelationMismatch)?;
        if !self.pending.insert(id) {
            return Err(TransportError::CorrelationMismatch);
        }
        self.send_json(&serde_json::json!({"id": id, "method": method, "params": params}))?;
        for _ in 0..MAX_CORRELATED_MESSAGES {
            let value = self.read_json()?;
            if value.get("method").and_then(Value::as_str).is_some() {
                if self.notifications.len() >= MAX_CORRELATED_MESSAGES {
                    return Err(TransportError::CorrelationMismatch);
                }
                self.notifications.push_back(value);
                continue;
            }
            if let Some(response_id) = value.get("id").and_then(Value::as_u64) {
                if response_id != id || !self.pending.remove(&response_id) {
                    return Err(TransportError::CorrelationMismatch);
                }
                if let Some(error) = value.get("error") {
                    return Err(TransportError::RemoteError {
                        code: error
                            .get("code")
                            .and_then(Value::as_i64)
                            .unwrap_or(i64::MIN),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unstructured app-server error")
                            .to_owned(),
                    });
                }
                return value
                    .get("result")
                    .cloned()
                    .ok_or(TransportError::CorrelationMismatch);
            }
            return Err(TransportError::CorrelationMismatch);
        }
        Err(TransportError::CorrelationMismatch)
    }

    pub fn next_message(&mut self) -> Result<Value, TransportError> {
        if let Some(value) = self.notifications.pop_front() {
            return Ok(value);
        }
        let value = self.read_json()?;
        if value.get("method").and_then(Value::as_str).is_none() {
            return Err(TransportError::CorrelationMismatch);
        }
        Ok(value)
    }

    pub fn respond_result(&mut self, id: u64, result: Value) -> Result<(), TransportError> {
        self.send_json(&serde_json::json!({"id": id, "result": result}))
    }

    pub fn respond_error(
        &mut self,
        id: u64,
        code: i64,
        message: &str,
    ) -> Result<(), TransportError> {
        self.send_json(&serde_json::json!({"id": id, "error": {"code": code, "message": message}}))
    }

    pub fn close(&mut self) -> Result<(), TransportError> {
        self.websocket.send_close(1000)
    }

    fn send_json(&mut self, value: &Value) -> Result<(), TransportError> {
        let bytes = serde_json::to_vec(value).map_err(|_| TransportError::InvalidJson)?;
        self.websocket.send_text(&bytes)
    }

    fn read_json(&mut self) -> Result<Value, TransportError> {
        match self.websocket.receive()? {
            Incoming::Text(bytes) => {
                let text = std::str::from_utf8(&bytes).map_err(|_| TransportError::InvalidUtf8)?;
                let parsed = parse(text).map_err(|_| TransportError::InvalidJson)?;
                let canonical = canonicalize(&parsed).map_err(|_| TransportError::InvalidJson)?;
                let value: Value =
                    serde_json::from_slice(&canonical).map_err(|_| TransportError::InvalidJson)?;
                if value.get("id").is_some()
                    && bytes.len() > MAX_RESPONSE_ID_PREFIX_BYTES
                    && !bytes[..MAX_RESPONSE_ID_PREFIX_BYTES]
                        .windows(4)
                        .any(|candidate| candidate == b"\"id\"")
                {
                    return Err(TransportError::CorrelationMismatch);
                }
                Ok(value)
            }
            Incoming::Close { .. } => Err(TransportError::Closed),
        }
    }
}

fn validate_method(method: &str) -> Result<(), TransportError> {
    if method.is_empty() || method.len() > 256 {
        Err(TransportError::InvalidJson)
    } else {
        Ok(())
    }
}

fn read_upgrade(stream: &mut UnixStream) -> Result<Vec<u8>, TransportError> {
    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    while response.len() < MAX_HTTP_UPGRADE_BYTES {
        stream.read_exact(&mut byte)?;
        response.push(byte[0]);
        if response.ends_with(b"\r\n\r\n") {
            return Ok(response);
        }
    }
    Err(TransportError::UpgradeRejected)
}

fn validate_upgrade(response: &[u8], key: &str) -> Result<(), TransportError> {
    let response = std::str::from_utf8(response).map_err(|_| TransportError::UpgradeRejected)?;
    let mut lines = response.split("\r\n");
    if lines.next() != Some("HTTP/1.1 101 Switching Protocols") {
        return Err(TransportError::UpgradeRejected);
    }
    let mut upgrade = false;
    let mut connection = false;
    let mut accept = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "upgrade" => upgrade = value.eq_ignore_ascii_case("websocket"),
            "connection" => {
                connection = value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            }
            "sec-websocket-accept" => accept = Some(value),
            _ => {}
        }
    }
    let digest = Sha1::digest(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
    let expected = base64::engine::general_purpose::STANDARD.encode(digest);
    if upgrade && connection && accept == Some(expected.as_str()) {
        Ok(())
    } else {
        Err(TransportError::UpgradeRejected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Cursor, Result as IoResult};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;

    #[derive(Default)]
    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Duplex {
        fn with_input(input: Vec<u8>) -> Self {
            Self {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }
    }
    impl Read for Duplex {
        fn read(&mut self, buffer: &mut [u8]) -> IoResult<usize> {
            self.input.read(buffer)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, buffer: &[u8]) -> IoResult<usize> {
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }
        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    fn frame(final_frame: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() <= 125);
        let mut bytes = vec![
            (if final_frame { 0x80 } else { 0 }) | opcode,
            payload.len() as u8,
        ];
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn fragmentation_and_ping_are_handled_without_losing_text() {
        let mut input = frame(false, 0x1, b"{\"a\":");
        input.extend(frame(true, 0x9, b"p"));
        input.extend(frame(true, 0x0, b"1}"));
        let mut websocket = WebSocket::new(Duplex::with_input(input));
        assert_eq!(
            websocket.receive().unwrap(),
            Incoming::Text(b"{\"a\":1}".to_vec())
        );
        let output = websocket.into_inner().output;
        assert_eq!(output[0] & 0x0f, 0xA);
        assert_ne!(output[1] & 0x80, 0);
    }

    #[test]
    fn masked_server_and_nonminimal_lengths_fail_closed() {
        let mut masked = WebSocket::new(Duplex::with_input(vec![0x81, 0x80, 0, 0, 0, 0]));
        assert_eq!(
            masked.receive().unwrap_err(),
            TransportError::InvalidFrame("server frame is masked")
        );
        let mut nonminimal = WebSocket::new(Duplex::with_input(vec![0x81, 126, 0, 1, b'x']));
        assert_eq!(
            nonminimal.receive().unwrap_err(),
            TransportError::InvalidFrame("non-minimal length")
        );
    }

    #[test]
    fn json_rpc_queues_notifications_and_rejects_wrong_response_ids() {
        let notification =
            serde_json::to_vec(&serde_json::json!({"method":"turn/started","params":{}})).unwrap();
        let response =
            serde_json::to_vec(&serde_json::json!({"id":1,"result":{"ok":true}})).unwrap();
        let mut input = frame(true, 0x1, &notification);
        input.extend(frame(true, 0x1, &response));
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(input));
        assert_eq!(
            rpc.request("initialize", serde_json::json!({})).unwrap(),
            serde_json::json!({"ok":true})
        );
        assert_eq!(rpc.next_message().unwrap()["method"], "turn/started");

        let wrong = serde_json::to_vec(&serde_json::json!({"id":9,"result":{}})).unwrap();
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(frame(true, 0x1, &wrong)));
        assert_eq!(
            rpc.request("x", serde_json::json!({})).unwrap_err(),
            TransportError::CorrelationMismatch
        );
    }

    #[test]
    fn server_requests_take_precedence_and_duplicate_members_are_rejected() {
        let server_request = br#"{"id":1,"method":"item/tool/requestUserInput","params":{}}"#;
        let response = br#"{"id":1,"result":{"ok":true}}"#;
        let mut input = frame(true, 0x1, server_request);
        input.extend(frame(true, 0x1, response));
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(input));
        assert_eq!(
            rpc.request("initialize", serde_json::json!({})).unwrap(),
            serde_json::json!({"ok":true})
        );
        assert_eq!(
            rpc.next_message().unwrap()["method"],
            "item/tool/requestUserInput"
        );

        let duplicate = br#"{"method":"turn/started","params":{},"params":{"bad":true}}"#;
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(frame(true, 0x1, duplicate)));
        assert_eq!(rpc.next_message().unwrap_err(), TransportError::InvalidJson);
    }

    #[test]
    fn unix_upgrade_validates_accept_and_client_frames_are_masked() {
        let root = PathBuf::from("/tmp").join(format!("dg-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let socket = root.join("server.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut key = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.trim_end().split_once(':')
                    && name.eq_ignore_ascii_case("sec-websocket-key")
                {
                    key = Some(value.trim().to_owned());
                }
            }
            let key = key.unwrap();
            let digest =
                Sha1::digest(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
            let accept = base64::engine::general_purpose::STANDARD.encode(digest);
            write!(stream, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").unwrap();
            stream.flush().unwrap();
            let request = read_masked_client_frame(&mut stream);
            let value: Value = serde_json::from_slice(&request).unwrap();
            assert_eq!(value["id"], 1);
            assert_eq!(value["method"], "initialize");
            let response =
                serde_json::to_vec(&serde_json::json!({"id":1,"result":{"codexHome":"/tmp/home"}}))
                    .unwrap();
            stream.write_all(&frame(true, 0x1, &response)).unwrap();
            stream.flush().unwrap();
        });
        let mut client = JsonRpcConnection::connect(&socket, Duration::from_secs(2)).unwrap();
        assert_eq!(
            client.request("initialize", serde_json::json!({})).unwrap()["codexHome"],
            "/tmp/home"
        );
        server.join().unwrap();
        std::fs::remove_file(&socket).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }

    fn read_masked_client_frame(stream: &mut UnixStream) -> Vec<u8> {
        let mut prefix = [0_u8; 2];
        stream.read_exact(&mut prefix).unwrap();
        assert_ne!(prefix[1] & 0x80, 0);
        let length = usize::from(prefix[1] & 0x7f);
        assert!(length <= 125);
        let mut mask = [0_u8; 4];
        stream.read_exact(&mut mask).unwrap();
        let mut payload = vec![0_u8; length];
        stream.read_exact(&mut payload).unwrap();
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        payload
    }
}
