//! Private WebSocket-over-Unix transport for one Run worker.

use crate::jcs::{canonicalize, parse};
use base64::Engine as _;
use serde_json::Value;
use sha1::{Digest as _, Sha1};
use sha2::Sha256;
use std::collections::{BTreeSet, VecDeque};
use std::io::{IoSlice, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use uuid::Uuid;
use zeroize::Zeroizing;

pub const MAX_HTTP_UPGRADE_BYTES: usize = 16 * 1024;
pub const MAX_WEBSOCKET_FRAME_BYTES: usize = 16 * 1024 * 1024 + 65_536;
/// Bound for *unsolicited* messages, which the reader must buffer whole before
/// it can know what they are.  Solicited responses are streamed instead and are
/// deliberately not subject to this bound.
pub const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// A JSON-RPC response must present its top-level `id` member inside this
/// window so a streaming reader can correlate before it has read the body.
pub const EARLY_TOP_LEVEL_ID_PREFIX_BYTES: usize = 16 * 1024 * 1024;
/// A solicited response envelope is every top-level member except the streamed
/// `result` value.  It must stay small enough to parse without streaming.
pub const MAX_SOLICITED_ENVELOPE_BYTES: usize = MAX_WEBSOCKET_MESSAGE_BYTES;
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
    rejected_frame: Option<Vec<u8>>,
}

impl<T: Wire> WebSocket<T> {
    #[must_use]
    pub fn new(wire: T) -> Self {
        Self {
            wire,
            fragmented_opcode: None,
            fragmented: Vec::new(),
            closed: false,
            rejected_frame: None,
        }
    }

    #[must_use]
    pub fn into_inner(self) -> T {
        self.wire
    }

