//! The encrypted channel shared by the PC server and the phone client: framing, padding,
//! encryption, and strict envelope parsing in one place.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

use crate::clock::{with_deadline, SyncClock};
use crate::error::{EnvelopeError, FrameError};
use crate::frame::{
    decode_envelope, encode_envelope, FrameReader, MsgType, ENVELOPE_HEADER_LEN, MAX_FRAME_LEN,
};
use crate::message::{ErrorBody, ErrorCode};
use crate::noise::Transport;

/// Why a channel operation failed. `code()` is the only text that reaches logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelError {
    /// Clean end of stream.
    Closed,
    /// The socket failed.
    Io,
    /// Zero-length, oversized or truncated frame.
    Frame,
    /// Authentication failed: close, never resynchronise.
    Decrypt,
    /// The decrypted envelope is malformed.
    Envelope(EnvelopeError),
    /// A body is not valid JSON for its type.
    Json,
    /// A body or message above the size limits.
    TooLarge,
    /// A write did not complete in time.
    WriteTimeout,
}

impl ChannelError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            ChannelError::Closed => "closed",
            ChannelError::Io => "io",
            ChannelError::Frame => "frame",
            ChannelError::Decrypt => "decrypt",
            ChannelError::Envelope(_) => "envelope",
            ChannelError::Json => "json",
            ChannelError::TooLarge => "too_large",
            ChannelError::WriteTimeout => "write_timeout",
        }
    }

    /// The `ERROR` code to send for a failure of the peer's making, if one should be sent.
    /// Decrypt failures and dead sockets get no reply.
    pub(crate) fn reply(self) -> Option<ErrorCode> {
        match self {
            ChannelError::Envelope(_) => Some(ErrorCode::Protocol),
            ChannelError::Json => Some(ErrorCode::Malformed),
            ChannelError::TooLarge => Some(ErrorCode::TooLarge),
            _ => None,
        }
    }
}

impl From<FrameError> for ChannelError {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::Closed => ChannelError::Closed,
            FrameError::Io(_) => ChannelError::Io,
            FrameError::Truncated | FrameError::ZeroLength | FrameError::Oversize => {
                ChannelError::Frame
            }
        }
    }
}

/// One received, authenticated and structurally valid message. It owns the decrypted envelope and
/// points at its body, so a message is never copied a second time.
#[derive(Debug)]
pub(crate) struct Incoming {
    pub(crate) msg_type: MsgType,
    plain: Vec<u8>,
    body_len: usize,
}

impl Incoming {
    /// Parse the body as JSON of type `T`.
    pub(crate) fn json<T: DeserializeOwned>(&self) -> Result<T, ChannelError> {
        let end = ENVELOPE_HEADER_LEN + self.body_len;
        let body = self
            .plain
            .get(ENVELOPE_HEADER_LEN..end)
            .ok_or(ChannelError::Json)?;
        serde_json::from_slice(body).map_err(|_| ChannelError::Json)
    }
}

/// An established Noise transport over a byte stream.
pub(crate) struct Channel<S> {
    reader: FrameReader<ReadHalf<S>>,
    writer: WriteHalf<S>,
    transport: Transport,
    clock: Arc<dyn SyncClock>,
    write_timeout: Duration,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Channel<S> {
    /// Take over the halves used during the handshake.
    pub(crate) fn new(
        mut reader: FrameReader<ReadHalf<S>>,
        writer: WriteHalf<S>,
        transport: Transport,
        clock: Arc<dyn SyncClock>,
        write_timeout: Duration,
    ) -> Self {
        reader.set_max_len(MAX_FRAME_LEN);
        Channel {
            reader,
            writer,
            transport,
            clock,
            write_timeout,
        }
    }

    /// Messages sent plus received, for the lifetime limit.
    pub(crate) fn total_messages(&self) -> u64 {
        self.transport
            .messages_sent()
            .saturating_add(self.transport.messages_received())
    }

    pub(crate) async fn send_json<T: Serialize>(
        &mut self,
        msg_type: MsgType,
        body: &T,
    ) -> Result<(), ChannelError> {
        let bytes = serde_json::to_vec(body).map_err(|_| ChannelError::Json)?;
        self.send_bytes(msg_type, &bytes).await
    }

    pub(crate) async fn send_bytes(
        &mut self,
        msg_type: MsgType,
        body: &[u8],
    ) -> Result<(), ChannelError> {
        // At most two copies of the message exist at once (envelope plaintext, framed ciphertext),
        // so a connection stays within its 128 KiB budget.
        let plain = encode_envelope(msg_type, body).map_err(|_| ChannelError::TooLarge)?;
        let frame = self
            .transport
            .encrypt_framed(&plain)
            .map_err(|_| ChannelError::TooLarge)?;
        drop(plain);
        let writer = &mut self.writer;
        let written = with_deadline(&*self.clock, self.write_timeout, async {
            writer.write_all(&frame).await?;
            writer.flush().await
        })
        .await;
        match written {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(ChannelError::Io),
            Err(_) => Err(ChannelError::WriteTimeout),
        }
    }

    /// Receive the next message. Cancel-safe, so it may sit in `select!`.
    pub(crate) async fn recv(&mut self) -> Result<Incoming, ChannelError> {
        let frame = self.reader.read_frame().await?;
        let plain = self
            .transport
            .decrypt(&frame)
            .map_err(|_| ChannelError::Decrypt)?;
        drop(frame);
        let (msg_type, body) = decode_envelope(&plain).map_err(ChannelError::Envelope)?;
        let body_len = body.len();
        Ok(Incoming {
            msg_type,
            plain,
            body_len,
        })
    }

    /// Best-effort `ERROR` before closing.
    pub(crate) async fn send_error(&mut self, code: ErrorCode) {
        let _ = self.send_json(MsgType::Error, &ErrorBody { code }).await;
    }

    /// Best-effort close.
    pub(crate) async fn shutdown(&mut self) {
        let _ = with_deadline(&*self.clock, Duration::from_secs(1), self.writer.shutdown()).await;
    }
}
