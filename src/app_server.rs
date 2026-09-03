//! Private WebSocket-over-Unix transport for one Run worker.

use crate::jcs::{canonicalize, parse};
use base64::Engine as _;
use serde_json::Value;
use sha1::{Digest as _, Sha1};
use sha2::Sha256;
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;
use zeroize::Zeroizing;

pub const MAX_HTTP_UPGRADE_BYTES: usize = 16 * 1024;
pub const MAX_WEBSOCKET_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Bound for *unsolicited* messages, which the reader must buffer whole before
/// it can know what they are.  Solicited responses are streamed instead and are
/// deliberately not subject to this bound.
pub const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// A JSON-RPC response must present its top-level `id` member inside this
/// window so a streaming reader can correlate before it has read the body.
pub const EARLY_TOP_LEVEL_ID_PREFIX_BYTES: usize = 64 * 1024;
/// A solicited response envelope is every top-level member except the streamed
/// `result` value.  It must stay small enough to parse without streaming.
pub const MAX_SOLICITED_ENVELOPE_BYTES: usize = 64 * 1024;
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

    /// Read one whole text message, handing each decoded chunk to `accept` as
    /// it arrives.  Deliberately carries no total-message bound: a solicited
    /// reply is only as large as the caller asked it to be, and the caller — not
    /// the transport — decides what of it is worth retaining.
    pub fn receive_streaming<F>(&mut self, mut accept: F) -> Result<(), TransportError>
    where
        F: FnMut(&[u8]) -> Result<(), TransportError>,
    {
        let mut utf8 = Utf8Stream::default();
        let mut started = false;
        loop {
            let (final_frame, opcode, payload) = self.read_frame()?;
            match opcode {
                0x0 | 0x1 => {
                    if opcode == 0x1 {
                        if started {
                            return Err(TransportError::InvalidFrame("nested data frame"));
                        }
                        started = true;
                    } else if !started {
                        return Err(TransportError::InvalidFrame("unexpected continuation"));
                    }
                    let text = utf8.accept(&payload)?;
                    if !text.is_empty() {
                        accept(&text)?;
                    }
                    if final_frame {
                        return utf8.finish();
                    }
                }
                0x8 => {
                    if !self.closed {
                        self.send_frame(true, 0x8, &payload)?;
                    }
                    self.closed = true;
                    return Err(TransportError::Closed);
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
        // The frame is masked and assembled whole before any of it is written,
        // which is what lets a wire shared by several senders hand the socket
        // one frame at a time instead of interleaving halves of two.
        self.wire
            .write_all(&frame_bytes(final_frame, opcode, payload)?)?;
        self.wire.flush()?;
        Ok(())
    }
}

/// Mask and assemble one whole client frame.
fn frame_bytes(final_frame: bool, opcode: u8, payload: &[u8]) -> Result<Vec<u8>, TransportError> {
    if payload.len() > MAX_WEBSOCKET_FRAME_BYTES || (opcode & 0x08 != 0 && payload.len() > 125) {
        return Err(TransportError::MessageTooLarge);
    }
    let mut frame = vec![(if final_frame { 0x80 } else { 0 }) | opcode];
    match payload.len() {
        length @ 0..=125 => frame.push(0x80 | u8::try_from(length).expect("bounded")),
        length @ 126..=65_535 => {
            frame.push(0x80 | 126);
            frame.extend(u16::try_from(length).expect("bounded").to_be_bytes());
        }
        length => {
            frame.push(0x80 | 127);
            frame.extend(u64::try_from(length).expect("bounded").to_be_bytes());
        }
    }
    let digest = Sha256::digest(Uuid::now_v7().as_bytes());
    let mask = [digest[0], digest[1], digest[2], digest[3]];
    frame.extend(mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    Ok(frame)
}

/// Incremental, structural view of one top-level JSON-RPC message.
///
/// The scanner never classifies a message textually: it tracks container depth
/// and string state so a member name is recognised only where a member name may
/// legally appear.  That answers the question a streaming reader must settle
/// before it holds the whole body — is this a correlatable response, and did its
/// top-level `id` arrive early enough to correlate?  While scanning it can also
/// rebuild a small *envelope* copy in which one streamed member's value is
/// replaced by `null`, so the response status stays parseable however large the
/// streamed value grows.
#[derive(Debug)]
pub struct TopLevelStreamScanner {
    elide: &'static str,
    envelope: Option<Vec<u8>>,
    elided: bool,
    consumed: usize,
    depth: usize,
    in_string: bool,
    escaped: bool,
    last_significant: u8,
    reading_name: bool,
    name: Vec<u8>,
    name_escaped: bool,
    name_offset: usize,
    pending_name: Option<Vec<u8>>,
    current_name: Option<Vec<u8>>,
    eliding: bool,
    id_offset: Option<usize>,
    started: bool,
    complete: bool,
}

impl TopLevelStreamScanner {
    /// Classify an already-buffered message without retaining a copy.
    #[must_use]
    pub fn classifier() -> Self {
        Self::build("", None)
    }

    /// Stream `elide`'s value to a sink and retain the remaining envelope.
    #[must_use]
    pub fn streaming(elide: &'static str) -> Self {
        Self::build(elide, Some(Vec::new()))
    }

    fn build(elide: &'static str, envelope: Option<Vec<u8>>) -> Self {
        Self {
            elide,
            envelope,
            elided: false,
            consumed: 0,
            depth: 0,
            in_string: false,
            escaped: false,
            last_significant: 0,
            reading_name: false,
            name: Vec::new(),
            name_escaped: false,
            name_offset: 0,
            pending_name: None,
            current_name: None,
            eliding: false,
            id_offset: None,
            started: false,
            complete: false,
        }
    }

    /// Byte offset at which the top-level `id` member name began, if any.
    #[must_use]
    pub const fn id_offset(&self) -> Option<usize> {
        self.id_offset
    }

    /// A top-level `id` that arrived inside the correlation window.
    #[must_use]
    pub const fn correlatable(&self) -> bool {
        match self.id_offset {
            Some(offset) => offset <= EARLY_TOP_LEVEL_ID_PREFIX_BYTES,
            None => false,
        }
    }

    /// A message carrying a structurally top-level `id` is a reply, not a
    /// notification.
    #[must_use]
    pub const fn is_response(&self) -> bool {
        self.id_offset.is_some()
    }

    #[must_use]
    pub const fn streamed_value(&self) -> bool {
        self.elided
    }

    fn limit(&self) -> usize {
        if self.elided {
            MAX_SOLICITED_ENVELOPE_BYTES
        } else {
            MAX_WEBSOCKET_MESSAGE_BYTES
        }
    }

    /// Consume one chunk, forwarding the elided member's bytes to `streamed`.
    pub fn feed<S: SolicitedSink>(
        &mut self,
        chunk: &[u8],
        streamed: &mut S,
    ) -> Result<(), TransportError> {
        let mut start = None;
        for (index, byte) in chunk.iter().enumerate() {
            let before = self.eliding;
            self.step(*byte)?;
            if self.eliding && !before {
                start = Some(index + 1);
            }
            if !self.eliding && before {
                let from = start.take().unwrap_or(0);
                streamed.accept(&chunk[from..index])?;
            }
        }
        if self.eliding {
            let from = start.unwrap_or(0);
            streamed.accept(&chunk[from..])?;
        }
        Ok(())
    }

    fn push(&mut self, byte: u8) -> Result<(), TransportError> {
        let limit = self.limit();
        let Some(envelope) = self.envelope.as_mut() else {
            return Ok(());
        };
        if envelope.len() >= limit {
            return Err(TransportError::CorrelationMismatch);
        }
        envelope.push(byte);
        Ok(())
    }

    fn step(&mut self, byte: u8) -> Result<(), TransportError> {
        self.consumed += 1;
        if self.complete {
            return match byte {
                b' ' | b'\t' | b'\r' | b'\n' => Ok(()),
                _ => Err(TransportError::InvalidJson),
            };
        }
        if self.in_string {
            return self.step_in_string(byte);
        }
        match byte {
            b' ' | b'\t' | b'\r' | b'\n' => {
                if !self.eliding {
                    self.push(byte)?;
                }
                return Ok(());
            }
            b'"' => {
                if self.depth == 1 && matches!(self.last_significant, b'{' | b',') {
                    self.reading_name = true;
                    self.name.clear();
                    self.name_escaped = false;
                    self.name_offset = self.consumed;
                }
                self.in_string = true;
            }
            b'{' | b'[' => {
                if self.depth == 0 {
                    if byte != b'{' {
                        return Err(TransportError::InvalidJson);
                    }
                    self.started = true;
                }
                self.depth += 1;
            }
            b'}' => {
                if self.depth == 0 {
                    return Err(TransportError::InvalidJson);
                }
                if self.depth == 1 {
                    self.eliding = false;
                    self.current_name = None;
                    self.depth = 0;
                    self.complete = true;
                } else {
                    self.depth -= 1;
                }
            }
            b']' => {
                if self.depth <= 1 {
                    return Err(TransportError::InvalidJson);
                }
                self.depth -= 1;
            }
            b',' if self.depth == 1 => {
                self.eliding = false;
                self.current_name = None;
            }
            _ => {}
        }
        self.last_significant = byte;
        if !self.eliding {
            self.push(byte)?;
        }
        if byte == b':' && self.depth == 1 {
            self.current_name = self.pending_name.take();
            if !self.elide.is_empty() && self.current_name.as_deref() == Some(self.elide.as_bytes())
            {
                if !self.correlatable() {
                    return Err(TransportError::CorrelationMismatch);
                }
                self.eliding = true;
                self.elided = true;
                for placeholder in b"null" {
                    self.push(*placeholder)?;
                }
            }
        }
        Ok(())
    }

    fn step_in_string(&mut self, byte: u8) -> Result<(), TransportError> {
        if !self.eliding {
            self.push(byte)?;
        }
        if self.escaped {
            self.escaped = false;
            return Ok(());
        }
        match byte {
            b'\\' => {
                self.escaped = true;
                if self.reading_name {
                    self.name_escaped = true;
                }
            }
            b'"' => {
                self.in_string = false;
                self.last_significant = b'"';
                if self.reading_name {
                    self.reading_name = false;
                    if self.name_escaped {
                        // Deciding whether an escaped name spells `id` needs
                        // decoding, which is exactly the textual guessing this
                        // scanner exists to avoid, so fail closed instead.
                        return Err(TransportError::CorrelationMismatch);
                    }
                    if self.name == b"id" && self.id_offset.is_none() {
                        self.id_offset = Some(self.name_offset);
                    }
                    self.pending_name = Some(std::mem::take(&mut self.name));
                }
            }
            _ => {
                if self.reading_name {
                    if self.name.len() >= 256 {
                        return Err(TransportError::CorrelationMismatch);
                    }
                    self.name.push(byte);
                }
            }
        }
        Ok(())
    }

    /// The retained envelope once the top-level object has closed.
    pub fn finish(self) -> Result<Vec<u8>, TransportError> {
        if !self.started || !self.complete {
            return Err(TransportError::InvalidJson);
        }
        Ok(self.envelope.unwrap_or_default())
    }
}

/// Destination for the streamed member value of a solicited response.
pub trait SolicitedSink {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), TransportError>;
}

/// Discards every streamed byte; used when only classification is wanted.
#[derive(Debug, Default)]
pub struct DiscardSink;

impl SolicitedSink for DiscardSink {
    fn accept(&mut self, _chunk: &[u8]) -> Result<(), TransportError> {
        Ok(())
    }
}

impl SolicitedSink for Vec<u8> {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), TransportError> {
        self.extend_from_slice(chunk);
        Ok(())
    }
}

/// Incremental UTF-8 validation across WebSocket frame boundaries.
#[derive(Debug, Default)]
struct Utf8Stream {
    tail: Vec<u8>,
}

impl Utf8Stream {
    /// Returns the longest complete-UTF-8 prefix of `tail + chunk` and retains
    /// the trailing partial sequence for the next chunk.
    fn accept(&mut self, chunk: &[u8]) -> Result<Vec<u8>, TransportError> {
        let mut buffer = std::mem::take(&mut self.tail);
        buffer.extend_from_slice(chunk);
        match std::str::from_utf8(&buffer) {
            Ok(_) => Ok(buffer),
            Err(error) if error.error_len().is_none() && error.valid_up_to() + 4 > buffer.len() => {
                self.tail = buffer.split_off(error.valid_up_to());
                Ok(buffer)
            }
            Err(_) => Err(TransportError::InvalidUtf8),
        }
    }

    fn finish(self) -> Result<(), TransportError> {
        if self.tail.is_empty() {
            Ok(())
        } else {
            Err(TransportError::InvalidUtf8)
        }
    }
}

pub struct JsonRpcConnection<T> {
    websocket: WebSocket<T>,
    next_request_id: u64,
    pending: BTreeSet<u64>,
    notifications: VecDeque<Value>,
    last_response_id_offset: Option<usize>,
}

impl JsonRpcConnection<UnixStream> {
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, TransportError> {
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        upgrade(&mut stream)?;
        Ok(Self::new(stream))
    }
}

/// Complete the WebSocket upgrade on an already-connected Unix socket.
fn upgrade(stream: &mut UnixStream) -> Result<(), TransportError> {
    let raw_key = Uuid::now_v7();
    let key = base64::engine::general_purpose::STANDARD.encode(raw_key.as_bytes());
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )?;
    stream.flush()?;
    let response = read_upgrade(stream)?;
    validate_upgrade(&response, &key)
}