    pub fn send_text(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        std::str::from_utf8(payload).map_err(|_| TransportError::InvalidUtf8)?;
        let frames = text_frames(payload).collect::<Result<Vec<_>, _>>()?;
        let mut slices = frames
            .iter()
            .map(|frame| IoSlice::new(frame))
            .collect::<Vec<_>>();
        let mut remaining = slices.as_mut_slice();
        while !remaining.is_empty() {
            match self.wire.write_vectored(remaining) {
                Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into()),
                Ok(written) => IoSlice::advance_slices(&mut remaining, written),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        self.wire.flush()?;
        Ok(())
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
        self.receive_with_boundary(|_, _| {})
    }

    fn receive_with_boundary(
        &mut self,
        mut boundary: impl FnMut(&mut T, bool),
    ) -> Result<Incoming, TransportError> {
        loop {
            boundary(&mut self.wire, self.fragmented_opcode.is_none());
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
        self.rejected_frame = None;
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
                    if final_frame {
                        utf8.finish()?;
                        if !text.is_empty()
                            && let Err(error) = accept(&text)
                        {
                            // Reuse the already-read frame only when it is
                            // the whole message. Never accumulate history.
                            if opcode == 0x1 {
                                self.rejected_frame = Some(text);
                            }
                            return Err(error);
                        }
                        return Ok(());
                    }
                    if !text.is_empty() {
                        accept(&text)?;
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

/// The selected native UDS receiver accepts at most16MiB per frame.
const MAX_OUTBOUND_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// A complete text message uses bounded masked frames, including an empty message.
fn text_frames(payload: &[u8]) -> impl Iterator<Item = Result<Vec<u8>, TransportError>> + '_ {
    let count = payload.len().div_ceil(MAX_OUTBOUND_FRAME_BYTES).max(1);
    (0..count).map(move |index| {
        let start = index * MAX_OUTBOUND_FRAME_BYTES;
        let end = payload.len().min(start + MAX_OUTBOUND_FRAME_BYTES);
        frame_bytes(
            index + 1 == count,
            if index == 0 { 0x1 } else { 0x0 },
            &payload[start..end],
        )
    })
}

#[cfg(test)]
thread_local! {static AFTER_OUTBOUND_FRAME:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=std::cell::RefCell::new(None);}
fn after_outbound_frame() {
    #[cfg(test)]
    {
        let callback = AFTER_OUTBOUND_FRAME.with(|slot| slot.borrow_mut().take());
        if let Some(callback) = callback {
            callback();
        }
    }
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
    expected_id: Option<u64>,
    eligible: bool,
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
    id_token: Option<Vec<u8>>,
    id_token_invalid: bool,
    confirmed_id: Option<(u64, usize)>,
    method_seen: bool,
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

    fn matching(id: u64, eligible: bool) -> Self {
        let mut scanner = Self::streaming("result");
        scanner.expected_id = Some(id);
        scanner.eligible = eligible;
        scanner
    }

    fn build(elide: &'static str, envelope: Option<Vec<u8>>) -> Self {
        Self {
            elide,
            expected_id: None,
            eligible: false,
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
            id_token: None,
            id_token_invalid: false,
            confirmed_id: None,
            method_seen: false,
            started: false,
            complete: false,
        }
    }

    /// Byte offset at which the top-level `id` member name began, if any.
    #[must_use]
    pub const fn id_offset(&self) -> Option<usize> {
        self.id_offset
    }

    /// The numeric id value was complete before the raw classification window.
    #[must_use]
    pub const fn correlatable(&self) -> bool {
        matches!(self.confirmed_id, Some((_, available)) if available < EARLY_TOP_LEVEL_ID_PREFIX_BYTES)
    }

    #[must_use]
    pub fn id_available_at(&self) -> Option<usize> {
        self.confirmed_id.map(|(_, available)| available)
    }

    fn observe_id_value(&mut self, byte: u8) {
        let Some(token) = self.id_token.as_mut() else {
            return;
        };
        if token.is_empty() && !self.id_token_invalid && byte.is_ascii_whitespace() {
            return;
        }
        if byte.is_ascii_whitespace() || matches!(byte, b',' | b'}') {
            if !self.id_token_invalid {
                // Use the same JCS number interpretation as decode_message;
                // availability does not impose a new lexical numeric format.
                let value = std::str::from_utf8(token)
                    .ok()
                    .and_then(|text| parse(text).ok())
                    .and_then(|value| canonicalize(&value).ok())
                    .and_then(|bytes| serde_json::from_slice::<u64>(&bytes).ok());
                if let Some(value) = value {
                    self.confirmed_id = Some((value, self.consumed));
                }
            }
            self.id_token = None;
        } else if token.len() < MAX_WEBSOCKET_MESSAGE_BYTES {
            token.push(byte);
        } else {
            // The ordinary raw-message allowance also bounds an unclassified
            // token; eligible ambiguity is rejected earlier at its raw window.
            self.id_token_invalid = true;
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
        if byte.is_ascii_whitespace()
            && !self.in_string
            && self.depth <= 1
            && envelope.last().is_some_and(u8::is_ascii_whitespace)
        {
            return Ok(());
        }
        if envelope.len() >= limit {
            return Err(TransportError::CorrelationMismatch);
        }
        envelope.push(byte);
        Ok(())
    }

    fn step(&mut self, byte: u8) -> Result<(), TransportError> {
        self.consumed += 1;
        self.observe_id_value(byte);
        if self.consumed > MAX_WEBSOCKET_MESSAGE_BYTES && (self.method_seen || !self.eligible) {
            return Err(TransportError::MessageTooLarge);
        }
        if self.eligible
            && self.consumed >= EARLY_TOP_LEVEL_ID_PREFIX_BYTES
            && !self.method_seen
            && !self.confirmed_id.is_some_and(|(id, available)| {
                Some(id) == self.expected_id && available < EARLY_TOP_LEVEL_ID_PREFIX_BYTES
            })
        {
            return Err(TransportError::CorrelationMismatch);
        }
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
            if self.current_name.as_deref() == Some(b"id") {
                self.id_token = Some(Vec::new());
                self.id_token_invalid = false;
            }
            if !self.elide.is_empty() && self.current_name.as_deref() == Some(self.elide.as_bytes())
            {
                // Parse only the already observed prefix. A later method or
                // duplicate member still invalidates the final envelope.
                let mut prefix = self
                    .envelope
                    .as_ref()
                    .ok_or(TransportError::CorrelationMismatch)?
                    .clone();
                prefix.extend_from_slice(b"null}");
                let prefix = decode_message(&prefix)?;
                if prefix.get("method").is_some() {
                    return Ok(()); // Ordinary bounded native request/notification.
                }
                if prefix.get("id").is_some()
                    && prefix.get("id").and_then(Value::as_u64) != self.expected_id
                {
                    return Err(TransportError::CorrelationMismatch);
                }
                if !self.eligible || !self.correlatable() || prefix.get("id").is_none() {
                    return Ok(()); // A bounded reply still undergoes final correlation.
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
                    if self.name == b"method" {
                        self.method_seen = true;
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
        if !self.elided && self.consumed > MAX_WEBSOCKET_MESSAGE_BYTES {
            return Err(TransportError::MessageTooLarge);
        }
        Ok(self.envelope.unwrap_or_default())
    }
}

/// Validates one JSON value without retaining its keys, strings or numbers.
/// Proof extraction has separate identity, duplicate-key and Turn bounds.
#[derive(Debug, Default)]
pub(crate) struct JsonGrammar {
    stack: Vec<JsonFrame>,
    token: JsonToken,
    complete: bool,
}

#[derive(Clone, Copy, Debug)]
enum JsonFrame {
    Object(JsonObject),
    Array(JsonArray),
}

#[derive(Clone, Copy, Debug)]
enum JsonObject {
    KeyOrEnd,
    Key,
    Colon,
    Value,
    CommaOrEnd,
}

#[derive(Clone, Copy, Debug)]
enum JsonArray {
    ValueOrEnd,
    Value,
    CommaOrEnd,
}

#[derive(Clone, Copy, Debug, Default)]
enum JsonToken {
    #[default]
    None,
    String(JsonString),
    Literal {
        bytes: &'static [u8],
        next: usize,
    },
    Number(JsonNumber),
}

#[derive(Clone, Copy, Debug)]
enum JsonNumber {
    Sign,
    Zero,
    Integer,
    Dot,
    Fraction,
    Exponent,
    ExponentSign,
    ExponentDigits,
}

impl JsonNumber {
    fn can_end(self) -> bool {
        matches!(
            self,
            Self::Zero | Self::Integer | Self::Fraction | Self::ExponentDigits
        )
    }

    fn push(self, byte: u8) -> Result<Option<Self>, TransportError> {
        if matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b',' | b']' | b'}') {
            return self
                .can_end()
                .then_some(None)
                .ok_or(TransportError::CorrelationMismatch);
        }
        let next = match (self, byte) {
            (Self::Sign, b'0') => Self::Zero,
            (Self::Sign, b'1'..=b'9') => Self::Integer,
            (Self::Integer, b'0'..=b'9') => Self::Integer,
            (Self::Zero | Self::Integer, b'.') => Self::Dot,
            (Self::Zero | Self::Integer | Self::Fraction, b'e' | b'E') => Self::Exponent,
            (Self::Dot | Self::Fraction, b'0'..=b'9') => Self::Fraction,
            (Self::Exponent, b'+' | b'-') => Self::ExponentSign,
            (Self::Exponent | Self::ExponentSign | Self::ExponentDigits, b'0'..=b'9') => {
                Self::ExponentDigits
            }
            _ => return Err(TransportError::CorrelationMismatch),
        };
        Ok(Some(next))
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum JsonEscape {
    #[default]
    None,
    Slash,
    Hex {
        digits: u8,
        value: u16,
        low: bool,
    },
    LowSlash,
    LowU,
}

#[derive(Clone, Copy, Debug, Default)]
struct JsonString {
    key: bool,
    escape: JsonEscape,
    continuation: u8,
    lower: u8,
    upper: u8,
}

impl JsonString {
    fn push(&mut self, byte: u8) -> Result<bool, TransportError> {
        match self.escape {
            JsonEscape::Slash => {
                self.escape = match byte {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => JsonEscape::None,
                    b'u' => JsonEscape::Hex {
                        digits: 0,
                        value: 0,
                        low: false,
                    },
                    _ => return Err(TransportError::CorrelationMismatch),
                };
            }
            JsonEscape::Hex { digits, value, low } => {
                let digit = match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    b'A'..=b'F' => byte - b'A' + 10,
                    _ => return Err(TransportError::CorrelationMismatch),
                };
                let value = (value << 4) | u16::from(digit);
                self.escape = if digits < 3 {
                    JsonEscape::Hex {
                        digits: digits + 1,
                        value,
                        low,
                    }
                } else if low {
                    if !(0xdc00..=0xdfff).contains(&value) {
                        return Err(TransportError::CorrelationMismatch);
                    }
                    JsonEscape::None
                } else if (0xd800..=0xdbff).contains(&value) {
                    JsonEscape::LowSlash
                } else if (0xdc00..=0xdfff).contains(&value) {
                    return Err(TransportError::CorrelationMismatch);
                } else {
                    JsonEscape::None
                };
            }
            JsonEscape::LowSlash => {
                if byte != b'\\' {
                    return Err(TransportError::CorrelationMismatch);
                }
                self.escape = JsonEscape::LowU;
            }
            JsonEscape::LowU => {
                if byte != b'u' {
                    return Err(TransportError::CorrelationMismatch);
                }
                self.escape = JsonEscape::Hex {
                    digits: 0,
                    value: 0,
                    low: true,
                };
            }
            JsonEscape::None => {
                if self.continuation > 0 {
                    if !(self.lower..=self.upper).contains(&byte) {
                        return Err(TransportError::CorrelationMismatch);
                    }
                    self.continuation -= 1;
                    self.lower = 0x80;
                    self.upper = 0xbf;
                    return Ok(false);
                }
                let (remaining, lower, upper) = match byte {
                    0..=0x1f => return Err(TransportError::CorrelationMismatch),
                    b'"' => return Ok(true),
                    b'\\' => {
                        self.escape = JsonEscape::Slash;
                        return Ok(false);
                    }
                    0x20..=0x7f => return Ok(false),
                    0xc2..=0xdf => (1, 0x80, 0xbf),
                    0xe0 => (2, 0xa0, 0xbf),
                    0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf),
                    0xed => (2, 0x80, 0x9f),
                    0xf0 => (3, 0x90, 0xbf),
                    0xf1..=0xf3 => (3, 0x80, 0xbf),
                    0xf4 => (3, 0x80, 0x8f),
                    _ => return Err(TransportError::CorrelationMismatch),
                };
                self.continuation = remaining;
                self.lower = lower;
                self.upper = upper;
            }
        }
        Ok(false)
    }
}

impl JsonGrammar {
    #[cfg(test)]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.stack.capacity() * std::mem::size_of::<JsonFrame>()
    }

    fn value_complete(&mut self) {
        if self.stack.is_empty() {
            self.complete = true;
        }
    }

    fn close(&mut self) {
        self.stack.pop();
        self.value_complete();
    }

    fn start_value(&mut self, byte: u8) -> Result<(), TransportError> {
        if let Some(frame) = self.stack.last_mut() {
            *frame = match *frame {
                JsonFrame::Object(JsonObject::Value) => JsonFrame::Object(JsonObject::CommaOrEnd),
                JsonFrame::Array(JsonArray::ValueOrEnd | JsonArray::Value) => {
                    JsonFrame::Array(JsonArray::CommaOrEnd)
                }
                _ => return Err(TransportError::CorrelationMismatch),
            };
        }
        match byte {
            b'{' | b'[' => {
                if self.stack.len() >= 256 {
                    return Err(TransportError::CorrelationMismatch);
                }
                self.stack.push(if byte == b'{' {
                    JsonFrame::Object(JsonObject::KeyOrEnd)
                } else {
                    JsonFrame::Array(JsonArray::ValueOrEnd)
                });
            }
            b'"' => self.token = JsonToken::String(JsonString::default()),
            b't' => {
                self.token = JsonToken::Literal {
                    bytes: b"true",
                    next: 1,
                }
            }
            b'f' => {
                self.token = JsonToken::Literal {
                    bytes: b"false",
                    next: 1,
                }
            }
            b'n' => {
                self.token = JsonToken::Literal {
                    bytes: b"null",
                    next: 1,
                }
            }
            b'-' => self.token = JsonToken::Number(JsonNumber::Sign),
            b'0' => self.token = JsonToken::Number(JsonNumber::Zero),
            b'1'..=b'9' => self.token = JsonToken::Number(JsonNumber::Integer),
            _ => return Err(TransportError::CorrelationMismatch),
        }
        Ok(())
    }

    pub(crate) fn push(&mut self, byte: u8) -> Result<(), TransportError> {
        match self.token {
            JsonToken::String(mut string) => {
                if string.push(byte)? {
                    self.token = JsonToken::None;
                    if string.key {
                        *self
                            .stack
                            .last_mut()
                            .ok_or(TransportError::CorrelationMismatch)? =
                            JsonFrame::Object(JsonObject::Colon);
                    } else {
                        self.value_complete();
                    }
                } else {
                    self.token = JsonToken::String(string);
                }
                return Ok(());
            }
            JsonToken::Literal { bytes, next } => {
                if byte != bytes[next] {
                    return Err(TransportError::CorrelationMismatch);
                }
                if next + 1 == bytes.len() {
                    self.token = JsonToken::None;
                    self.value_complete();
                } else {
                    self.token = JsonToken::Literal {
                        bytes,
                        next: next + 1,
                    };
                }
                return Ok(());
            }
            JsonToken::Number(number) => {
                if let Some(next) = number.push(byte)? {
                    self.token = JsonToken::Number(next);
                    return Ok(());
                }
                self.token = JsonToken::None;
                self.value_complete();
            }
            JsonToken::None => {}
        }
        if matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            return Ok(());
        }
        if self.complete {
            return Err(TransportError::CorrelationMismatch);
        }
        match (self.stack.last().copied(), byte) {
            (Some(JsonFrame::Object(JsonObject::KeyOrEnd | JsonObject::CommaOrEnd)), b'}')
            | (Some(JsonFrame::Array(JsonArray::ValueOrEnd | JsonArray::CommaOrEnd)), b']') => {
                self.close()
            }
            (Some(JsonFrame::Object(JsonObject::KeyOrEnd | JsonObject::Key)), b'"') => {
                self.token = JsonToken::String(JsonString {
                    key: true,
                    ..JsonString::default()
                });
            }
            (Some(JsonFrame::Object(JsonObject::Colon)), b':') => {
                *self.stack.last_mut().unwrap() = JsonFrame::Object(JsonObject::Value);
            }
            (Some(JsonFrame::Object(JsonObject::CommaOrEnd)), b',') => {
                *self.stack.last_mut().unwrap() = JsonFrame::Object(JsonObject::Key);
            }
            (Some(JsonFrame::Array(JsonArray::CommaOrEnd)), b',') => {
                *self.stack.last_mut().unwrap() = JsonFrame::Array(JsonArray::Value);
            }
            (Some(JsonFrame::Object(JsonObject::Value)), _)
            | (Some(JsonFrame::Array(JsonArray::ValueOrEnd | JsonArray::Value)), _)
            | (None, _) => self.start_value(byte)?,
            _ => return Err(TransportError::CorrelationMismatch),
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<(), TransportError> {
        if let JsonToken::Number(number) = self.token
            && number.can_end()
        {
            self.token = JsonToken::None;
            self.value_complete();
        }
        if self.complete && self.stack.is_empty() && matches!(self.token, JsonToken::None) {
            Ok(())
        } else {
            Err(TransportError::CorrelationMismatch)
        }
    }
}

/// Destination for the streamed member value of a solicited response.
pub trait SolicitedSink {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), TransportError>;
}

impl SolicitedSink for JsonGrammar {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), TransportError> {
        for byte in chunk {
            self.push(*byte)?;
        }
        Ok(())
    }
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

impl JsonRpcConnection<crate::darwin::DeadlineStream> {
    pub(crate) fn connect_history(path: &Path, timeout: Duration) -> Result<Self, TransportError> {
        let mut stream =
            crate::darwin::DeadlineStream::connect(path, std::time::Instant::now() + timeout)?;
        upgrade(&mut stream)?;
        Ok(Self::new(stream))
    }

    pub(crate) fn request_history<S: SolicitedSink>(
        &mut self,
        params: Value,
        timeout: Duration,
        sink: &mut S,
    ) -> Result<Value, TransportError> {
        let deadline = std::time::Instant::now() + timeout;
        self.websocket.wire.set_deadline(deadline);
        self.request_solicited_inner("thread/read", params, sink, Some(deadline))
    }
}

/// Complete the WebSocket upgrade on an already-connected socket.
fn upgrade(stream: &mut impl Wire) -> Result<(), TransportError> {
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
        self.request_solicited_inner(method, params, sink, None)
    }

    fn request_solicited_inner<S: SolicitedSink>(
        &mut self,
        method: &str,
        params: Value,
        sink: &mut S,
        deadline: Option<std::time::Instant>,
    ) -> Result<Value, TransportError> {
        if let Some(deadline) = deadline {
            check_deadline(deadline)?;
        }
        validate_method(method)?;
        let eligible = history_exception(method, &params);
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
            let mut scanner = TopLevelStreamScanner::matching(id, eligible);
            let mut streamed = SolicitedGuard {
                sink,
                accepted: false,
                grammar: JsonGrammar::default(),
            };
            self.websocket.receive_streaming(|chunk| {
                if let Some(deadline) = deadline {
                    check_deadline(deadline)?;
                }
                scanner.feed(chunk, &mut streamed)?;
                if let Some(deadline) = deadline {
                    check_deadline(deadline)?;
                }
                Ok(())
            })?;
            let accepted = streamed.accepted;
            let solicited = scanner.streamed_value();
            if scanner.is_response() {
                self.last_response_id_offset = scanner.id_offset();
            }
            if accepted != solicited {
                return Err(TransportError::CorrelationMismatch);
            }
            if solicited {
                streamed.grammar.finish()?;
            }
            let correlatable = scanner.correlatable();
            let envelope_bytes = scanner.finish()?;
            let mut envelope = decode_message(&envelope_bytes)?;
            if let Some(method) = envelope.get("method") {
                if method.as_str().is_none() {
                    return Err(TransportError::InvalidJson);
                }
                if solicited {
                    return Err(TransportError::CorrelationMismatch);
                }
                if self.notifications.len() >= MAX_CORRELATED_MESSAGES {
                    return Err(TransportError::CorrelationMismatch);
                }
                self.notifications.push_back(envelope);
                continue;
            }
            if !valid_solicited_response(&envelope, id) || !self.pending.remove(&id) {
                return Err(TransportError::CorrelationMismatch);
            }
            if let Some(error) = envelope.get("error") {
                return Err(remote_error(error));
            }
            if !solicited {
                if !correlatable {
                    return Err(TransportError::CorrelationMismatch);
                }
                forward_bounded_result(&envelope_bytes, sink)?;
                envelope["result"] = Value::Null;
            }
            if let Some(deadline) = deadline {
                check_deadline(deadline)?;
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
                    if value.get("method").is_none() && !scanner.correlatable() {
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
/// Frames are assembled before taking the lock. A text message keeps that lock
/// through all continuations; other messages and control frames keep their byte
/// boundaries and follow the complete message.
pub struct SharedWire<W: Write> {
    inner: Mutex<SharedWireState<W>>,
    sending: Mutex<bool>,
    send_changed: Condvar,
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
            sending: Mutex::new(false),
            send_changed: Condvar::new(),
        }
    }

    fn admit_send(
        &self,
        deadline: Option<std::time::Instant>,
    ) -> Result<SendPermit<'_, W>, TransportError> {
        let mut sending = self.sending.lock().map_err(|_| TransportError::Closed)?;
        while *sending {
            sending = if let Some(deadline) = deadline {
                check_deadline(deadline)?;
                self.send_changed
                    .wait_timeout(
                        sending,
                        deadline.saturating_duration_since(std::time::Instant::now()),
                    )
                    .map_err(|_| TransportError::Closed)?
                    .0
            } else {
                self.send_changed
                    .wait(sending)
                    .map_err(|_| TransportError::Closed)?
            };
        }
        if let Some(deadline) = deadline {
            check_deadline(deadline)?;
        }
        *sending = true;
        Ok(SendPermit(self))
    }

    /// Hold the shared sender for the complete frame group, including every continuation.
    fn emit_frames(&self, frames: &[IoSlice<'_>]) -> Result<(), TransportError> {
        let _permit = self.admit_send(None)?;
        let mut state = self.inner.lock().map_err(|_| TransportError::Closed)?;
        if state.closed {
            return Err(TransportError::Closed);
        }
        state.closed = frames
            .first()
            .and_then(|frame| frame.first())
            .is_some_and(|byte| byte & 0x0f == 0x8);
        let outcome = (|| -> std::io::Result<()> {
            for frame in frames {
                state.wire.write_all(frame)?;
                after_outbound_frame();
            }
            state.wire.flush()
        })();
        if outcome.is_err() {
            state.closed = true;
        }
        outcome.map_err(TransportError::from)
    }

    fn emit(&self, frame: &[u8]) -> Result<(), TransportError> {
        self.emit_frames(&[IoSlice::new(frame)])
    }

    pub fn send_text(&self, payload: &[u8]) -> Result<(), TransportError> {
        std::str::from_utf8(payload).map_err(|_| TransportError::InvalidUtf8)?;
        let frames = text_frames(payload).collect::<Result<Vec<_>, _>>()?;
        let slices = frames
            .iter()
            .map(|frame| IoSlice::new(frame))
            .collect::<Vec<_>>();
        self.emit_frames(&slices)
    }

    /// Send a close frame, treating an already-closed wire as success.
    pub fn send_close(&self, code: u16) -> Result<(), TransportError> {
        match self.emit(&frame_bytes(true, 0x8, &code.to_be_bytes())?) {
            Err(TransportError::Closed) => Ok(()),
            other => other,
        }
    }
}

struct SendPermit<'a, W: Write>(&'a SharedWire<W>);
impl<W: Write> Drop for SendPermit<'_, W> {
    fn drop(&mut self) {
        if let Ok(mut sending) = self.0.sending.lock() {
            *sending = false;
        }
        self.0.send_changed.notify_all();
    }
}

impl SharedWire<UnixStream> {
    fn emit_before(
        &self,
        frames: &[Vec<u8>],
        deadline: std::time::Instant,
    ) -> Result<(), TransportError> {
        let _permit = self.admit_send(Some(deadline))?;
        let mut state = self.inner.lock().map_err(|_| TransportError::Closed)?;
        if state.closed {
            return Err(TransportError::Closed);
        }
        for frame in frames {
            if let Err(error) = crate::darwin::write_before(&state.wire, frame, deadline) {
                state.closed = true;
                let _ = state.wire.shutdown(Shutdown::Both);
                return Err(error.into());
            }
        }
        check_deadline(deadline)
    }

    fn send_before(
        &self,
        payload: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), TransportError> {
        let frames = text_frames(payload).collect::<Result<Vec<_>, _>>()?;
        self.emit_before(&frames, deadline)
    }
}

/// Runtime control frames share the active read operation's absolute deadline.
struct RuntimeWire {
    read: crate::darwin::WakeRead,
    writer: Arc<SharedWire<UnixStream>>,
    pending: Vec<u8>,
}
impl Read for RuntimeWire {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.read.read(bytes)
    }
}
impl Write for RuntimeWire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.pending.len().saturating_add(bytes.len()) > MAX_WEBSOCKET_FRAME_BYTES + 14 {
            return Err(std::io::Error::other("websocket frame exceeds its bound"));
        }
        self.pending.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let frame = std::mem::take(&mut self.pending);
        if frame.is_empty() {
            return Ok(());
        }
        let outcome = if let Some(deadline) = self.read.deadline() {
            self.writer.emit_before(&[frame], deadline)
        } else {
            self.writer.emit(&frame)
        };
        outcome.map_err(std::io::Error::other)
    }
}

/// A wire whose reads come from a private half and whose writes are buffered
/// per frame and handed to a shared write half on flush.
///
/// Individual control frames buffer until flush. Vectored writes carry a whole
/// preframed text message directly through the shared sender's message lock;
/// each frame remains bounded and no larger pending buffer is introduced.
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

    fn write_vectored(&mut self, frames: &[IoSlice<'_>]) -> std::io::Result<usize> {
        if !self.pending.is_empty() {
            return Err(std::io::Error::other(
                "a frame is pending before a complete message",
            ));
        }
        self.write
            .emit_frames(frames)
            .map_err(std::io::Error::other)?;
        Ok(frames.iter().map(|frame| frame.len()).sum())
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
    socket: Mutex<WebSocket<RuntimeWire>>,
    writer: Arc<SharedWire<UnixStream>>,
    closer: UnixStream,
    queued: Mutex<VecDeque<Value>>,
    identity: Uuid,
    ownership: Mutex<ReadOwnership>,
    read_changed: Condvar,
    wake: Arc<crate::darwin::ReadWake>,
}

impl DuplexConnection {
    /// Connect, upgrade, and split one app-server socket.
    ///
    /// `timeout` bounds the upgrade and every later write.  Reads are left
    /// unbounded on purpose: a Run that is idle for longer than the transport
    /// timeout is not a failed Run, and a read that expired mid-frame would
    /// leave the reassembly state torn.  `shutdown` is what ends a read.
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, TransportError> {
        Self::connect_registered(path, timeout, |_| Ok(()), || false)
    }

    pub(crate) fn connect_registered(
        path: &Path,
        timeout: Duration,
        register: impl FnOnce(&UnixStream) -> Result<(), TransportError>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, TransportError> {
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or(TransportError::Closed)?;
        let mut stream = crate::darwin::DeadlineStream::connect_registered(
            path,
            deadline,
            |stream| {
                register(stream).map_err(|_| std::io::Error::from(std::io::ErrorKind::Interrupted))
            },
            cancelled,
        )?
        .into_stream();
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        upgrade(&mut stream)?;
        stream.set_read_timeout(None)?;
        Self::attach(stream)
    }

    /// Split an already-upgraded socket.
    pub fn attach(stream: UnixStream) -> Result<Self, TransportError> {
        let (read, wake) = crate::darwin::WakeRead::new(stream.try_clone()?)?;
        let write = stream.try_clone()?;
        let writer = Arc::new(SharedWire::new(write));
        Ok(Self {
            socket: Mutex::new(WebSocket::new(RuntimeWire {
                read,
                writer: Arc::clone(&writer),
                pending: Vec::new(),
            })),
            writer,
            closer: stream,
            queued: Mutex::new(VecDeque::new()),
            identity: Uuid::now_v7(),
            ownership: Mutex::new(ReadOwnership::default()),
            read_changed: Condvar::new(),
            wake,
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
        let frames = text_frames(&bytes)
            .map(|frame| frame.map(Zeroizing::new))
            .collect::<Result<Vec<_>, _>>()?;
        let slices = frames
            .iter()
            .map(|frame| IoSlice::new(frame))
            .collect::<Vec<_>>();
        self.writer.emit_frames(&slices)
    }

    /// Read one whole message, queued ones first.
    pub fn receive(&self) -> Result<Value, TransportError> {
        if let Some(queued) = self.take_queued() {
            return Ok(queued);
        }
        self.read_background_value()?
            .ok_or(TransportError::CorrelationMismatch)
    }

    fn read_background_value(&self) -> Result<Option<Value>, TransportError> {
        let mut socket = self.socket.lock().map_err(|_| TransportError::Closed)?;
        let incoming = socket.receive_with_boundary(|wire, boundary| wire.read.boundary(boundary));
        if socket.wire.read.yielded {
            return Ok(None);
        }
        match incoming? {
            Incoming::Text(bytes) => {
                let value = decode_message(&bytes)?;
                let mut scanner = TopLevelStreamScanner::classifier();
                scanner.feed(&bytes, &mut DiscardSink)?;
                if value.get("id").is_some()
                    && value.get("method").is_none()
                    && !scanner.correlatable()
                {
                    return Err(TransportError::CorrelationMismatch);
                }
                Ok(Some(value))
            }
            Incoming::Close { .. } => Err(TransportError::Closed),
        }
    }

    #[cfg(test)]
    pub(crate) fn observe_reads(&self) -> std::sync::mpsc::Receiver<usize> {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.socket.lock().unwrap().wire.read.progress = Some(sender);
        receiver
    }

    #[cfg(test)]
    pub(crate) fn active_read_deadline(&self) -> Option<std::time::Instant> {
        self.wake.deadline()
    }

    /// Delivery belongs to the old grant and completes before acknowledgement.
    pub(crate) fn receive_background(
        &self,
        deliver: impl FnOnce(Option<Value>),
    ) -> Result<(), TransportError> {
        let mut ownership = self.ownership.lock().map_err(|_| TransportError::Closed)?;
        while ownership.ticket.is_some() && !ownership.closed {
            #[cfg(test)]
            {
                ownership.waiting += 1;
                self.read_changed.notify_all();
            }
            ownership = self
                .read_changed
                .wait(ownership)
                .map_err(|_| TransportError::Closed)?;
            #[cfg(test)]
            {
                ownership.waiting -= 1;
            }
        }
        if ownership.closed {
            return Err(TransportError::Closed);
        }
        ownership.background = true;
        self.read_changed.notify_all();
        drop(ownership);
        let outcome = if let Some(value) = self.take_queued() {
            Ok(Some(value))
        } else {
            self.read_background_value()
        };
        let outcome = outcome.map(deliver);
        let mut ownership = self.ownership.lock().map_err(|_| TransportError::Closed)?;
        ownership.background = false;
        if outcome.is_err() {
            ownership.closed = true;
        }
        self.read_changed.notify_all();
        outcome.map(|_| ())
    }

    pub(crate) fn lease(
        &self,
        deadline: std::time::Instant,
    ) -> Result<ReadLease<'_>, TransportError> {
        check_deadline(deadline)?;
        let mut ownership = self.ownership.lock().map_err(|_| TransportError::Closed)?;
        if ownership.closed || ownership.ticket.is_some() {
            return Err(TransportError::CorrelationMismatch);
        }
        ownership.sequence = ownership
            .sequence
            .checked_add(1)
            .ok_or(TransportError::Closed)?;
        let ticket = ReadTicket {
            connection: self.identity,
            sequence: ownership.sequence,
        };
        ownership.ticket = Some(ticket);
        self.read_changed.notify_all();
        if let Err(error) = self.wake.pause(deadline) {
            drop(ownership);
            self.abort();
            return Err(error.into());
        }
        while ownership.background && !ownership.closed {
            if let Err(error) = check_deadline(deadline) {
                drop(ownership);
                self.abort();
                return Err(error);
            }
            ownership = self
                .read_changed
                .wait_timeout(
                    ownership,
                    deadline.saturating_duration_since(std::time::Instant::now()),
                )
                .map_err(|_| TransportError::Closed)?
                .0;
        }
        if ownership.closed || check_deadline(deadline).is_err() {
            drop(ownership);
            self.abort();
            return Err(TransportError::Closed);
        }
        // This is an acknowledgement, not a replacement blocking socket lock.
        if self.socket.try_lock().is_err() {
            drop(ownership);
            self.abort();
            return Err(TransportError::CorrelationMismatch);
        }
        ownership.held = true;
        Ok(ReadLease {
            connection: self,
            ticket,
            deadline,
            request: std::cell::Cell::new(None),
            read_finished: std::cell::Cell::new(false),
        })
    }

    fn abort(&self) {
        if let Ok(mut ownership) = self.ownership.lock() {
            ownership.closed = true;
        }
        let _ = self.closer.shutdown(Shutdown::Both);
        self.wake.resume();
        self.read_changed.notify_all();
    }

    /// Read a bounded, unqualified reply and queue intervening native messages.
    /// The Worker's lease carries the actual request shape for the uncapped path.
    pub fn receive_solicited<S: SolicitedSink>(
        &self,
        id: u64,
        sink: &mut S,
    ) -> Result<Value, TransportError> {
        self.receive_solicited_before(id, sink, None, false, &mut false)
    }

    fn receive_solicited_before<S: SolicitedSink>(
        &self,
        id: u64,
        sink: &mut S,
        deadline: Option<std::time::Instant>,
        eligible: bool,
        protocol_complete: &mut bool,
    ) -> Result<Value, TransportError> {
        // Taking the read half rather than waiting for it: this is only called
        // with no read grant outstanding, so a busy lock means that invariant
        // broke and the Run must fail closed instead of deadlocking on itself.
        let mut socket = self
            .socket
            .try_lock()
            .map_err(|_| TransportError::CorrelationMismatch)?;
        socket.wire.read.boundary(false);
        for _ in 0..MAX_CORRELATED_MESSAGES {
            if let Some(deadline) = deadline {
                check_deadline(deadline)?;
            }
            let mut scanner = TopLevelStreamScanner::matching(id, eligible);
            let mut streamed = SolicitedGuard {
                sink,
                accepted: false,
                grammar: JsonGrammar::default(),
            };
            let received = socket.receive_streaming(|chunk| {
                if let Some(deadline) = deadline {
                    check_deadline(deadline)?;
                }
                scanner.feed(chunk, &mut streamed)?;
                if let Some(deadline) = deadline {
                    check_deadline(deadline)?;
                }
                Ok(())
            });
            let rejected_frame = socket.rejected_frame.take();
            if let Err(error) = received {
                if deadline.is_none_or(|deadline| check_deadline(deadline).is_ok())
                    && let Some(bytes) = rejected_frame
                    && valid_rejected_frame(&bytes, id, eligible)
                    && deadline.is_none_or(|deadline| check_deadline(deadline).is_ok())
                {
                    *protocol_complete = true;
                }
                return Err(error);
            }
            let accepted = streamed.accepted;
            let solicited = scanner.streamed_value();
            if accepted != solicited {
                return Err(TransportError::CorrelationMismatch);
            }
            if solicited {
                streamed.grammar.finish()?;
            }
            let correlatable = scanner.correlatable();
            let envelope_bytes = scanner.finish()?;
            let mut envelope = decode_message(&envelope_bytes)?;
            if let Some(method) = envelope.get("method") {
                if method.as_str().is_none() {
                    return Err(TransportError::InvalidJson);
                }
                if solicited {
                    return Err(TransportError::CorrelationMismatch);
                }
                self.queue(envelope)?;
                continue;
            }
            if !valid_solicited_response(&envelope, id) {
                return Err(TransportError::CorrelationMismatch);
            }
            if let Some(deadline) = deadline {
                check_deadline(deadline)?;
            }
            *protocol_complete = true;
            if let Some(error) = envelope.get("error") {
                return Err(remote_error(error));
            }
            if !solicited {
                if !correlatable {
                    return Err(TransportError::CorrelationMismatch);
                }
                if let Some(deadline) = deadline {
                    check_deadline(deadline)?;
                }
                forward_bounded_result(&envelope_bytes, sink)?;
                envelope["result"] = Value::Null;
            }
            if let Some(deadline) = deadline {
                check_deadline(deadline)?;
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
        self.abort();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReadTicket {
    connection: Uuid,
    sequence: u64,
}

#[derive(Default)]
struct ReadOwnership {
    background: bool,
    #[cfg(test)]
    waiting: usize,
    ticket: Option<ReadTicket>,
    held: bool,
    sequence: u64,
    closed: bool,
}

/// Acknowledged ownership of this connection's read half. Drop is its only return.
pub(crate) struct ReadLease<'a> {
    connection: &'a DuplexConnection,
    ticket: ReadTicket,
    deadline: std::time::Instant,
    request: std::cell::Cell<Option<(u64, bool)>>,
    read_finished: std::cell::Cell<bool>,
}

impl ReadLease<'_> {
    fn validate(&self) -> Result<(), TransportError> {
        let ownership = self
            .connection
            .ownership
            .lock()
            .map_err(|_| TransportError::Closed)?;
        if ownership.closed
            || !ownership.held
            || ownership.background
            || ownership.ticket != Some(self.ticket)
            || self.ticket.connection != self.connection.identity
        {
            return Err(TransportError::CorrelationMismatch);
        }
        check_deadline(self.deadline)
    }

    pub(crate) fn send(&self, value: &Value) -> Result<(), TransportError> {
        self.validate()?;
        if self.request.get().is_some() {
            return Err(TransportError::CorrelationMismatch);
        }
        let id = value
            .get("id")
            .and_then(Value::as_u64)
            .ok_or(TransportError::CorrelationMismatch)?;
        self.request.set(Some((
            id,
            value
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|method| history_exception(method, &value["params"])),
        )));
        let bytes = serde_json::to_vec(value).map_err(|_| TransportError::InvalidJson)?;
        let outcome = self.connection.writer.send_before(&bytes, self.deadline);
        if outcome.is_err() {
            self.connection.abort();
        }
        outcome
    }

    pub(crate) fn receive<S: SolicitedSink>(
        &self,
        id: u64,
        sink: &mut S,
    ) -> Result<Value, TransportError> {
        self.validate()?;
        let Some((sent_id, eligible)) = self.request.get() else {
            return Err(TransportError::CorrelationMismatch);
        };
        if id != sent_id {
            return Err(TransportError::CorrelationMismatch);
        }
        let mut complete = false;
        let outcome = self.connection.receive_solicited_before(
            id,
            sink,
            Some(self.deadline),
            eligible,
            &mut complete,
        );
        self.read_finished.set(complete);
        if !complete {
            self.connection.abort();
        }
        outcome
    }
}

impl Drop for ReadLease<'_> {
    fn drop(&mut self) {
        if self.request.get().is_some() && !self.read_finished.get() {
            self.connection.abort();
        }
        if let Ok(mut ownership) = self.connection.ownership.lock() {
            if ownership.ticket == Some(self.ticket) && ownership.held {
                ownership.held = false;
                ownership.ticket = None;
                self.connection.wake.resume();
            } else {
                ownership.closed = true;
                let _ = self.connection.closer.shutdown(Shutdown::Both);
            }
        }
        self.connection.read_changed.notify_all();
    }
}

fn check_deadline(deadline: std::time::Instant) -> Result<(), TransportError> {
    if std::time::Instant::now() >= deadline {
        Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into())
    } else {
        Ok(())
    }
}

fn forward_bounded_result<S: SolicitedSink>(
    bytes: &[u8],
    sink: &mut S,
) -> Result<(), TransportError> {
    let object: std::collections::BTreeMap<&str, &serde_json::value::RawValue> =
        serde_json::from_slice(bytes).map_err(|_| TransportError::InvalidJson)?;
    let result = object
        .get("result")
        .ok_or(TransportError::CorrelationMismatch)?;
    sink.accept(result.get().as_bytes())
}

pub(crate) fn history_exception(method: &str, params: &Value) -> bool {
    method == "thread/read" && params.get("includeTurns").and_then(Value::as_bool) == Some(true)
}

/// Fails closed if a streamed value reaches a sink more than once, which would
/// mean two top-level members were both treated as the solicited value.
struct SolicitedGuard<'a, S> {
    sink: &'a mut S,
    accepted: bool,
    grammar: JsonGrammar,
}

impl<S: SolicitedSink> SolicitedSink for SolicitedGuard<'_, S> {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), TransportError> {
        self.accepted = true;
        self.sink.accept(chunk)?;
        self.grammar.accept(chunk)
    }
}

fn valid_solicited_response(value: &Value, id: u64) -> bool {
    if value.get("method").is_some() || value.get("id").and_then(Value::as_u64) != Some(id) {
        return false;
    }
    match (value.get("result"), value.get("error")) {
        (Some(_), None) => true,
        (None, Some(error)) => {
            error.get("code").and_then(Value::as_i64).is_some()
                && error.get("message").and_then(Value::as_str).is_some()
        }
        _ => false,
    }
}

fn valid_rejected_frame(bytes: &[u8], id: u64, eligible: bool) -> bool {
    let mut scanner = TopLevelStreamScanner::matching(id, eligible);
    let mut grammar = JsonGrammar::default();
    if scanner.feed(bytes, &mut grammar).is_err() {
        return false;
    }
    (!scanner.streamed_value() || grammar.finish().is_ok())
        && scanner.finish().is_ok()
        && decode_message(bytes).is_ok_and(|value| valid_solicited_response(&value, id))
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
    let value: Value =
        serde_json::from_slice(&canonical).map_err(|_| TransportError::InvalidJson)?;
    // Preserve the raw dynamic-call argument bound before Value conversion can
    // remove whitespace or normalize number/string spellings. Other methods
    // and the legacy tool's narrower semantic bound retain their own owners.
    if value["method"] == "item/tool/call" {
        let object: std::collections::BTreeMap<String, &serde_json::value::RawValue> =
            serde_json::from_str(text).map_err(|_| TransportError::InvalidJson)?;
        if let Some(params) = object.get("params") {
            let params: std::collections::BTreeMap<String, &serde_json::value::RawValue> =
                serde_json::from_str(params.get()).map_err(|_| TransportError::InvalidJson)?;
            if params
                .get("arguments")
                .is_some_and(|raw| raw.get().len() > 16 * 1024 * 1024)
            {
                return Err(TransportError::InvalidJson);
            }
        }
    }
    Ok(value)
}

fn validate_method(method: &str) -> Result<(), TransportError> {
    if method.is_empty() || method.len() > 256 {
        Err(TransportError::InvalidJson)
    } else {
        Ok(())
    }
}

fn read_upgrade(stream: &mut impl Read) -> Result<Vec<u8>, TransportError> {
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
pub(crate) mod tests {
    #[test]
    fn dynamic_call_raw_argument_allowance_survives_framing_and_value_normalization() {
        for length in [
            4 * 1024 * 1024 + 1,
            16 * 1024 * 1024 - 1,
            16 * 1024 * 1024,
            16 * 1024 * 1024 + 1,
        ] {
            let raw = format!(
                "{{\"method\":\"item/tool/call\",\"params\":{{\"arguments\":{{{}\"x\":0}}}}}}",
                " ".repeat(length - 7)
            );
            let result = decode_message(raw.as_bytes());
            assert_eq!(
                result.is_ok(),
                length <= 16 * 1024 * 1024,
                "raw argument length {length}"
            );
        }
        assert_eq!(MAX_WEBSOCKET_FRAME_BYTES, 16 * 1024 * 1024 + 65_536);
    }

    use super::*;
    use std::io::{BufRead as _, BufReader, Cursor, Result as IoResult};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;

    pub(crate) fn client_frames(bytes: &[u8]) -> Vec<(bool, u8, Vec<u8>)> {
        let mut input = Cursor::new(bytes);
        let mut frames = Vec::new();
        while (input.position() as usize) < bytes.len() {
            let mut header = [0u8; 2];
            input.read_exact(&mut header).unwrap();
            assert_eq!(header[0] & 0x70, 0);
            assert_ne!(header[1] & 0x80, 0, "client frame was not masked");
            let length = match header[1] & 0x7f {
                126 => {
                    let mut b = [0; 2];
                    input.read_exact(&mut b).unwrap();
                    let n = u16::from_be_bytes(b) as usize;
                    assert!(n >= 126);
                    n
                }
                127 => {
                    let mut b = [0; 8];
                    input.read_exact(&mut b).unwrap();
                    let n = u64::from_be_bytes(b);
                    assert!(n >= 65536 && n >> 63 == 0);
                    usize::try_from(n).unwrap()
                }
                n => usize::from(n),
            };
            assert!(
                length <= 16 * 1024 * 1024,
                "frame exceeds selected native receiver limit"
            );
            let opcode = header[0] & 0xf;
            let final_frame = header[0] & 0x80 != 0;
            if opcode & 8 != 0 {
                assert!(final_frame && length <= 125);
            }
            let mut mask = [0; 4];
            input.read_exact(&mut mask).unwrap();
            let mut payload = vec![0; length];
            input.read_exact(&mut payload).unwrap();
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
            frames.push((final_frame, opcode, payload));
        }
        frames
    }
    pub(crate) fn client_messages(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut pending = None;
        let mut messages = Vec::new();
        for (final_frame, opcode, payload) in client_frames(bytes) {
            match opcode {
                1 => {
                    assert!(pending.is_none(), "interleaved text message");
                    pending = Some(payload);
                }
                0 => pending
                    .as_mut()
                    .expect("orphan continuation")
                    .extend(payload),
                8..=10 => continue,
                _ => panic!("invalid client data opcode"),
            }
            if final_frame {
                let message = pending.take().unwrap();
                std::str::from_utf8(&message).unwrap();
                messages.push(message);
            }
        }
        assert!(pending.is_none(), "unfinished client message");
        messages
    }

    #[test]
    fn outbound_frame_partitions_and_short_writes_preserve_exact_utf8_text() {
        for length in [
            0,
            125,
            126,
            65535,
            65536,
            16 * 1024 * 1024 - 1,
            16 * 1024 * 1024,
            16 * 1024 * 1024 + 1,
            32 * 1024 * 1024 + 7,
        ] {
            let payload = vec![b'x'; length];
            let mut socket = WebSocket::new(Duplex::default());
            socket.send_text(&payload).unwrap();
            let bytes = socket.into_inner().output;
            let frames = client_frames(&bytes);
            assert_eq!(frames.len(), length.div_ceil(16 * 1024 * 1024).max(1));
            assert_eq!(frames[0].1, 1);
            assert!(frames.iter().skip(1).all(|frame| frame.1 == 0));
            assert!(frames.last().unwrap().0);
            assert!(frames.iter().take(frames.len() - 1).all(|frame| !frame.0));
            assert_eq!(client_messages(&bytes), vec![payload]);
        }
        struct Short {
            output: Vec<u8>,
            interrupted: bool,
            zero: bool,
        }
        impl Read for Short {
            fn read(&mut self, _: &mut [u8]) -> IoResult<usize> {
                Ok(0)
            }
        }
        impl Write for Short {
            fn write(&mut self, bytes: &[u8]) -> IoResult<usize> {
                if self.interrupted {
                    self.interrupted = false;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                if self.zero {
                    return Ok(0);
                }
                let n = bytes.len().min(1024);
                self.output.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> IoResult<()> {
                Ok(())
            }
        }
        let payload = format!("{}界\"\\", "a".repeat(16 * 1024 * 1024 - 1));
        let mut socket = WebSocket::new(Short {
            output: vec![],
            interrupted: true,
            zero: false,
        });
        socket.send_text(payload.as_bytes()).unwrap();
        assert_eq!(
            client_messages(&socket.into_inner().output),
            vec![payload.into_bytes()]
        );
        let mut zero = WebSocket::new(Short {
            output: vec![],
            interrupted: false,
            zero: true,
        });
        assert!(matches!(zero.send_text(b"x"), Err(TransportError::Io(_))));
        let mut invalid = WebSocket::new(Duplex::default());
        assert_eq!(invalid.send_text(&[0xff]), Err(TransportError::InvalidUtf8));
        assert!(invalid.into_inner().output.is_empty());
    }

    #[test]
    fn shared_outbound_message_orders_competing_text_and_reader_control_frames() {
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> IoResult<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> IoResult<()> {
                Ok(())
            }
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let wire = Arc::new(SharedWire::new(Capture(Arc::clone(&output))));
        let (entered, waiting) = std::sync::mpsc::sync_channel(0);
        let (release, resume) = std::sync::mpsc::sync_channel(0);
        let a = Arc::clone(&wire);
        let payload = vec![b'a'; 16 * 1024 * 1024 + 1];
        let expected = payload.clone();
        let first = thread::spawn(move || {
            AFTER_OUTBOUND_FRAME.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                }))
            });
            a.send_text(&payload).unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        let (observed, observation) = std::sync::mpsc::sync_channel(0);
        let (completed, completion) = std::sync::mpsc::sync_channel(1);
        let b = Arc::clone(&wire);
        let second = thread::spawn(move || {
            let held = b.inner.try_lock().is_err();
            observed.send(held).unwrap();
            let input = [frame(true, 9, b"ping"), frame(true, 1, b"server")].concat();
            let mut reader = WebSocket::new(SplitWire::new(Cursor::new(input), Arc::clone(&b)));
            assert_eq!(
                reader.receive().unwrap(),
                Incoming::Text(b"server".to_vec())
            );
            b.send_text(b"competing-message").unwrap();
            completed.send(()).unwrap();
        });
        let held = observation.recv_timeout(Duration::from_secs(10)).unwrap();
        if !held {
            completion.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        release.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        let bytes = output.lock().unwrap().clone();
        assert_eq!(
            client_messages(&bytes),
            vec![expected, b"competing-message".to_vec()]
        );
        let controls = client_frames(&bytes)
            .into_iter()
            .filter(|frame| frame.1 >= 8)
            .collect::<Vec<_>>();
        assert_eq!(controls, vec![(true, 10, b"ping".to_vec())]);
        wire.send_close(1000).unwrap();
        wire.send_close(1000).unwrap();
        assert_eq!(wire.send_text(b"after-close"), Err(TransportError::Closed));
        assert_eq!(
            client_frames(&output.lock().unwrap())
                .iter()
                .filter(|frame| frame.1 == 8)
                .count(),
            1
        );
    }

    #[test]
    fn outbound_write_failure_poison_and_split_pending_bound_refuse_more_bytes() {
        struct Fail {
            remaining: usize,
        }
        impl Write for Fail {
            fn write(&mut self, bytes: &[u8]) -> IoResult<usize> {
                if self.remaining == 0 {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                let n = bytes.len().min(self.remaining);
                self.remaining -= n;
                Ok(n)
            }
            fn flush(&mut self) -> IoResult<()> {
                Ok(())
            }
        }
        let shared = SharedWire::new(Fail { remaining: 32 });
        assert!(matches!(
            shared.send_text(&vec![b'a'; 16 * 1024 * 1024 + 1]),
            Err(TransportError::Io(_))
        ));
        assert_eq!(
            shared.send_text(b"after-failure"),
            Err(TransportError::Closed)
        );
        let output = Arc::new(SharedWire::new(Vec::<u8>::new()));
        let mut split = SplitWire::new(Cursor::new(vec![]), Arc::clone(&output));
        assert!(
            split
                .write(&vec![0; MAX_WEBSOCKET_FRAME_BYTES + 15])
                .is_err()
        );
        assert!(output.inner.lock().unwrap().wire.is_empty());
        split.write_all(&[0x81]).unwrap();
        assert!(
            split
                .write_vectored(&[IoSlice::new(&frame_bytes(true, 1, b"later").unwrap())])
                .is_err()
        );
        assert!(output.inner.lock().unwrap().wire.is_empty());
    }

    #[test]
    fn sensitive_duplex_outbound_uses_the_same_complete_message_fragmentation() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let receiver = thread::spawn(move || {
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let connection = DuplexConnection::attach(socket).unwrap();
        let payload = serde_json::json!({"protected":"x".repeat(16*1024*1024+1)});
        connection.send_sensitive(&payload).unwrap();
        connection.shutdown();
        let bytes = receiver.join().unwrap();
        let messages = client_messages(&bytes);
        assert_eq!(messages, vec![serde_json::to_vec(&payload).unwrap()]);
        assert!(client_frames(&bytes).len() > 1);
    }

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

    fn lease_peer() -> (
        Arc<DuplexConnection>,
        UnixStream,
        std::sync::mpsc::Receiver<usize>,
    ) {
        let (socket, peer) = UnixStream::pair().unwrap();
        let wire = Arc::new(DuplexConnection::attach(socket).unwrap());
        let (send, receive) = std::sync::mpsc::channel();
        wire.socket.lock().unwrap().wire.read.progress = Some(send);
        (wire, peer, receive)
    }

    fn parked(progress: &std::sync::mpsc::Receiver<usize>) {
        while progress.recv_timeout(Duration::from_secs(5)).unwrap() != 0 {}
    }

    fn lease_deadline() -> std::time::Instant {
        std::time::Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn read_lease_yields_quiet_reader_and_excludes_a_delayed_grant() {
        let (wire, mut peer, progress) = lease_peer();
        let background = Arc::clone(&wire);
        let reader = std::thread::spawn(move || {
            background
                .receive_background(|value| assert!(value.is_none()))
                .unwrap()
        });
        parked(&progress);
        let lease = wire.lease(lease_deadline()).unwrap();
        reader.join().unwrap();
        assert!(!wire.ownership.lock().unwrap().background);
        assert!(
            wire.socket.try_lock().is_ok(),
            "ACK must release the actual read half"
        );
        let background = Arc::clone(&wire);
        let (start, started) = std::sync::mpsc::channel();
        let delayed = std::thread::spawn(move || {
            start.send(()).unwrap();
            background
                .receive_background(|value| assert_eq!(value.unwrap()["method"], "after"))
                .unwrap();
        });
        started.recv().unwrap();
        let mut ownership = wire.ownership.lock().unwrap();
        while ownership.waiting == 0 && !ownership.background {
            let (state, timeout) = wire
                .read_changed
                .wait_timeout(ownership, Duration::from_secs(5))
                .unwrap();
            ownership = state;
            assert!(
                !timeout.timed_out(),
                "delayed grant never reached the ownership gate"
            );
        }
        assert!(
            !ownership.background,
            "delayed grant entered read half under lease"
        );
        drop(ownership);
        assert!(
            !wire.ownership.lock().unwrap().background,
            "a pending grant cannot read under the lease"
        );
        peer.write_all(&frame(true, 1, br#"{"method":"after"}"#))
            .unwrap();
        // Returning a lease that never sent is cancellation with zero dispatch.
        drop(lease);
        delayed.join().unwrap();
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn read_lease_finishes_partial_frames_before_ack_and_retains_delivery_order() {
        let text = br#"{"method":"before","params":"a"}"#;
        let complete = frame(true, 1, text);
        let unicode = "{\"method\":\"before\",\"params\":\"한\"}".as_bytes();
        let split = unicode.iter().position(|b| *b >= 128).unwrap() + 1;
        let mut fragmented = frame(false, 1, &unicode[..split]);
        let cut = fragmented.len();
        fragmented.extend(frame(true, 9, b"ping"));
        fragmented.extend(frame(true, 0, &unicode[split..]));
        for (bytes, cut) in [
            (complete.clone(), 1),
            (complete.clone(), 5),
            (fragmented, cut),
        ] {
            let (wire, mut peer, progress) = lease_peer();
            peer.write_all(&bytes[..cut]).unwrap();
            let background = Arc::clone(&wire);
            let (delivery, delivered) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                background
                    .receive_background(|value| delivery.send(value.unwrap()).unwrap())
                    .unwrap()
            });
            parked(&progress); // read consumed the prefix and is blocked inside this message
            let leased = Arc::clone(&wire);
            let (ack, acknowledged) = std::sync::mpsc::channel();
            let drain = std::thread::spawn(move || {
                let lease = leased.lease(lease_deadline()).unwrap();
                ack.send(()).unwrap();
                assert_eq!(
                    delivered.try_recv().unwrap()["method"],
                    "before",
                    "delivery precedes ACK"
                );
                lease
                    .send(&serde_json::json!({"id":7,"method":"thread/read","params":{}}))
                    .unwrap();
                let mut sink = Vec::new();
                lease.receive(7, &mut sink).unwrap();
                assert_eq!(sink, br#"{"ok":true}"#);
                assert_eq!(leased.take_queued().unwrap()["method"], "during");
                assert_eq!(
                    leased.take_queued().unwrap()["id"],
                    7,
                    "method+id is a native request"
                );
                assert!(leased.take_queued().is_none());
            });
            let mut state = wire.ownership.lock().unwrap();
            while state.ticket.is_none() {
                state = wire.read_changed.wait(state).unwrap();
            }
            assert!(state.background && !state.held);
            assert!(
                acknowledged.try_recv().is_err(),
                "partial message cannot ACK"
            );
            drop(state);
            peer.write_all(&bytes[cut..]).unwrap();
            acknowledged.recv_timeout(Duration::from_secs(5)).unwrap();
            // A fragmented message's pong is a separate control frame.
            let mut request = read_masked_client_frame(&mut peer);
            if request == b"ping" {
                request = read_masked_client_frame(&mut peer);
            }
            assert_eq!(serde_json::from_slice::<Value>(&request).unwrap()["id"], 7);
            for message in [
                br#"{"method":"during"}"#.as_slice(),
                br#"{"method":"native/request","id":7,"params":{}}"#,
                br#"{"id":7,"result":{"ok":true}}"#,
            ] {
                peer.write_all(&frame(true, 1, message)).unwrap();
            }
            reader.join().unwrap();
            drain.join().unwrap();
        }
    }

    #[test]
    fn read_lease_binds_connection_ticket_and_single_send() {
        for corrupt_connection in [false, true] {
            let (wire, mut peer, _) = lease_peer();
            let mut lease = wire.lease(lease_deadline()).unwrap();
            if corrupt_connection {
                lease.ticket.connection = Uuid::now_v7();
            } else {
                lease.ticket.sequence += 1;
            }
            assert_eq!(
                lease.send(&serde_json::json!({"id":1,"method":"thread/read"})),
                Err(TransportError::CorrelationMismatch)
            );
            drop(lease);
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).unwrap();
            assert!(bytes.is_empty(), "invalid ticket dispatched bytes");
        }
        let (wire, mut peer, _) = lease_peer();
        let lease = wire.lease(lease_deadline()).unwrap();
        let request = serde_json::json!({"id":1,"method":"thread/read"});
        lease.send(&request).unwrap();
        assert_eq!(
            lease.send(&request),
            Err(TransportError::CorrelationMismatch)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&read_masked_client_frame(&mut peer)).unwrap(),
            request
        );
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn read_lease_fences_partial_timeout_and_shutdown_wakes_pending_handoff() {
        for shutdown in [false, true] {
            let (wire, mut peer, progress) = lease_peer();
            peer.write_all(&[0x81]).unwrap();
            let background = Arc::clone(&wire);
            let reader = std::thread::spawn(move || {
                background.receive_background(|_| panic!("partial header delivered"))
            });
            parked(&progress);
            let leased = Arc::clone(&wire);
            let drain = std::thread::spawn(move || {
                leased
                    .lease(std::time::Instant::now() + Duration::from_millis(100))
                    .is_err()
            });
            let mut ownership = wire.ownership.lock().unwrap();
            while ownership.ticket.is_none() {
                ownership = wire.read_changed.wait(ownership).unwrap();
            }
            drop(ownership);
            if shutdown {
                wire.shutdown();
            }
            assert!(drain.join().unwrap());
            assert!(reader.join().unwrap().is_err());
            assert!(wire.ownership.lock().unwrap().closed);
            assert!(wire.lease(lease_deadline()).is_err());
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).unwrap();
            assert!(bytes.is_empty());
        }
    }

    #[test]
    fn read_lease_solicited_failure_keeps_only_complete_message_connections() {
        struct Reject;
        impl SolicitedSink for Reject {
            fn accept(&mut self, _: &[u8]) -> Result<(), TransportError> {
                Err(TransportError::InvalidJson)
            }
        }
        for final_frame in [false, true] {
            let (wire, mut peer, _) = lease_peer();
            let lease = wire.lease(lease_deadline()).unwrap();
            lease
                .send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":true}}))
                .unwrap();
            peer.write_all(&frame(final_frame, 1, br#"{"id":1,"result":{}}"#))
                .unwrap();
            assert_eq!(
                lease.receive(1, &mut Reject),
                Err(TransportError::InvalidJson)
            );
            assert_eq!(wire.ownership.lock().unwrap().closed, !final_frame);
            drop(lease);
            if final_frame {
                peer.write_all(&frame(true, 1, br#"{"method":"after"}"#))
                    .unwrap();
                wire.receive_background(|value| assert_eq!(value.unwrap()["method"], "after"))
                    .unwrap();
            }
        }
        let (wire, mut peer, _) = lease_peer();
        let lease = wire.lease(lease_deadline()).unwrap();
        lease
            .send(
                &serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":true}}),
            )
            .unwrap();
        peer.write_all(&frame(
            true,
            1,
            br#"{"id":1,"error":{"code":-1,"message":"refused"}}"#,
        ))
        .unwrap();
        assert!(matches!(
            lease.receive(1, &mut Vec::new()),
            Err(TransportError::RemoteError { .. })
        ));
        assert!(!wire.ownership.lock().unwrap().closed);
        for index in 0..MAX_CORRELATED_MESSAGES {
            wire.queue(serde_json::json!({"method":"queued","params":index}))
                .unwrap();
        }
        assert_eq!(
            wire.queue(serde_json::json!({"method":"overflow"})),
            Err(TransportError::CorrelationMismatch)
        );
        assert_eq!(wire.queued.lock().unwrap().len(), MAX_CORRELATED_MESSAGES);
    }

    #[test]
    fn read_lease_invalid_envelopes_remain_fenced_after_a_final_frame() {
        struct Reject;
        impl SolicitedSink for Reject {
            fn accept(&mut self, _: &[u8]) -> Result<(), TransportError> {
                Err(TransportError::InvalidJson)
            }
        }
        let long_member = format!(r#"{{"id":1,"result":{{}},"{}":0}}"#, "x".repeat(257));
        for body in [
            r#"{"id":1,"result":{}}trailing"#,
            r#"{"id":1,"result":{},"id":1}"#,
            r#"{"id":1,"result":{},"method":"late"}"#,
            r#"{"id":2,"result":{}}"#,
            r#"{"id":1,"result":{},"error":{"code":-1,"message":"both"}}"#,
            r#"{"id":1,"error":{"code":-1}}"#,
            long_member.as_str(),
        ] {
            for reject in [false, true] {
                let (wire, mut peer, _) = lease_peer();
                let lease = wire.lease(lease_deadline()).unwrap();
                lease.send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":true}})).unwrap();
                read_masked_client_frame(&mut peer);
                peer.write_all(&long_frame(true, 1, body.as_bytes()))
                    .unwrap();
                let result = if reject {
                    lease.receive(1, &mut Reject)
                } else {
                    lease.receive(1, &mut Vec::new())
                };
                assert!(result.is_err(), "{body}, reject={reject}");
                drop(lease);
                assert!(
                    wire.lease(lease_deadline()).is_err(),
                    "{body}, reject={reject}"
                );
                let mut bytes = Vec::new();
                peer.read_to_end(&mut bytes).unwrap();
                assert!(bytes.is_empty(), "failed connection dispatched again");
            }
        }
        // An application refusal must not hide malformed body grammar either.
        for body in [
            r#"{"id":1,"result":{broken}}"#,
            r#"{"id":1,"result":{"x":1,"x":2}}"#,
        ] {
            let (wire, mut peer, _) = lease_peer();
            let lease = wire.lease(lease_deadline()).unwrap();
            lease.send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":true}})).unwrap();
            read_masked_client_frame(&mut peer);
            peer.write_all(&long_frame(true, 1, body.as_bytes()))
                .unwrap();
            assert!(lease.receive(1, &mut Reject).is_err());
            drop(lease);
            assert!(wire.lease(lease_deadline()).is_err());
        }
    }

    #[test]
    fn solicited_body_grammar_finishes_before_connection_return() {
        for (body, valid, recoverable) in [
            (br#"{"id":1,"result":}"#.as_slice(), false, false),
            (br#"{"id":1,"result":1e}"#.as_slice(), false, false),
            (br#"{"id":1,"result":tru}"#.as_slice(), false, false),
            (br#"{"id":1,"result":[}}"#.as_slice(), false, false),
            (br#"{"id":1,"result":null}"#.as_slice(), true, false),
            (br#"{"id":1,"result":1}"#.as_slice(), true, false),
            (br#"{"id":1,"result":true}"#.as_slice(), true, false),
            (br#"{"id":1,"result":{"thread":{"id":"thread","parentThreadId":null,"turns":[{"id":"turn","status":"completed","items":[],"itemsView":"full"}]}}}"#.as_slice(), true, true),
        ] {
            for fragmented in [false, true] {
                let frames = if fragmented {
                    let cut = body.len() - 1;
                    let mut frames = long_frame(false, 1, &body[..cut]);
                    frames.extend(long_frame(true, 0, &body[cut..]));
                    frames
                } else {
                    long_frame(true, 1, body)
                };
                for duplex in [false, true] {
                    let mut sink = crate::turn::ThreadTurnSink::for_recovery("thread", "turn");
                    let mut connection = None;
                    let outcome = if duplex {
                        let (wire, mut peer, _) = lease_peer();
                        let lease = wire.lease(lease_deadline()).unwrap();
                        lease.send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":true}})).unwrap();
                        read_masked_client_frame(&mut peer);
                        peer.write_all(&frames).unwrap();
                        let result = lease.receive(1, &mut sink);
                        drop(lease);
                        connection = Some((wire, peer));
                        result
                    } else {
                        let mut rpc = JsonRpcConnection::new(Duplex::with_input(frames.clone()));
                        rpc.request_solicited("thread/read", serde_json::json!({"includeTurns":true}), &mut sink)
                    };
                    assert_eq!(outcome.is_ok(), valid, "{body:?}, fragmented={fragmented}, duplex={duplex}: {outcome:?}");
                    if let Some((wire, _peer)) = connection {
                        assert_eq!(wire.lease(lease_deadline()).is_ok(), valid, "body completion did not fence ownership");
                    }
                    assert_eq!(sink.into_recovery().is_ok(), recoverable);
                }
            }
        }
    }

    #[test]
    fn solicited_early_identity_prevents_body_forwarding_in_both_adapters() {
        let cases = [
            (r#"{"id":2,"result":{"poison":true}}"#, false),
            (r#"{"id":"1","result":{"poison":true}}"#, false),
            (r#"{"id":null,"result":{"poison":true}}"#, false),
            (r#"{"result":{"poison":true},"id":1}"#, false),
            (
                r#"{"method":"native/request","id":1,"result":{"poison":true}}"#,
                false,
            ),
            (r#"{"id":1,"result":{},"id":2}"#, true),
            (r#"{"id":1,"result":{},"method":"late"}"#, true),
            (r#"{"id":1,"result":{}}trailing"#, true),
        ];
        for (body, provisional) in cases {
            for duplex in [false, true] {
                let mut sink = Vec::new();
                let outcome = if duplex {
                    let (wire, mut peer, _) = lease_peer();
                    let lease = wire.lease(lease_deadline()).unwrap();
                    lease
                        .send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":true}}))
                        .unwrap();
                    let mut input = frame(true, 1, body.as_bytes());
                    input.extend(frame(true, 1, br#"{"id":1,"result":{}}"#));
                    peer.write_all(&input).unwrap();
                    peer.shutdown(Shutdown::Write).unwrap();
                    lease.receive(1, &mut sink)
                } else {
                    let mut input = frame(true, 1, body.as_bytes());
                    input.extend(frame(true, 1, br#"{"id":1,"result":{}}"#));
                    let mut rpc = JsonRpcConnection::new(Duplex::with_input(input));
                    rpc.request_solicited(
                        "thread/read",
                        serde_json::json!({"includeTurns":true}),
                        &mut sink,
                    )
                };
                if body.starts_with("{\"result\":") {
                    assert!(
                        outcome.is_ok(),
                        "small bounded result before id: {outcome:?}"
                    );
                    assert_eq!(sink, br#"{"poison":true}"#);
                } else if body.contains("native/request") {
                    assert!(
                        outcome.is_ok(),
                        "native request remains queued: {outcome:?}"
                    );
                    assert_eq!(sink, b"{}");
                } else {
                    assert!(outcome.is_err(), "{body}");
                    if !provisional {
                        assert!(
                            sink.is_empty(),
                            "ineligible body reached application sink: {body}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn solicited_matching_history_exceeds_ordinary_bound_with_bounded_retention() {
        struct Measured {
            inner: crate::turn::ThreadTurnSink,
            peak: usize,
            forwarded: usize,
        }
        impl SolicitedSink for Measured {
            fn accept(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
                for chunk in bytes.chunks(4096) {
                    self.inner.accept(chunk)?;
                    self.peak = self.peak.max(self.inner.retained_parser_bytes());
                    self.forwarded += chunk.len();
                }
                Ok(())
            }
        }
        for duplex in [false, true] {
            let (socket, mut peer) = UnixStream::pair().unwrap();
            let producer = std::thread::spawn(move || {
                let request: Value =
                    serde_json::from_slice(&read_masked_client_frame(&mut peer)).unwrap();
                assert_eq!(request["method"], "thread/read");
                peer.write_all(&frame(true, 1, br#"{"method":"before"}"#))
                    .unwrap();
                peer.write_all(&frame(
                    true,
                    1,
                    br#"{"method":"native/request","id":1,"params":{}}"#,
                ))
                .unwrap();
                let prefix = br#"{"id":1,"result":{"thread":{"id":"thread-1","turns":[{"id":"decoy","status":"completed","items":[{"type":"agentMessage","text":""#;
                peer.write_all(&long_frame(false, 1, prefix)).unwrap();
                let block = vec![b'x'; 256 * 1024];
                for _ in 0..136 {
                    peer.write_all(&long_frame(false, 0, &block)).unwrap();
                }
                peer.write_all(&frame(true, 9, b"keepalive")).unwrap();
                let suffix = "\"}]},{\"id\":\"wanted\",\"status\":\"completed\",\"items\":[{\"type\":\"agentMessage\",\"phase\":\"final_answer\",\"text\":\"한\"}]}]}}}".as_bytes();
                let split = suffix.iter().position(|byte| *byte >= 128).unwrap() + 1;
                peer.write_all(&long_frame(false, 0, &suffix[..split]))
                    .unwrap();
                peer.write_all(&long_frame(true, 0, &suffix[split..]))
                    .unwrap();
                peer.write_all(&frame(true, 1, br#"{"method":"after"}"#))
                    .unwrap();
                assert_eq!(read_masked_client_frame(&mut peer), b"keepalive");
            });
            let mut sink = Measured {
                inner: crate::turn::ThreadTurnSink::new("wanted"),
                peak: 0,
                forwarded: 0,
            };
            if duplex {
                let wire = DuplexConnection::attach(socket).unwrap();
                let lease = wire
                    .lease(lease_deadline() + Duration::from_secs(30))
                    .unwrap();
                lease.send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":true}})).unwrap();
                lease.receive(1, &mut sink).unwrap();
                assert_eq!(wire.queued.lock().unwrap().len(), 2);
                assert_eq!(wire.take_queued().unwrap()["method"], "before");
                assert_eq!(wire.take_queued().unwrap()["method"], "native/request");
                drop(lease);
                wire.receive_background(|value| assert_eq!(value.unwrap()["method"], "after"))
                    .unwrap();
            } else {
                let mut rpc = JsonRpcConnection::new(socket);
                rpc.request_solicited(
                    "thread/read",
                    serde_json::json!({"includeTurns":true}),
                    &mut sink,
                )
                .unwrap();
                assert_eq!(rpc.notifications.len(), 2);
                assert_eq!(rpc.next_message().unwrap()["method"], "before");
                assert_eq!(rpc.next_message().unwrap()["method"], "native/request");
                assert_eq!(rpc.next_message().unwrap()["method"], "after");
            }
            producer.join().unwrap();
            assert!(sink.forwarded > MAX_WEBSOCKET_MESSAGE_BYTES);
            assert!(
                sink.peak < 16 * 1024,
                "unrelated history retained {} bytes",
                sink.peak
            );
            println!(
                "duplex={duplex} forwarded={} retained_parser_peak={} queue_peak=2",
                sink.forwarded, sink.peak
            );
            let items = sink.inner.into_items().unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0]["text"], "한");
        }
    }

    #[test]
    fn solicited_ineligible_large_body_never_gets_the_streaming_exception() {
        for duplex in [false, true] {
            for prefix in [
                br#"{"id":2,"result":""#.as_slice(),
                br#"{"method":"native/request","id":1,"result":""#,
            ] {
                let (socket, mut peer) = UnixStream::pair().unwrap();
                let producer = std::thread::spawn(move || {
                    read_masked_client_frame(&mut peer);
                    let mut written = 0;
                    if peer.write_all(&long_frame(false, 1, prefix)).is_ok() {
                        let block = vec![b'x'; 256 * 1024];
                        for _ in 0..140 {
                            if peer.write_all(&long_frame(false, 0, &block)).is_err() {
                                break;
                            }
                            written += block.len();
                        }
                    }
                    let _ = peer.write_all(&frame(true, 0, br#""}"#));
                    let _ = peer.write_all(&frame(true, 1, br#"{"id":1,"result":{}}"#));
                    written
                });
                let mut sink = Vec::new();
                if duplex {
                    let wire = DuplexConnection::attach(socket).unwrap();
                    let lease = wire
                        .lease(lease_deadline() + Duration::from_secs(30))
                        .unwrap();
                    lease
                        .send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":true}}))
                        .unwrap();
                    assert!(lease.receive(1, &mut sink).is_err());
                    wire.shutdown();
                } else {
                    let mut rpc = JsonRpcConnection::new(socket);
                    assert!(
                        rpc.request_solicited(
                            "thread/read",
                            serde_json::json!({"includeTurns":true}),
                            &mut sink
                        )
                        .is_err()
                    );
                }
                assert!(sink.is_empty(), "known ineligible body was forwarded");
                let written = producer.join().unwrap();
                println!("duplex={duplex} peer_payload_written={written} application_forwarded=0");
            }
        }
    }

    #[test]
    fn history_deadline_bounds_trickling_headers_payloads_and_sink_processing() {
        for case in ["trickle", "header", "payload", "sink"] {
            let (stream, mut peer) = UnixStream::pair().unwrap();
            peer.set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let wire =
                crate::darwin::DeadlineStream::from_connected(stream, lease_deadline()).unwrap();
            let mut connection = JsonRpcConnection::new(wire);
            let server = std::thread::spawn(move || {
                read_masked_client_frame(&mut peer);
                let response = frame(true, 1, br#"{"id":1,"result":{"ok":true}}"#);
                match case {
                    "trickle" => {
                        for byte in response {
                            if peer.write_all(&[byte]).is_err() {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(15));
                        }
                    }
                    "header" | "payload" => {
                        let count = if case == "header" {
                            1
                        } else {
                            response.len() - 2
                        };
                        peer.write_all(&response[..count]).unwrap();
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    "sink" => peer.write_all(&response).unwrap(),
                    _ => unreachable!(),
                }
            });
            struct Sink(bool);
            impl SolicitedSink for Sink {
                fn accept(&mut self, _: &[u8]) -> Result<(), TransportError> {
                    if self.0 {
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    Ok(())
                }
            }
            let started = std::time::Instant::now();
            let result = connection.request_history(
                serde_json::json!({"threadId":"original","includeTurns":true}),
                Duration::from_millis(100),
                &mut Sink(case == "sink"),
            );
            assert!(
                matches!(result, Err(TransportError::Io(_))),
                "{case}: {result:?}"
            );
            assert!(started.elapsed() < Duration::from_secs(2), "{case}");
            drop(connection);
            server.join().unwrap();
        }
    }

    #[test]
    fn history_deadline_is_renewed_for_each_request_and_bounds_blocked_writes() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let wire = crate::darwin::DeadlineStream::from_connected(stream, lease_deadline()).unwrap();
        let mut connection = JsonRpcConnection::new(wire);
        let server = std::thread::spawn(move || {
            for id in 1..=2 {
                let request: Value =
                    serde_json::from_slice(&read_masked_client_frame(&mut peer)).unwrap();
                assert_eq!(request["id"], id);
                peer.write_all(&frame(
                    true,
                    1,
                    format!("{{\"id\":{id},\"result\":{{\"ok\":true}}}}").as_bytes(),
                ))
                .unwrap();
            }
        });
        for request in 0..2 {
            let mut sink = Vec::new();
            connection
                .request_history(
                    serde_json::json!({"threadId":"original","includeTurns":true}),
                    Duration::from_millis(100),
                    &mut sink,
                )
                .unwrap();
            assert_eq!(sink, br#"{"ok":true}"#);
            if request == 0 {
                std::thread::sleep(Duration::from_millis(150));
            }
        }
        server.join().unwrap();

        let (stream, _not_reading) = UnixStream::pair().unwrap();
        let wire = crate::darwin::DeadlineStream::from_connected(stream, lease_deadline()).unwrap();
        let mut connection = JsonRpcConnection::new(wire);
        let started = std::time::Instant::now();
        assert!(matches!(
            connection.request_history(
                serde_json::json!({"threadId":"x".repeat(1024*1024),"includeTurns":true}),
                Duration::from_millis(100),
                &mut Vec::new(),
            ),
            Err(TransportError::Io(_))
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn read_lease_cancel_after_send_and_expired_before_send_are_distinct() {
        let (wire, mut peer, _) = lease_peer();
        assert!(wire.lease(std::time::Instant::now()).is_err());
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        peer.set_nonblocking(false).unwrap();
        let lease = wire.lease(lease_deadline()).unwrap();
        lease
            .send(&serde_json::json!({"id":1,"method":"thread/read"}))
            .unwrap();
        read_masked_client_frame(&mut peer);
        drop(lease);
        assert!(wire.ownership.lock().unwrap().closed);
        assert!(wire.lease(lease_deadline()).is_err());
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).unwrap();
        assert!(bytes.is_empty());
        for duplex in [false, true] {
            let mut sink = Vec::new();
            if duplex {
                let (wire, mut peer, _) = lease_peer();
                let lease = wire.lease(lease_deadline()).unwrap();
                lease
                    .send(&serde_json::json!({"id":1,"method":"other"}))
                    .unwrap();
                peer.write_all(&frame(true, 1, br#"{"id":1,"result":{}}"#))
                    .unwrap();
                assert!(lease.receive(1, &mut sink).is_ok());
            } else {
                let mut rpc = JsonRpcConnection::new(Duplex::with_input(frame(
                    true,
                    1,
                    br#"{"id":1,"result":{}}"#,
                )));
                assert!(
                    rpc.request_solicited("other", serde_json::json!({}), &mut sink)
                        .is_ok()
                );
            }
            assert_eq!(sink, b"{}");
        }
    }

    #[test]
    fn read_lease_deadline_covers_sink_processing_and_eof() {
        struct WaitForDeadline(std::time::Instant);
        impl SolicitedSink for WaitForDeadline {
            fn accept(&mut self, _: &[u8]) -> Result<(), TransportError> {
                let (_sender, receiver) = std::sync::mpsc::channel::<()>();
                assert!(
                    receiver
                        .recv_timeout(self.0.saturating_duration_since(std::time::Instant::now()))
                        .is_err()
                );
                Ok(())
            }
        }
        for (eof, eligible) in [(false, false), (false, true), (true, true)] {
            let (wire, mut peer, _) = lease_peer();
            let deadline = std::time::Instant::now() + Duration::from_millis(100);
            let lease = wire.lease(deadline).unwrap();
            assert_eq!(
                lease.deadline, deadline,
                "handoff reset the operation budget"
            );
            lease
                .send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":eligible}}))
                .unwrap();
            if eof {
                peer.shutdown(Shutdown::Write).unwrap();
            } else {
                peer.write_all(&frame(true, 1, br#"{"id":1,"result":{}}"#))
                    .unwrap();
            }
            assert!(lease.receive(1, &mut WaitForDeadline(deadline)).is_err());
            // Bounded replies were validated before sink processing. An
            // expired streamed reply has no budget left to certify its body.
            assert_eq!(wire.ownership.lock().unwrap().closed, eof || eligible);
        }
    }

    #[test]
    fn solicited_request_shape_keeps_ordinary_small_results_and_bounds_nonhistory() {
        for params in [
            serde_json::json!({}),
            serde_json::json!({"includeTurns":false}),
            serde_json::json!({"includeTurns":null}),
            serde_json::json!({"includeTurns":"true"}),
            Value::Null,
        ] {
            for duplex in [false, true] {
                let mut sink = Vec::new();
                let raw = br#"{"id":1,"result":{"raw":1e0}}"#;
                if duplex {
                    let (wire, mut peer, _) = lease_peer();
                    let lease = wire.lease(lease_deadline()).unwrap();
                    lease
                        .send(&serde_json::json!({"id":1,"method":"thread/read","params":params}))
                        .unwrap();
                    peer.write_all(&frame(true, 1, raw)).unwrap();
                    lease.receive(1, &mut sink).unwrap();
                } else {
                    let mut rpc = JsonRpcConnection::new(Duplex::with_input(frame(true, 1, raw)));
                    rpc.request_solicited("thread/read", params.clone(), &mut sink)
                        .unwrap();
                }
                assert_eq!(
                    sink, br#"{"raw":1e0}"#,
                    "bounded forwarding normalized original proof bytes"
                );
            }
        }
        for duplex in [false, true] {
            for params in [
                serde_json::json!({}),
                serde_json::json!({"includeTurns":false}),
            ] {
                let (socket, mut peer) = UnixStream::pair().unwrap();
                let producer = std::thread::spawn(move || {
                    read_masked_client_frame(&mut peer);
                    peer.write_all(&long_frame(false, 1, br#"{"id":1,"result":""#))
                        .unwrap();
                    let block = vec![b'x'; 256 * 1024];
                    for _ in 0..140 {
                        if peer.write_all(&long_frame(false, 0, &block)).is_err() {
                            break;
                        }
                    }
                    let _ = peer.write_all(&frame(true, 0, br#""}"#));
                });
                let mut sink = Vec::new();
                if duplex {
                    let wire = DuplexConnection::attach(socket).unwrap();
                    let lease = wire
                        .lease(lease_deadline() + Duration::from_secs(30))
                        .unwrap();
                    lease
                        .send(&serde_json::json!({"id":1,"method":"thread/read","params":params}))
                        .unwrap();
                    assert!(lease.receive(1, &mut sink).is_err());
                    wire.shutdown();
                } else {
                    let mut rpc = JsonRpcConnection::new(socket);
                    assert!(
                        rpc.request_solicited("thread/read", params, &mut sink)
                            .is_err()
                    );
                }
                assert!(sink.is_empty(), "noneligible oversized result reached sink");
                producer.join().unwrap();
            }
        }
    }

    #[test]
    fn read_lease_rejects_invalid_ack_states_without_dispatch() {
        for field in 0..4 {
            let (wire, mut peer, _) = lease_peer();
            let mut lease = wire.lease(lease_deadline()).unwrap();
            {
                let mut state = wire.ownership.lock().unwrap();
                match field {
                    0 => state.held = false,
                    1 => state.background = true,
                    2 => state.closed = true,
                    _ => {
                        lease.ticket.connection = Uuid::now_v7();
                        state.ticket = Some(lease.ticket);
                    }
                }
            }
            assert!(
                lease
                    .send(&serde_json::json!({"id":1,"method":"thread/read"}))
                    .is_err()
            );
            peer.set_nonblocking(true).unwrap();
            assert_eq!(
                peer.read(&mut [0]).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            // Restore only test-injected impossible state before returning the owner.
            let mut state = wire.ownership.lock().unwrap();
            state.held = true;
            state.background = false;
            state.closed = false;
            lease.ticket.connection = wire.identity;
            state.ticket = Some(lease.ticket);
            drop(state);
            drop(lease);
        }
    }

    #[test]
    fn read_lease_return_allows_exactly_one_next_owner() {
        let (wire, _peer, _) = lease_peer();
        let first = wire.lease(lease_deadline()).unwrap();
        let ticket = first.ticket;
        assert!(
            wire.lease(lease_deadline()).is_err(),
            "two simultaneous lease owners"
        );
        drop(first);
        let second = wire.lease(lease_deadline()).unwrap();
        assert_ne!(second.ticket, ticket);
        assert_eq!(second.ticket.connection, ticket.connection);
        assert!(second.validate().is_ok());
        drop(second);
        assert!(wire.ownership.lock().unwrap().ticket.is_none());
    }

    #[test]
    fn solicited_id_value_availability_and_classification_window_edges() {
        struct Counted(UnixStream, Arc<std::sync::atomic::AtomicUsize>);
        impl Read for Counted {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.read(bytes)?;
                self.1.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                Ok(n)
            }
        }
        impl Write for Counted {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.0.flush()
            }
        }
        let window = EARLY_TOP_LEVEL_ID_PREFIX_BYTES;
        // Prefix lengths include the delimiter needed to disambiguate a number.
        for duplex in [false, true] {
            for mode in 0..8 {
                let (socket, mut peer) = UnixStream::pair().unwrap();
                let available = if mode < 3 { window - 1 + mode } else { 8 };
                let producer = std::thread::spawn(move || {
                    read_masked_client_frame(&mut peer);
                    let mut send = |final_frame, opcode, bytes: &[u8]| {
                        peer.write_all(&long_frame(final_frame, opcode, bytes))
                    };
                    let outcome = (|| -> std::io::Result<()> {
                        if mode < 3 {
                            send(false, 1, br#"{"id":"#)?;
                            let spaces = vec![b' '; 64 * 1024];
                            let count = available - 8;
                            for part in (0..count).step_by(spaces.len()) {
                                send(false, 0, &spaces[..(count - part).min(spaces.len())])?;
                            }
                            if mode == 0 {
                                send(true, 0, br#"1,"result":{}}"#)?;
                            } else {
                                send(false, 0, br#"1,"result":{"poison":""#)?;
                                let body = vec![b'x'; 256 * 1024];
                                for _ in 0..140 {
                                    send(false, 0, &body)?;
                                }
                                send(true, 0, br#""}}"#)?;
                            }
                        } else {
                            send(
                                false,
                                1,
                                match mode {
                                    3 | 6 => br#"{"id":1,"#,
                                    7 => br#"{"method":"ordinary","#,
                                    5 => br#"{"method":"ordinary","padding":""#,
                                    _ => br#"{"id":1,"padding":""#,
                                },
                            )?;
                            let outer_gap = matches!(mode, 3 | 6 | 7);
                            let payload = vec![if outer_gap { b' ' } else { b'x' }; 256 * 1024];
                            for _ in 0..if matches!(mode, 6 | 7) {
                                256
                            } else if outer_gap {
                                136
                            } else {
                                68
                            } {
                                send(false, 0, &payload)?;
                            }
                            send(
                                true,
                                0,
                                match mode {
                                    3 | 6 => br#""metadata":true,"result":{}}"#,
                                    7 => br#""id":1,"params":{}}"#,
                                    5 => br#"","id":1,"params":{}}"#,
                                    _ => br#"","result":{}}"#,
                                },
                            )?;
                            if mode == 5 || mode == 7 {
                                send(true, 1, br#"{"id":1,"result":{}}"#)?;
                            }
                        }
                        Ok(())
                    })();
                    // Early refusal deliberately closes a producer still sending.
                    outcome.is_ok()
                });
                let mut sink = Vec::new();
                let received = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let accepted = if duplex {
                    let wire = DuplexConnection::attach(socket).unwrap();
                    let progress = wire.observe_reads();
                    let lease = wire
                        .lease(lease_deadline() + Duration::from_secs(30))
                        .unwrap();
                    lease.send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":mode!=6}})).unwrap();
                    let result = lease.receive(1, &mut sink);
                    received.store(
                        progress.try_iter().sum(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    if mode == 5 && result.is_ok() {
                        assert_eq!(wire.queued.lock().unwrap().len(), 1);
                        assert_eq!(wire.take_queued().unwrap()["method"], "ordinary");
                    }
                    wire.shutdown();
                    result.is_ok()
                } else {
                    let mut rpc = JsonRpcConnection::new(Counted(socket, Arc::clone(&received)));
                    let result = rpc.request_solicited(
                        "thread/read",
                        serde_json::json!({"includeTurns":mode!=6}),
                        &mut sink,
                    );
                    if mode == 5 && result.is_ok() {
                        assert_eq!(rpc.notifications.len(), 1);
                        assert_eq!(rpc.next_message().unwrap()["method"], "ordinary");
                    }
                    if result.is_ok() {
                        assert_eq!(rpc.last_response_id_offset(), Some(2));
                    }
                    result.is_ok()
                };
                producer.join().unwrap();
                let received = received.load(std::sync::atomic::Ordering::Relaxed);
                if matches!(mode, 1 | 2) {
                    assert!(
                        received < window + MAX_WEBSOCKET_FRAME_BYTES,
                        "ambiguous prefix consumed body: {received}"
                    );
                }
                if matches!(mode, 6 | 7) {
                    assert!(
                        received < MAX_WEBSOCKET_MESSAGE_BYTES + MAX_WEBSOCKET_FRAME_BYTES,
                        "ordinary raw oversize consumed whole compactable input: {received}"
                    );
                }
                println!(
                    "window duplex={duplex} mode={mode} received={received} accepted={accepted}"
                );
                assert_eq!(
                    accepted,
                    mode == 0 || matches!(mode, 3..=5),
                    "duplex={duplex} mode={mode} available={available}"
                );
                assert_eq!(
                    sink,
                    if accepted {
                        b"{}".as_slice()
                    } else {
                        b"".as_slice()
                    }
                );
            }
        }
        let mut scanner = TopLevelStreamScanner::matching(1, true);
        scanner.feed(br#"{"id":1,"#, &mut Vec::new()).unwrap();
        assert_eq!(scanner.id_offset(), Some(2));
        assert_eq!(scanner.id_available_at(), Some(8));
        let spaces = vec![b' '; 64 * 1024];
        for _ in 0..260 {
            scanner.feed(&spaces, &mut Vec::new()).unwrap();
        }
        scanner
            .feed(br#""metadata":true,"result":{}}"#, &mut Vec::new())
            .unwrap();
        assert!(scanner.correlatable());
        assert!(
            scanner.finish().unwrap().len() < 128,
            "outer whitespace was retained"
        );
    }

    #[test]
    fn solicited_id_availability_preserves_existing_jcs_numeric_values() {
        for raw in ["1", "1.0", "1e0", &format!("1.{}", "0".repeat(256))] {
            for eligible in [false, true] {
                for duplex in [false, true] {
                    let bytes = format!("{{\"id\":{raw},\"result\":{{}}}}");
                    let mut sink = Vec::new();
                    if duplex {
                        let (wire, mut peer, _) = lease_peer();
                        let lease = wire.lease(lease_deadline()).unwrap();
                        lease.send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":eligible}})).unwrap();
                        peer.write_all(&long_frame(true, 1, bytes.as_bytes()))
                            .unwrap();
                        lease.receive(1, &mut sink).unwrap();
                    } else {
                        let mut rpc = JsonRpcConnection::new(Duplex::with_input(long_frame(
                            true,
                            1,
                            bytes.as_bytes(),
                        )));
                        rpc.request_solicited(
                            "thread/read",
                            serde_json::json!({"includeTurns":eligible}),
                            &mut sink,
                        )
                        .unwrap();
                    }
                    assert_eq!(sink, b"{}");
                }
            }
        }
    }

    #[test]
    fn read_lease_invalid_wire_completion_never_returns_reader_ownership() {
        let mut nested = frame(false, 1, br#"{"id":1,"result":"#);
        nested.extend(frame(true, 1, br#"{}}"#));
        let mut invalid = br#"{"id":1,"result":{}}"#.to_vec();
        invalid.push(0xff);
        let mut incomplete = br#"{"id":1,"result":{}}"#.to_vec();
        incomplete.push(0xe1);
        for input in [
            nested,
            frame(true, 0, br#"{"id":1,"result":{}}"#),
            frame(true, 1, &invalid),
            frame(true, 1, &incomplete),
        ] {
            let (wire, mut peer, _) = lease_peer();
            let lease = wire.lease(lease_deadline()).unwrap();
            lease.send(&serde_json::json!({"id":1,"method":"thread/read","params":{"includeTurns":true}})).unwrap();
            peer.write_all(&input).unwrap();
            assert!(lease.receive(1, &mut Vec::new()).is_err());
            assert!(
                wire.ownership.lock().unwrap().closed,
                "invalid final opcode/UTF8 cannot return a healthy read half"
            );
            drop(lease);
            assert!(wire.lease(lease_deadline()).is_err());
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
            .request_solicited(
                "thread/read",
                serde_json::json!({"includeTurns":true}),
                &mut streamed,
            )
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

    pub(crate) fn read_masked_client_frame(stream: &mut UnixStream) -> Vec<u8> {
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
