//! Messages between the compositor and its clients. Each message is one frame on an
//! AF_UNIX stream socket: a `u32` little-endian byte length, then a JSON body.
//!
//! After `Response::NewWindow`, the compositor sends one descriptor of the window
//! memory with `SCM_RIGHTS`, attached to the one byte that follows the response.
//! A client takes it with `recvmsg` on that byte; a plain read of the byte closes
//! the descriptor.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::thread::sleep;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const SOCKET_PATH: &str = "/var/run/compositor.sock";

/// [`connect`] tries this many times before it gives up.
const CONNECT_ATTEMPTS: u32 = 50;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Upper bound for one frame body. The reader allocates this many bytes at most.
pub const MAX_MESSAGE_BYTES: u32 = 64 * 1024;

/// Asks for the size of the screen.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfoRequest {}

/// The size of the screen in pixels.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfoResponse {
    pub width: u32,
    pub height: u32,
}

/// The window memory has `height` rows of `width` pixels with no padding
/// between rows. Each pixel is one `u32` in `0xAARRGGBB` form.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewWindowRequest {
    /// The screen column of the left edge of the window, in pixels.
    pub x: u32,
    /// The screen row of the top edge of the window, in pixels.
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewWindowResponse {}

/// Marks a window rectangle as changed. Nothing is shown until the next
/// `Finish`. The part outside the window is ignored.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageRequest {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageResponse {}

/// Ends a frame: every rectangle marked by `Damage` since the last `Finish`
/// reaches the screen before the response. The part of the window outside
/// the screen is not shown.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishRequest {
    /// `fence` is the GPU fence id of the last submit that wrote the window.
    /// `0` means that no GPU work is pending, for example when the client
    /// wrote the window only with the CPU and a completed transfer.
    pub fence: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishResponse {}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    DisplayInfo(DisplayInfoRequest),
    NewWindow(NewWindowRequest),
    Damage(DamageRequest),
    Finish(FinishRequest),
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    DisplayInfo(DisplayInfoResponse),
    NewWindow(NewWindowResponse),
    Damage(DamageResponse),
    Finish(FinishResponse),
}

/// Connects to `SOCKET_PATH`. Tries again for about five seconds, because
/// `services` starts the compositor first but does not wait for its socket.
pub fn connect() -> io::Result<UnixStream> {
    let mut attempt = 1;
    loop {
        match UnixStream::connect(SOCKET_PATH) {
            Ok(stream) => return Ok(stream),
            Err(e) if attempt == CONNECT_ATTEMPTS => return Err(e),
            Err(_) => {
                attempt += 1;
                sleep(CONNECT_RETRY_DELAY);
            }
        }
    }
}

/// Sends `request` and reads the response. The end of the stream before the
/// response is an error.
pub fn call(stream: &UnixStream, request: &Request) -> io::Result<Response> {
    write_message(&mut &*stream, request)?;
    read_message::<Response>(&mut &*stream)?
        .ok_or_else(|| io::Error::other("compositor closed the connection"))
}

/// Writes one frame: a `u32` little-endian byte length, then the JSON body.
/// A body longer than `MAX_MESSAGE_BYTES` returns `io::ErrorKind::InvalidData`.
pub fn write_message<T: Serialize>(writer: &mut impl Write, message: &T) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    let len = u32::try_from(body.len())
        .ok()
        .filter(|&len| len <= MAX_MESSAGE_BYTES)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "message too large"))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&body)
}

/// Reads one frame. Returns `Ok(None)` when the stream ends before the length prefix.
/// A length above `MAX_MESSAGE_BYTES` or a body that is not a valid `T` returns
/// `io::ErrorKind::InvalidData`. A stream that ends inside the frame returns
/// `io::ErrorKind::UnexpectedEof`.
pub fn read_message<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<Option<T>> {
    let mut prefix = [0; 4];
    match reader.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(prefix);
    if len > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut body = vec![0; len as usize];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn frame(len: u32, body: &[u8]) -> Cursor<Vec<u8>> {
        let mut bytes = len.to_le_bytes().to_vec();
        bytes.extend_from_slice(body);
        Cursor::new(bytes)
    }

    #[test]
    fn given_new_window_request_when_written_and_read_then_equal() {
        let request = Request::NewWindow(NewWindowRequest {
            x: 160,
            y: 120,
            width: 256,
            height: 128,
        });
        let mut bytes = Vec::new();
        write_message(&mut bytes, &request).unwrap();

        let read = read_message::<Request>(&mut Cursor::new(bytes)).unwrap();

        assert_eq!(read, Some(request));
    }

    #[test]
    fn given_new_window_response_when_written_and_read_then_equal() {
        let response = Response::NewWindow(NewWindowResponse {});
        let mut bytes = Vec::new();
        write_message(&mut bytes, &response).unwrap();

        let read = read_message::<Response>(&mut Cursor::new(bytes)).unwrap();

        assert_eq!(read, Some(response));
    }

    #[test]
    fn given_display_info_request_when_written_and_read_then_equal() {
        let request = Request::DisplayInfo(DisplayInfoRequest {});
        let mut bytes = Vec::new();
        write_message(&mut bytes, &request).unwrap();

        let read = read_message::<Request>(&mut Cursor::new(bytes)).unwrap();

        assert_eq!(read, Some(request));
    }

    #[test]
    fn given_display_info_response_when_written_and_read_then_equal() {
        let response = Response::DisplayInfo(DisplayInfoResponse {
            width: 800,
            height: 600,
        });
        let mut bytes = Vec::new();
        write_message(&mut bytes, &response).unwrap();

        let read = read_message::<Response>(&mut Cursor::new(bytes)).unwrap();

        assert_eq!(read, Some(response));
    }

    #[test]
    fn given_finish_request_when_written_and_read_then_equal() {
        let request = Request::Finish(FinishRequest { fence: 7 });
        let mut bytes = Vec::new();
        write_message(&mut bytes, &request).unwrap();

        let read = read_message::<Request>(&mut Cursor::new(bytes)).unwrap();

        assert_eq!(read, Some(request));
    }

    #[test]
    fn given_empty_stream_when_read_then_none() {
        let read = read_message::<Request>(&mut Cursor::new(Vec::new())).unwrap();

        assert_eq!(read, None);
    }

    #[test]
    fn given_length_above_max_when_read_then_invalid_data() {
        let err = read_message::<Request>(&mut frame(MAX_MESSAGE_BYTES + 1, b"")).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn given_body_shorter_than_length_when_read_then_unexpected_eof() {
        let err = read_message::<Request>(&mut frame(10, b"abc")).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn given_unknown_variant_when_read_then_invalid_data() {
        let body = br#"{"Unknown":{}}"#;

        let err = read_message::<Request>(&mut frame(body.len() as u32, body)).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