impl<T: Wire> JsonRpcConnection<T> {
    #[must_use]
    pub fn new(wire: T) -> Self {
        Self {
            websocket: WebSocket::new(wire),
            next_request_id: 1,
            pending: BTreeSet::new(),
            notifications: VecDeque::new(),
            last_response_id_offset: None,
        }
    }

    /// Byte offset at which the most recent reply presented its structurally
    /// top-level `id`.
    ///
    /// A compatibility probe cannot claim an app-server answers early enough to
    /// correlate before its body arrives without seeing this measurement, and
    /// the offset is only knowable while the message is being read.
    #[must_use]
    pub const fn last_response_id_offset(&self) -> Option<usize> {
        self.last_response_id_offset
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
                    return Err(remote_error(error));
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

    /// Issue a request whose reply this caller solicited and already knows may
    /// be far larger than an unsolicited message may be.  The reply's `result`
    /// value is streamed into `sink` instead of being buffered, so no
    /// whole-message bound applies to it; only the small envelope around it is
    /// retained.  Correlation still fails closed: the reply must present a
    /// structurally top-level `id` before its streamed value begins.
    pub fn request_solicited<S: SolicitedSink>(
        &mut self,
        method: &str,
        params: Value,
        sink: &mut S,
    ) -> Result<Value, TransportError> {
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
            let mut scanner = TopLevelStreamScanner::streaming("result");
            let mut streamed = SolicitedGuard {
                sink,
                accepted: false,
            };
            self.websocket
                .receive_streaming(|chunk| scanner.feed(chunk, &mut streamed))?;
            let accepted = streamed.accepted;
            let solicited = scanner.streamed_value();
            if scanner.is_response() {
                self.last_response_id_offset = scanner.id_offset();
            }
            if accepted != solicited {
                return Err(TransportError::CorrelationMismatch);
            }
            let envelope = decode_message(&scanner.finish()?)?;
            if envelope.get("method").and_then(Value::as_str).is_some() {
                if solicited {
                    return Err(TransportError::CorrelationMismatch);
                }
                if self.notifications.len() >= MAX_CORRELATED_MESSAGES {
                    return Err(TransportError::CorrelationMismatch);
                }
                self.notifications.push_back(envelope);
                continue;
            }
            let response_id = envelope
                .get("id")
                .and_then(Value::as_u64)
                .ok_or(TransportError::CorrelationMismatch)?;
            if response_id != id || !self.pending.remove(&response_id) {
                return Err(TransportError::CorrelationMismatch);
            }
            if let Some(error) = envelope.get("error") {
                return Err(remote_error(error));
            }
            if !solicited {
                return Err(TransportError::CorrelationMismatch);
            }
            return Ok(envelope);
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
                let value = decode_message(&bytes)?;
                let mut scanner = TopLevelStreamScanner::classifier();
                scanner.feed(&bytes, &mut DiscardSink)?;
                if value.get("id").is_some() {
                    if !scanner.correlatable() {
                        return Err(TransportError::CorrelationMismatch);
                    }
                    self.last_response_id_offset = scanner.id_offset();
                }
                Ok(value)
            }
            Incoming::Close { .. } => Err(TransportError::Closed),
        }
    }
}

