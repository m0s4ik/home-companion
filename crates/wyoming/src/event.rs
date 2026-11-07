// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Framing, exactly as `wyoming/event.py` (1.11.0) reads and writes it.
//!
//! A frame is one line of JSON terminated by `\n`:
//! `{"type": str, "version": str, "data": {...}?, "data_length": n?, "payload_length": n?}`,
//! then `data_length` bytes of JSON merged into `data` (Python `dict.update`, so the block wins
//! per key), then `payload_length` bytes of payload.
//!
//! The writer does what `async_write_event` does: `data` goes in a separate block announced by
//! `data_length` (never inline, and not at all when empty), `payload_length` only when there is a
//! non-empty payload. JSON is written with Python's `json.dumps` separators (`", "` and `": "`)
//! and without ASCII escaping, so a header line is byte-identical to Python's.

use std::cell::RefCell;
use std::io::{BufRead, Write};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::{Error, MAX_FRAME, MAX_HEADER, PROTOCOL_VERSION};

/// One Wyoming event, untyped.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Event {
    /// The `type` field (`"audio-chunk"`, ...).
    pub kind: String,
    /// `data`, after merging the data block.
    pub data: Map<String, Value>,
    /// The binary payload; `None` when `payload_length` is absent or 0.
    pub payload: Option<Vec<u8>>,
}

impl Event {
    /// An event with no data and no payload.
    pub fn new(kind: impl Into<String>) -> Event {
        Event {
            kind: kind.into(),
            data: Map::new(),
            payload: None,
        }
    }

    /// An event with data and no payload.
    pub fn with_data(kind: impl Into<String>, data: Map<String, Value>) -> Event {
        Event {
            kind: kind.into(),
            data,
            payload: None,
        }
    }
}

/// Reads one event; `Ok(None)` at EOF on a frame boundary. Limits: 1 MiB header, 16 MiB data and
/// payload.
pub fn read_event(r: &mut impl BufRead) -> Result<Option<Event>, Error> {
    read_event_limited(r, MAX_FRAME)
}

/// As [`read_event`] with an explicit limit for `data_length` and `payload_length`.
pub fn read_event_limited(r: &mut impl BufRead, max_frame: usize) -> Result<Option<Event>, Error> {
    let mut line = Vec::new();
    if !read_line(r, &mut line)? {
        return Ok(None);
    }
    let header: Value = serde_json::from_slice(&line)
        .map_err(|e| Error::Frame(format!("header is not JSON: {e}")))?;
    let Value::Object(mut header) = header else {
        return Err(Error::Frame("header is not a JSON object".into()));
    };
    let kind = match header.remove("type") {
        Some(Value::String(s)) => s,
        Some(_) => return Err(Error::Frame("\"type\" is not a string".into())),
        None => return Err(Error::Frame("header without \"type\"".into())),
    };
    let mut data = match header.remove("data") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => return Err(Error::Frame("\"data\" is not an object".into())),
    };
    let data_length = length(&header, "data_length", max_frame)?;
    let payload_length = length(&header, "payload_length", max_frame)?;

    if data_length > 0 {
        let mut block = vec![0u8; data_length];
        read_exact(r, &mut block)?;
        let block: Value = serde_json::from_slice(&block)
            .map_err(|e| Error::Frame(format!("data block is not JSON: {e}")))?;
        match block {
            Value::Object(m) => data.extend(m),
            Value::Null => {}
            _ => return Err(Error::Frame("data block is not an object".into())),
        }
    }
    let payload = if payload_length > 0 {
        let mut p = vec![0u8; payload_length];
        read_exact(r, &mut p)?;
        Some(p)
    } else {
        None
    };
    Ok(Some(Event {
        kind,
        data,
        payload,
    }))
}

/// Reads a line into `line` (without `\n`). `Ok(false)` at EOF before any byte.
fn read_line(r: &mut impl BufRead, line: &mut Vec<u8>) -> Result<bool, Error> {
    loop {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        if buf.is_empty() {
            if line.is_empty() {
                return Ok(false);
            }
            return Err(Error::Frame("truncated".into()));
        }
        match buf.iter().position(|&b| b == b'\n') {
            Some(i) => {
                line.extend_from_slice(&buf[..i]);
                r.consume(i + 1);
                if line.len() > MAX_HEADER {
                    return Err(Error::Frame("header line over 1 MiB".into()));
                }
                return Ok(true);
            }
            None => {
                let n = buf.len();
                line.extend_from_slice(buf);
                r.consume(n);
                if line.len() > MAX_HEADER {
                    return Err(Error::Frame("header line over 1 MiB".into()));
                }
            }
        }
    }
}

fn read_exact(r: &mut impl BufRead, buf: &mut [u8]) -> Result<(), Error> {
    r.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Frame("truncated".into())
        } else {
            Error::Io(e)
        }
    })
}

