//! Length-prefixed frames over a byte stream, and the padded envelope inside each Noise message.
//!
//! * `Frame = len u16 big endian (1..=65535) || bytes`.
//! * Envelope plaintext: `type u8 | flags u8 (0) | body_len u16 | body | zero padding`.
//!
//! Both parsers take untrusted bytes and never panic. [`FrameReader`] is cancel-safe, so it can
//! sit inside `tokio::select!`.

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{EnvelopeError, FrameError};
use crate::noise::{MAX_NOISE_MESSAGE, TAG_LEN};

/// Largest legal frame payload (the Noise message limit).
pub const MAX_FRAME_LEN: usize = MAX_NOISE_MESSAGE;
/// Largest message body: 60 KiB.
pub const MAX_BODY_LEN: usize = 61_440;
/// Envelope header length.
pub const ENVELOPE_HEADER_LEN: usize = 4;
/// SYNC plaintext is padded to a multiple of this.
pub const SYNC_PAD_UNIT: usize = 1024;
/// Every other message is padded to a multiple of this, so small messages look alike.
pub const SMALL_PAD_UNIT: usize = 64;
/// Longest envelope plaintext that still fits a Noise message after the tag.
pub const MAX_ENVELOPE_LEN: usize = MAX_NOISE_MESSAGE - TAG_LEN;

/// Encode `payload` as `len || payload`.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.is_empty() {
        return Err(FrameError::ZeroLength);
    }
    let len = u16::try_from(payload.len()).map_err(|_| FrameError::Oversize)?;
    let mut out = Vec::with_capacity(2 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Incremental frame decoder over a byte slice, for fuzzing and tests: returns the first frame and
/// how many bytes it used, or `Ok(None)` when more bytes are needed. Never panics.
pub fn decode_frame(bytes: &[u8], max_len: usize) -> Result<Option<(&[u8], usize)>, FrameError> {
    let Some(header) = bytes.get(..2) else {
        return Ok(None);
    };
    let len = usize::from(u16::from_be_bytes([header[0], header[1]]));
    if len == 0 {
        return Err(FrameError::ZeroLength);
    }
    if len > max_len {
        return Err(FrameError::Oversize);
    }
    match bytes.get(2..2 + len) {
        Some(body) => Ok(Some((body, 2 + len))),
        None => Ok(None),
    }
}

/// Reads frames from a stream, never buffering more than one frame (so at most
/// `2 + max_len` bytes). The length is checked as soon as the 2-byte header arrives, before the
/// body is read.
pub struct FrameReader<R> {
    inner: R,
    buf: Vec<u8>,
    max_len: usize,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// A reader that accepts frames up to `max_len` bytes.
    pub fn new(inner: R, max_len: usize) -> Self {
        FrameReader {
            inner,
            buf: Vec::new(),
            max_len: max_len.min(MAX_FRAME_LEN),
        }
    }

    /// Change the limit (for example from the 512-byte handshake cap to the transport cap).
    pub fn set_max_len(&mut self, max_len: usize) {
        self.max_len = max_len.min(MAX_FRAME_LEN);
    }

    /// Read the next frame.
    ///
    /// Cancel-safe: if the future is dropped, bytes already read stay buffered. Returns
    /// [`FrameError::Closed`] on a clean end of stream between frames and
    /// [`FrameError::Truncated`] when the stream ends inside one.
    pub async fn read_frame(&mut self) -> Result<Vec<u8>, FrameError> {
        loop {
            let mut want = 2usize.saturating_sub(self.buf.len());
            if self.buf.len() >= 2 {
                let len = usize::from(u16::from_be_bytes([self.buf[0], self.buf[1]]));
                if len == 0 {
                    return Err(FrameError::ZeroLength);
                }
                if len > self.max_len {
                    return Err(FrameError::Oversize);
                }
                want = (2 + len).saturating_sub(self.buf.len());
                if want == 0 {
                    let frame = self.buf[2..2 + len].to_vec();
                    self.buf.clear();
                    return Ok(frame);
                }
            }
            let mut scratch = [0u8; 8192];
            let take = want.min(scratch.len());
            let n = self.inner.read(&mut scratch[..take]).await?;
            if n == 0 {
                return Err(if self.buf.is_empty() {
                    FrameError::Closed
                } else {
                    FrameError::Truncated
                });
            }
            self.buf.extend_from_slice(&scratch[..n]);
        }
    }

    /// Read exactly `len` raw bytes (the unframed `ClientHello` and the status byte), at most 256.
    ///
    /// Cancel-safe like [`read_frame`](Self::read_frame): partial reads stay buffered.
    pub async fn read_raw(&mut self, len: usize) -> Result<Vec<u8>, FrameError> {
        let len = len.min(256);
        while self.buf.len() < len {
            let mut scratch = [0u8; 256];
            let want = len - self.buf.len();
            let n = self.inner.read(&mut scratch[..want]).await?;
            if n == 0 {
                return Err(if self.buf.is_empty() {
                    FrameError::Closed
                } else {
                    FrameError::Truncated
                });
            }
            self.buf.extend_from_slice(&scratch[..n]);
        }
        Ok(self.buf.drain(..len).collect())
    }

    /// Give back the underlying reader (any buffered partial frame is dropped).
    pub fn into_inner(self) -> R {
        self.inner
    }
}

/// Message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    /// Both directions; the PC adds its beacon key.
    Hello = 0x01,
    /// PC to phone: the user confirmed.
    PairOk = 0x02,
    /// PC to phone: a snapshot push.
    Sync = 0x03,
    /// Phone to PC: acknowledge a SYNC.
    Ack = 0x04,
    /// Phone to PC: toggle a reset alert.
    AlertSet = 0x05,
    /// Both directions.
    Ping = 0x06,
    /// Both directions.
    Pong = 0x07,
    /// Both directions: unpair, then the sender closes.
    Revoke = 0x08,
    /// Both directions: a protocol error, then the sender closes.
    Error = 0x7F,
}