/// The write half of one WebSocket, shared by every thread that sends on it.
///
/// A frame is masked and assembled whole before the lock is taken, so a control
/// caller's `turn/interrupt` can never interleave with the draining thread's
/// pong and leave the app-server reading half of each.
pub struct SharedWire<W: Write> {
    inner: Mutex<SharedWireState<W>>,
}

struct SharedWireState<W> {
    wire: W,
    closed: bool,
}

impl<W: Write> SharedWire<W> {
    #[must_use]
    pub fn new(wire: W) -> Self {
        Self {
            inner: Mutex::new(SharedWireState {
                wire,
                closed: false,
            }),
        }
    }

    /// Hand one already-framed message to the socket under a single lock.
    fn emit(&self, frame: &[u8]) -> Result<(), TransportError> {
        let mut state = self.inner.lock().map_err(|_| TransportError::Closed)?;
        if state.closed {
            return Err(TransportError::Closed);
        }
        // A close frame is the last thing this wire may carry.  Marking the
        // state before the write is what keeps two threads racing to close
        // from framing two close frames onto one socket.
        state.closed = frame.first().is_some_and(|byte| byte & 0x0f == 0x8);
        state.wire.write_all(frame)?;
        state.wire.flush()?;
        Ok(())
    }

    pub fn send_text(&self, payload: &[u8]) -> Result<(), TransportError> {
        std::str::from_utf8(payload).map_err(|_| TransportError::InvalidUtf8)?;
        self.emit(&frame_bytes(true, 0x1, payload)?)
    }

