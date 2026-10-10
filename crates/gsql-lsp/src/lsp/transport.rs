//! JSON-RPC message framing over a byte stream (the LSP base protocol).

use std::io::{self, BufRead, Read, Write};

use serde_json::{Value, json};

/// A decoded JSON-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<Value>,
    },
    /// Not a JSON-RPC message; the client gets an error response (-32700
    /// for broken JSON, -32600 for JSON that is not a request).
    Invalid {
        id: Value,
        code: i64,
        message: String,
    },
}

impl Message {
    pub fn from_value(value: Value) -> io::Result<Message> {
        let Value::Object(mut map) = value else {
            return Err(invalid_data("JSON-RPC message must be an object"));
        };
        let params = map.remove("params").unwrap_or(Value::Null);
        match (map.remove("id"), map.remove("method")) {
            (Some(id), Some(Value::String(method))) => {
                Ok(Message::Request { id, method, params })
            }
            (None, Some(Value::String(method))) => {
                Ok(Message::Notification { method, params })
            }
            (Some(id), None) => Ok(Message::Response {
                id,
                result: map.remove("result"),
                error: map.remove("error"),
            }),
            _ => Err(invalid_data(
                "JSON-RPC message has neither a method nor an id",
            )),
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Message::Request { id, method, params } => {
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
            }
            Message::Notification { method, params } => {
                json!({ "jsonrpc": "2.0", "method": method, "params": params })
            }
            Message::Invalid { id, code, message } => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
            Message::Response { id, result, error } => {
                let mut value = json!({ "jsonrpc": "2.0", "id": id });
                if let Some(error) = error {
                    value["error"] = error.clone();
                } else {
                    value["result"] = result.clone().unwrap_or(Value::Null);
                }
                value
            }
        }
    }
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// The largest message body accepted (a 5 MB document is about 7 MB of JSON).
const MAX_BODY_BYTES: usize = 256 << 20;

/// A broken header cannot be skipped: the body boundary is lost, so the rest
/// of the stream would be read from the middle of a message. (`InvalidData`,
/// a bad body, can be skipped.)
fn framing_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

/// Reads one message. Returns `Ok(None)` at end of input.
pub fn read_message(
    reader: &mut impl BufRead,
) -> io::Result<Option<Message>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if content_length.is_some() {
                break;
            }
            // Tolerate stray blank lines between messages.
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name
                .trim()
                .eq_ignore_ascii_case("content-length")
        {
            let length: usize = value.trim().parse().map_err(|_| {
                framing_error("invalid Content-Length header")
            })?;
            if length > MAX_BODY_BYTES {
                return Err(framing_error("Content-Length is too large"));
            }
            content_length = Some(length);
        }
    }
    let length = content_length.unwrap_or_default();
    // Not `vec![0; length]`: a lying header must not allocate what never arrives.
    let mut body = Vec::new();
    reader
        .by_ref()
        .take(length as u64)
        .read_to_end(&mut body)?;
    if body.len() < length {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return Ok(Some(Message::Invalid {
                id: Value::Null,
                code: -32700,
                message: format!("invalid JSON: {err}"),
            }));
        }
    };
    let id = value
        .get("id")
        .cloned()
        .unwrap_or(Value::Null);
    Ok(Some(match Message::from_value(value) {
        Ok(message) => message,
        Err(err) => Message::Invalid {
            id,
            code: -32600,
            message: err.to_string(),
        },
    }))
}

/// Writes one message with a `Content-Length` header.
pub fn write_message(
    writer: &mut impl Write,
    message: &Message,
) -> io::Result<()> {
    let body = serde_json::to_string(&message.to_value())?;
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trips_messages() {
        let message = Message::Request {
            id: json!(1),
            method: "initialize".into(),
            params: json!({ "rootUri": null }),
        };
        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).unwrap();
        let mut reader = Cursor::new(buffer);
        assert_eq!(read_message(&mut reader).unwrap(), Some(message));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn bad_headers_are_fatal_and_bad_bodies_are_not() {
        for header in [
            "Content-Length: abc",
            "Content-Length: 99999999999999999999",
            "Content-Length: 9999999999999",
        ] {
            let mut reader =
                Cursor::new(format!("{header}\r\n\r\n{{}}").into_bytes());
            let err = read_message(&mut reader).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{header}");
        }
        // A bad body is answered with an error, and the stream goes on.
        let mut reader =
            Cursor::new(b"Content-Length: 3\r\n\r\nabc".to_vec());
        assert!(matches!(
            read_message(&mut reader).unwrap(),
            Some(Message::Invalid { code: -32700, .. })
        ));
        let body = r#"{"jsonrpc":"2.0","id":7,"method":5}"#;
        let mut reader = Cursor::new(
            format!("Content-Length: {}\r\n\r\n{body}", body.len())
                .into_bytes(),
        );
        match read_message(&mut reader).unwrap() {
            Some(Message::Invalid {
                id, code: -32600, ..
            }) => assert_eq!(id, json!(7)),
            other => panic!("{other:?}"),
        }
        // A body shorter than announced ends the stream instead of allocating.
        let mut reader =
            Cursor::new(b"Content-Length: 200000000\r\n\r\n{}".to_vec());
        assert_eq!(
            read_message(&mut reader).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn classifies_messages() {
        let notification = Message::from_value(
            json!({ "jsonrpc": "2.0", "method": "exit" }),
        )
        .unwrap();
        assert!(matches!(notification, Message::Notification { .. }));
        let response = Message::from_value(
            json!({ "jsonrpc": "2.0", "id": 3, "result": null }),
        )
        .unwrap();
        assert!(matches!(response, Message::Response { .. }));
    }

    #[test]
    fn handles_multibyte_bodies() {
        let message = Message::Notification {
            method: "x".into(),
            params: json!({ "text": "héllo — 世界" }),
        };
        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).unwrap();
        let mut reader = Cursor::new(buffer);
        assert_eq!(read_message(&mut reader).unwrap(), Some(message));
    }
}