impl MsgType {
    /// Parse a wire byte.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x01 => MsgType::Hello,
            0x02 => MsgType::PairOk,
            0x03 => MsgType::Sync,
            0x04 => MsgType::Ack,
            0x05 => MsgType::AlertSet,
            0x06 => MsgType::Ping,
            0x07 => MsgType::Pong,
            0x08 => MsgType::Revoke,
            0x7F => MsgType::Error,
            _ => return None,
        })
    }

    fn pad_unit(self) -> usize {
        if self == MsgType::Sync {
            SYNC_PAD_UNIT
        } else {
            SMALL_PAD_UNIT
        }
    }
}

/// Build the envelope plaintext for `body`, zero-padded to the type's padding unit.
pub fn encode_envelope(msg_type: MsgType, body: &[u8]) -> Result<Vec<u8>, EnvelopeError> {
    if body.len() > MAX_BODY_LEN {
        return Err(EnvelopeError::BodyTooLong);
    }
    let unpadded = ENVELOPE_HEADER_LEN + body.len();
    let unit = msg_type.pad_unit();
    let total = unpadded.div_ceil(unit) * unit;
    debug_assert!(total <= MAX_ENVELOPE_LEN);
    let mut out = Vec::with_capacity(total);
    out.push(msg_type as u8);
    out.push(0);
    // The body cap above bounds this to 61,440.
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out.resize(total, 0);
    Ok(out)
}