    /// Send a close frame, treating an already-closed wire as success.
    pub fn send_close(&self, code: u16) -> Result<(), TransportError> {
        match self.emit(&frame_bytes(true, 0x8, &code.to_be_bytes())?) {
            Err(TransportError::Closed) => Ok(()),
            other => other,
        }
    }
}

/// A wire whose reads come from a private half and whose writes are buffered
/// per frame and handed to a shared write half on flush.
///
/// `WebSocket` writes one frame as a header followed by its masked payload and
/// then flushes, so buffering up to the flush is exactly what makes each frame
/// reach the socket whole while another thread is sending its own.
pub struct SplitWire<R: Read, W: Write> {
    read: R,
    pending: Vec<u8>,
    write: Arc<SharedWire<W>>,
}

impl<R: Read, W: Write> SplitWire<R, W> {
    #[must_use]
    pub fn new(read: R, write: Arc<SharedWire<W>>) -> Self {
        Self {
            read,
            pending: Vec::new(),
            write,
        }
    }
}

impl<R: Read, W: Write> Read for SplitWire<R, W> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.read.read(buffer)
    }
}

impl<R: Read, W: Write> Write for SplitWire<R, W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        // The bound is the frame bound plus its longest possible header, so a
        // caller that never flushes cannot grow this buffer without limit.
        if self.pending.len().saturating_add(buffer.len()) > MAX_WEBSOCKET_FRAME_BYTES + 14 {
            return Err(std::io::Error::other("websocket frame exceeds its bound"));
        }
        self.pending.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let frame = std::mem::take(&mut self.pending);
        self.write.emit(&frame).map_err(std::io::Error::other)
    }
}