fn length(header: &Map<String, Value>, key: &str, max: usize) -> Result<usize, Error> {
    match header.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => {
            if let Some(v) = n.as_u64() {
                if v > max as u64 {
                    Err(Error::Frame(format!(
                        "{key} {v} over the limit of {max} bytes"
                    )))
                } else {
                    Ok(v as usize)
                }
            } else if n.as_i64().is_some_and(|v| v < 0) {
                Err(Error::Frame(format!("{key} is negative")))
            } else {
                Err(Error::Frame(format!("{key} is not an integer")))
            }
        }
        Some(_) => Err(Error::Frame(format!("{key} is not a number"))),
    }
}

/// Writes one event (one `write_all` of header, data and payload, then `flush`).
pub fn write_event(w: &mut impl Write, e: &Event) -> Result<(), Error> {
    with_buffers(|buf, data| {
        encode_event(buf, data, &e.kind, &e.data, e.payload.as_deref());
        w.write_all(buf)?;
        w.flush()?;
        Ok(())
    })
}

thread_local! {
    static BUFFERS: RefCell<(Vec<u8>, Vec<u8>)> = const { RefCell::new((Vec::new(), Vec::new())) };
}

/// Runs `f` with two thread-local scratch buffers (frame, data block), cleared. Buffers that grew
/// beyond 1 MiB are released afterwards so one large frame does not pin memory.
pub(crate) fn with_buffers<T>(f: impl FnOnce(&mut Vec<u8>, &mut Vec<u8>) -> T) -> T {
    BUFFERS.with(|cell| match cell.try_borrow_mut() {
        Ok(mut b) => {
            let (frame, data) = &mut *b;
            frame.clear();
            data.clear();
            let out = f(frame, data);
            for v in [frame, data] {
                if v.capacity() > 1024 * 1024 {
                    *v = Vec::new();
                }
            }
            out
        }
        // Re-entrant use (never happens in this crate): fall back to fresh buffers.
        Err(_) => f(&mut Vec::new(), &mut Vec::new()),
    })
}

/// Assembles a frame into `buf`, using `scratch` for the data block.
pub(crate) fn encode_event(
    buf: &mut Vec<u8>,
    scratch: &mut Vec<u8>,
    kind: &str,
    data: &Map<String, Value>,
    payload: Option<&[u8]>,
) {
    scratch.clear();
    if !data.is_empty() {
        write_python_json(scratch, data);
    }
    encode_raw(buf, kind, scratch, payload.unwrap_or(&[]));
}

/// Assembles a frame from an already serialized data block (empty = none) and a payload (empty =
/// none).
pub(crate) fn encode_raw(buf: &mut Vec<u8>, kind: &str, data_block: &[u8], payload: &[u8]) {
    buf.extend_from_slice(b"{\"type\": ");
    write_python_json(buf, kind);
    buf.extend_from_slice(b", \"version\": \"");
    buf.extend_from_slice(PROTOCOL_VERSION.as_bytes());
    buf.push(b'"');
    if !data_block.is_empty() {
        let _ = write!(buf, ", \"data_length\": {}", data_block.len());
    }
    if !payload.is_empty() {
        let _ = write!(buf, ", \"payload_length\": {}", payload.len());
    }
    buf.extend_from_slice(b"}\n");
    buf.extend_from_slice(data_block);
    buf.extend_from_slice(payload);
}

/// Serializes `v` as Python's `json.dumps(v, ensure_ascii=False)` does for the values Wyoming
/// uses (separators `", "` and `": "`).
pub(crate) fn write_python_json<T: Serialize + ?Sized>(out: &mut Vec<u8>, v: &T) {
    let mut ser = serde_json::Serializer::with_formatter(&mut *out, PythonFormatter);
    // Serializing a Map/str/Value into a Vec cannot fail.
    let _ = v.serialize(&mut ser);
}

/// Python's default `json.dumps` separators.
struct PythonFormatter;

impl serde_json::ser::Formatter for PythonFormatter {
    fn begin_array_value<W: ?Sized + Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }
    fn begin_object_key<W: ?Sized + Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }
    fn begin_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b": ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_separators() {
        let mut out = Vec::new();
        let v = serde_json::json!({"a": [1, 2], "b": {"c": null}, "d": "è"});
        write_python_json(&mut out, &v);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            r#"{"a": [1, 2], "b": {"c": null}, "d": "è"}"#
        );
    }

    #[test]
    fn header_shape() {
        let mut buf = Vec::new();
        encode_raw(&mut buf, "audio-chunk", b"{}", &[0; 4]);
        assert!(buf.starts_with(
            b"{\"type\": \"audio-chunk\", \"version\": \"1.11.0\", \"data_length\": 2, \"payload_length\": 4}\n"
        ));
    }
}