/// Parse an envelope plaintext into its type and body, checking flags, length and padding.
pub fn decode_envelope(plain: &[u8]) -> Result<(MsgType, &[u8]), EnvelopeError> {
    if plain.len() < ENVELOPE_HEADER_LEN {
        return Err(EnvelopeError::Short);
    }
    if plain[1] != 0 {
        return Err(EnvelopeError::Flags);
    }
    let msg_type = MsgType::from_byte(plain[0]).ok_or(EnvelopeError::UnknownType)?;
    let body_len = usize::from(u16::from_be_bytes([plain[2], plain[3]]));
    if body_len > MAX_BODY_LEN {
        return Err(EnvelopeError::BodyTooLong);
    }
    let end = ENVELOPE_HEADER_LEN + body_len;
    let body = plain
        .get(ENVELOPE_HEADER_LEN..end)
        .ok_or(EnvelopeError::Truncated)?;
    if plain[end..].iter().any(|&b| b != 0) {
        return Err(EnvelopeError::Padding);
    }
    Ok((msg_type, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn frames_round_trip_through_a_stream() {
        let (mut a, b) = tokio::io::duplex(4096);
        a.write_all(&encode_frame(b"one").unwrap()).await.unwrap();
        a.write_all(&encode_frame(&[7u8; 300]).unwrap())
            .await
            .unwrap();
        drop(a);
        let mut reader = FrameReader::new(b, 1000);
        assert_eq!(reader.read_frame().await.unwrap(), b"one");
        assert_eq!(reader.read_frame().await.unwrap(), vec![7u8; 300]);
        assert!(matches!(reader.read_frame().await, Err(FrameError::Closed)));
    }

    #[tokio::test]
    async fn reader_rejects_zero_oversize_and_truncated_frames() {
        type Check = fn(&FrameError) -> bool;
        let cases: [(&[u8], Check); 4] = [
            (&[0, 0], |e| matches!(e, FrameError::ZeroLength)),
            (&[0x02, 0x01, 1, 2], |e| matches!(e, FrameError::Oversize)),
            (&[0, 5, 1, 2], |e| matches!(e, FrameError::Truncated)),
            (&[0], |e| matches!(e, FrameError::Truncated)),
        ];
        for (bytes, check) in cases {
            let (mut a, b) = tokio::io::duplex(64);
            a.write_all(bytes).await.unwrap();
            drop(a);
            let mut reader = FrameReader::new(b, 512);
            let err = reader.read_frame().await.unwrap_err();
            assert!(check(&err), "unexpected {err:?} for {bytes:?}");
        }
    }

    #[tokio::test]
    async fn oversize_is_rejected_before_the_body_arrives() {
        let (mut a, b) = tokio::io::duplex(64);
        // Header says 513 bytes; none of them are sent and the stream stays open.
        a.write_all(&513u16.to_be_bytes()).await.unwrap();
        let mut reader = FrameReader::new(b, 512);
        assert!(matches!(
            reader.read_frame().await,
            Err(FrameError::Oversize)
        ));
    }

    #[test]
    fn encode_frame_enforces_bounds() {
        assert!(matches!(encode_frame(&[]), Err(FrameError::ZeroLength)));
        assert!(encode_frame(&vec![0u8; 65535]).is_ok());
        assert!(matches!(
            encode_frame(&vec![0u8; 65536]),
            Err(FrameError::Oversize)
        ));
    }

    #[test]
    fn decode_frame_slice_parser() {
        assert_eq!(decode_frame(&[], 512).unwrap(), None);
        assert_eq!(decode_frame(&[0, 3, 1, 2], 512).unwrap(), None);
        assert_eq!(
            decode_frame(&[0, 2, 9, 8, 7], 512).unwrap(),
            Some((&[9u8, 8][..], 4))
        );
        assert!(matches!(
            decode_frame(&[0, 0], 512),
            Err(FrameError::ZeroLength)
        ));
        assert!(matches!(
            decode_frame(&[0xff, 0xff], 512),
            Err(FrameError::Oversize)
        ));
    }

    #[test]
    fn envelope_round_trip_and_padding() {
        let plain = encode_envelope(MsgType::Ping, b"{\"n\":1}").unwrap();
        assert_eq!(plain.len() % SMALL_PAD_UNIT, 0);
        assert_eq!(
            decode_envelope(&plain).unwrap(),
            (MsgType::Ping, &b"{\"n\":1}"[..])
        );
        let sync = encode_envelope(MsgType::Sync, &[b'x'; 1500]).unwrap();
        assert_eq!(sync.len(), 2048);
        let max = encode_envelope(MsgType::Sync, &vec![b'x'; MAX_BODY_LEN]).unwrap();
        assert!(max.len() <= MAX_ENVELOPE_LEN);
        assert!(matches!(
            encode_envelope(MsgType::Sync, &vec![0u8; MAX_BODY_LEN + 1]),
            Err(EnvelopeError::BodyTooLong)
        ));
    }

    #[test]
    fn envelope_rejects_malformed_input() {
        let good = encode_envelope(MsgType::Ack, b"{}").unwrap();
        assert_eq!(decode_envelope(&good[..3]), Err(EnvelopeError::Short));
        let mut bad = good.clone();
        bad[1] = 1;
        assert_eq!(decode_envelope(&bad), Err(EnvelopeError::Flags));
        let mut bad = good.clone();
        bad[0] = 0x55;
        assert_eq!(decode_envelope(&bad), Err(EnvelopeError::UnknownType));
        let mut bad = good.clone();
        bad[2..4].copy_from_slice(&(MAX_BODY_LEN as u16 + 1).to_be_bytes());
        assert_eq!(decode_envelope(&bad), Err(EnvelopeError::BodyTooLong));
        let mut bad = good.clone();
        bad[2..4].copy_from_slice(&200u16.to_be_bytes());
        assert_eq!(decode_envelope(&bad), Err(EnvelopeError::Truncated));
        let mut bad = good.clone();
        let last = bad.len() - 1;
        bad[last] = 1;
        assert_eq!(decode_envelope(&bad), Err(EnvelopeError::Padding));
    }
}