/// One app-server JSON-RPC connection whose reads and writes belong to
/// different threads.
///
/// Sending never waits on the reader: a request, an interaction answer, or an
/// interrupt is framed and written through the shared write half while the
/// draining side is still blocked on the socket.  Reading stays single-owner —
/// the reassembly state sits behind one lock, taken only by whichever thread
/// currently holds the Run's read grant — so no message is ever torn between
/// two readers.
pub struct DuplexConnection {
    socket: Mutex<WebSocket<SplitWire<UnixStream, UnixStream>>>,
    writer: Arc<SharedWire<UnixStream>>,
    closer: UnixStream,
    queued: Mutex<VecDeque<Value>>,
}

impl DuplexConnection {
    /// Connect, upgrade, and split one app-server socket.
    ///
    /// `timeout` bounds the upgrade and every later write.  Reads are left
    /// unbounded on purpose: a Run that is idle for longer than the transport
    /// timeout is not a failed Run, and a read that expired mid-frame would
    /// leave the reassembly state torn.  `shutdown` is what ends a read.
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, TransportError> {
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        upgrade(&mut stream)?;
        stream.set_read_timeout(None)?;
        Self::attach(stream)
    }

    /// Split an already-upgraded socket.
    pub fn attach(stream: UnixStream) -> Result<Self, TransportError> {
        let read = stream.try_clone()?;
        let write = stream.try_clone()?;
        let writer = Arc::new(SharedWire::new(write));
        Ok(Self {
            socket: Mutex::new(WebSocket::new(SplitWire::new(read, Arc::clone(&writer)))),
            writer,
            closer: stream,
            queued: Mutex::new(VecDeque::new()),
        })
    }

    /// Send one JSON-RPC message from any thread.
    pub fn send(&self, value: &Value) -> Result<(), TransportError> {
        let bytes = serde_json::to_vec(value).map_err(|_| TransportError::InvalidJson)?;
        self.writer.send_text(&bytes)
    }

    /// Send a protected JSON-RPC message and scrub both serialization buffers.
    ///
    /// The caller still owns and scrubs `value`; this method covers the two
    /// additional plaintext copies created by JSON encoding and WebSocket
    /// framing before the kernel write completes.
    pub fn send_sensitive(&self, value: &Value) -> Result<(), TransportError> {
        let bytes =
            Zeroizing::new(serde_json::to_vec(value).map_err(|_| TransportError::InvalidJson)?);
        let frame = Zeroizing::new(frame_bytes(true, 0x1, &bytes)?);
        self.writer.emit(&frame)
    }

    /// Read one whole message, queued ones first.
    pub fn receive(&self) -> Result<Value, TransportError> {
        if let Some(queued) = self.take_queued() {
            return Ok(queued);
        }
        let mut socket = self.socket.lock().map_err(|_| TransportError::Closed)?;
        match socket.receive()? {
            Incoming::Text(bytes) => {
                let value = decode_message(&bytes)?;
                let mut scanner = TopLevelStreamScanner::classifier();
                scanner.feed(&bytes, &mut DiscardSink)?;
                if value.get("id").is_some() && !scanner.correlatable() {
                    return Err(TransportError::CorrelationMismatch);
                }
                Ok(value)
            }
            Incoming::Close { .. } => Err(TransportError::Closed),
        }
    }

    /// Read until the reply to `id` arrives, streaming its `result` into `sink`
    /// rather than buffering it, and queueing every notification met on the way
    /// so the drain still folds them in afterwards.
    pub fn receive_solicited<S: SolicitedSink>(
        &self,
        id: u64,
        sink: &mut S,
    ) -> Result<Value, TransportError> {
        // Taking the read half rather than waiting for it: this is only called
        // with no read grant outstanding, so a busy lock means that invariant
        // broke and the Run must fail closed instead of deadlocking on itself.
        let mut socket = self
            .socket
            .try_lock()
            .map_err(|_| TransportError::CorrelationMismatch)?;
        for _ in 0..MAX_CORRELATED_MESSAGES {
            let mut scanner = TopLevelStreamScanner::streaming("result");
            let mut streamed = SolicitedGuard {
                sink,
                accepted: false,
            };
            socket.receive_streaming(|chunk| scanner.feed(chunk, &mut streamed))?;
            let accepted = streamed.accepted;
            let solicited = scanner.streamed_value();
            if accepted != solicited {
                return Err(TransportError::CorrelationMismatch);
            }
            let envelope = decode_message(&scanner.finish()?)?;
            if envelope.get("method").and_then(Value::as_str).is_some() {
                if solicited {
                    return Err(TransportError::CorrelationMismatch);
                }
                self.queue(envelope)?;
                continue;
            }
            let response_id = envelope
                .get("id")
                .and_then(Value::as_u64)
                .ok_or(TransportError::CorrelationMismatch)?;
            if response_id != id {
                return Err(TransportError::CorrelationMismatch);
            }
            if let Some(error) = envelope.get("error") {
                return Err(remote_error(error));
            }
            if !solicited {
                return Err(TransportError::CorrelationMismatch);
            }
            return Ok(envelope);
        }
        Err(TransportError::CorrelationMismatch)
    }

    /// Take one message read ahead of its turn, if any is waiting.
    pub fn take_queued(&self) -> Option<Value> {
        self.queued.lock().ok()?.pop_front()
    }

    fn queue(&self, value: Value) -> Result<(), TransportError> {
        let mut queued = self.queued.lock().map_err(|_| TransportError::Closed)?;
        if queued.len() >= MAX_CORRELATED_MESSAGES {
            return Err(TransportError::CorrelationMismatch);
        }
        queued.push_back(value);
        Ok(())
    }

    /// Close the conversation and stop the socket both ways, so a thread
    /// blocked reading returns now instead of waiting for the app-server.
    pub fn shutdown(&self) {
        let _ = self.writer.send_close(1000);
        let _ = self.closer.shutdown(Shutdown::Both);
    }
}

/// Fails closed if a streamed value reaches a sink more than once, which would
/// mean two top-level members were both treated as the solicited value.
struct SolicitedGuard<'a, S> {
    sink: &'a mut S,
    accepted: bool,
}

impl<S: SolicitedSink> SolicitedSink for SolicitedGuard<'_, S> {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), TransportError> {
        self.accepted = true;
        self.sink.accept(chunk)
    }
}

/// The transport error one remote JSON-RPC error object stands for.
pub fn remote_error(error: &Value) -> TransportError {
    TransportError::RemoteError {
        code: error
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MIN),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unstructured app-server error")
            .to_owned(),
    }
}

/// Ingest one app-server message through the duplicate-detecting reader before
/// it becomes a `Value`, as ADR-014 requires.
pub fn decode_message(bytes: &[u8]) -> Result<Value, TransportError> {
    let text = std::str::from_utf8(bytes).map_err(|_| TransportError::InvalidUtf8)?;
    let parsed = parse(text).map_err(|_| TransportError::InvalidJson)?;
    let canonical = canonicalize(&parsed).map_err(|_| TransportError::InvalidJson)?;
    serde_json::from_slice(&canonical).map_err(|_| TransportError::InvalidJson)
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

    fn long_frame(final_frame: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![(if final_frame { 0x80 } else { 0 }) | opcode];
        match payload.len() {
            length @ 0..=125 => bytes.push(u8::try_from(length).unwrap()),
            length @ 126..=65_535 => {
                bytes.push(126);
                bytes.extend(u16::try_from(length).unwrap().to_be_bytes());
            }
            length => {
                bytes.push(127);
                bytes.extend(u64::try_from(length).unwrap().to_be_bytes());
            }
        }
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn top_level_membership_is_structural_not_textual() {
        // A notification whose *content* mentions `"id"` is still a
        // notification; only a member of the top-level object counts.
        let notification =
            br#"{"method":"item/completed","params":{"item":{"id":"x"},"text":"\"id\""}}"#;
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(frame(true, 0x1, notification)));
        let message = rpc.next_message().unwrap();
        assert_eq!(message["method"], "item/completed");

        let mut scanner = TopLevelStreamScanner::classifier();
        scanner.feed(notification, &mut DiscardSink).unwrap();
        assert_eq!(scanner.id_offset(), None);
        assert!(!scanner.correlatable());

        let mut response = TopLevelStreamScanner::classifier();
        response
            .feed(br#"{"result":{"id":"nested"},"id":4}"#, &mut DiscardSink)
            .unwrap();
        assert!(response.correlatable());
    }

    #[test]
    fn the_measured_early_id_offset_is_available_to_a_compatibility_probe() {
        let response = br#"{"id":1,"result":{"ok":true}}"#;
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(long_frame(true, 0x1, response)));
        assert!(rpc.last_response_id_offset().is_none());
        rpc.request("initialize", serde_json::json!({})).unwrap();
        assert_eq!(rpc.last_response_id_offset(), Some(2));

        let padded = br#"{  "id" : 1 , "result":{}}"#;
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(long_frame(true, 0x1, padded)));
        rpc.request("x", serde_json::json!({})).unwrap();
        assert_eq!(rpc.last_response_id_offset(), Some(4));
    }

    #[test]
    fn an_escaped_top_level_member_name_fails_closed() {
        // `\u0069d` spells `id`.  Deciding that needs decoding, which is the
        // textual guessing this scanner exists to avoid, so it fails closed.
        let escaped = "{\"\\u0069d\":4,\"result\":{}}";
        let mut scanner = TopLevelStreamScanner::classifier();
        assert_eq!(
            scanner
                .feed(escaped.as_bytes(), &mut DiscardSink)
                .unwrap_err(),
            TransportError::CorrelationMismatch
        );
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(long_frame(
            true,
            0x1,
            escaped.as_bytes(),
        )));
        assert_eq!(
            rpc.next_message().unwrap_err(),
            TransportError::CorrelationMismatch
        );
    }

    #[test]
    fn a_solicited_reply_streams_past_the_unsolicited_message_bound() {
        let payload = "s".repeat(35 * 1024 * 1024);
        let message = format!("{{\"id\":1,\"result\":{{\"text\":\"{payload}\"}}}}");
        let bytes = message.as_bytes();
        let mut input = Vec::new();
        for (index, chunk) in bytes.chunks(8 * 1024 * 1024).enumerate() {
            let final_frame = (index + 1) * 8 * 1024 * 1024 >= bytes.len();
            let opcode = if index == 0 { 0x1 } else { 0x0 };
            input.extend(long_frame(final_frame, opcode, chunk));
        }

        let mut streamed = Vec::new();
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(input.clone()));
        let envelope = rpc
            .request_solicited("thread/read", serde_json::json!({}), &mut streamed)
            .unwrap();
        assert_eq!(envelope["id"], 1);
        assert!(envelope["result"].is_null(), "the streamed value is elided");
        assert_eq!(streamed.len(), payload.len() + "{\"text\":\"\"}".len());

        // The same reply is refused on the unsolicited path, which is what the
        // solicited path exists to get past.
        let mut buffered = JsonRpcConnection::new(Duplex::with_input(input));
        assert_eq!(
            buffered
                .request("thread/read", serde_json::json!({}))
                .unwrap_err(),
            TransportError::MessageTooLarge
        );
    }

    #[test]
    fn a_solicited_reply_that_hides_its_identity_never_reaches_the_sink() {
        let filler = "f".repeat(EARLY_TOP_LEVEL_ID_PREFIX_BYTES + 16);
        let message = format!("{{\"result\":{{\"text\":\"{filler}\"}},\"id\":1}}");
        let mut streamed = Vec::new();
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(long_frame(
            true,
            0x1,
            message.as_bytes(),
        )));
        assert_eq!(
            rpc.request_solicited("thread/read", serde_json::json!({}), &mut streamed)
                .unwrap_err(),
            TransportError::CorrelationMismatch
        );
        assert!(
            streamed.is_empty(),
            "an uncorrelated reply must not reach the caller's sink"
        );
    }

    #[test]
    fn notifications_still_queue_while_a_solicited_reply_is_awaited() {
        let notification =
            serde_json::to_vec(&serde_json::json!({"method":"turn/started","params":{}})).unwrap();
        let response = br#"{"id":1,"result":{"turns":[]}}"#;
        let mut input = long_frame(true, 0x1, &notification);
        input.extend(long_frame(true, 0x1, response));
        let mut streamed = Vec::new();
        let mut rpc = JsonRpcConnection::new(Duplex::with_input(input));
        let envelope = rpc
            .request_solicited("thread/read", serde_json::json!({}), &mut streamed)
            .unwrap();
        assert_eq!(envelope["id"], 1);
        assert_eq!(streamed, br#"{"turns":[]}"#);
        assert_eq!(rpc.next_message().unwrap()["method"], "turn/started");
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
