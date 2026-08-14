//! Session message protocol with length-prefixed binary encoding.
//!
//! Wire format; all multibyte integers are little-endian:
//!
//! ```text
//! [u32 message length (excluding this field)] [u8 tag] [payload...]
//! string: [u16 length] [UTF-8 bytes]
//! ```
//!
//! The codec is implemented manually because there are few message types and
//! the frame, audio, and input payloads already use custom binary layouts.
//! Encoding and decoding are synchronous pure functions; only endpoint I/O is
//! asynchronous through tokio.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol version, validated during the handshake.
/// v2: `SwapSlots` replaced by the consent-based `SwapRequest`/`SwapResponse` pair.
/// v3: `Hello` carries a join role; `Roster` broadcasts room membership.
pub const PROTOCOL_VERSION: u16 = 3;

/// Role a client requests when joining a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinRole {
    /// Occupies the second player seat and sends input.
    Player,
    /// Receives the media stream only; never sends input.
    Spectator,
}

impl JoinRole {
    /// Wire encoding of the role.
    fn to_wire(self) -> u8 {
        match self {
            JoinRole::Player => 0,
            JoinRole::Spectator => 1,
        }
    }

    /// Decodes the wire value, rejecting unknown roles.
    fn from_wire(value: u8) -> io::Result<Self> {
        match value {
            0 => Ok(JoinRole::Player),
            1 => Ok(JoinRole::Spectator),
            other => Err(invalid(format!("未知的加入角色: {other}"))),
        }
    }
}

/// Maximum message size: 16 MiB, well above an uncompressed frame.
/// This prevents a corrupted length field from exhausting memory.
const MAX_MESSAGE_LEN: u32 = 16 * 1024 * 1024;

/// Session message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Client greeting with protocol version, display name, and desired role.
    Hello {
        /// Client protocol version.
        version: u16,
        /// Client display name, such as a host name.
        name: String,
        /// Seat the client wants to occupy.
        role: JoinRole,
    },
    /// Host response requesting the pairing code.
    Challenge,
    /// Pairing code submitted by the client.
    PairCode {
        /// Four-digit pairing code.
        code: String,
    },
    /// Pairing succeeded, with the ROM currently running on the host.
    Welcome {
        /// Host ROM name for client display.
        rom_name: String,
    },
    /// Rejection reason; the host disconnects after sending this message.
    Reject {
        /// Human-readable rejection reason.
        reason: String,
    },
    /// Keepalive request for idle periods.
    Ping,
    /// Keepalive response.
    Pong,
    /// Requests a P1/P2 slot swap; the other side must accept before it happens.
    SwapRequest,
    /// Answer to `SwapRequest`; on acceptance the host flips the slots and
    /// broadcasts the new `SlotState`.
    SwapResponse {
        /// Whether the peer agreed to the swap.
        accepted: bool,
    },
    /// Host broadcast of the current slot assignment.
    SlotState {
        /// Whether the host occupies P1; false means host=P2 and client=P1.
        host_is_p1: bool,
    },
    /// The host starts a game, prompting the client to enter the netplay view.
    GameStart {
        /// Name of the game being started.
        rom_name: String,
        /// Host audio sample rate in Hz, used to configure the client output stream.
        sample_rate: u32,
    },
    /// Video frame (host -> client): LZ4-compressed RGBA frame or XOR delta.
    Frame {
        /// Increasing frame sequence number for diagnostics and statistics.
        seq: u32,
        /// `true` for a full keyframe, `false` for a delta frame.
        keyframe: bool,
        /// Compressed frame data.
        data: Vec<u8>,
    },
    /// Audio chunk (host -> client): mono little-endian i16 PCM.
    AudioChunk {
        /// Little-endian i16 PCM data.
        data: Vec<u8>,
    },
    /// Controller input (client -> host): a complete state-replacement bitmap.
    Input {
        /// Button bitmap with the same bit order as `ButtonState`.
        buttons: u8,
    },
    /// Game ended (host -> client); both sides return to the room and may choose another game.
    GameEnd,
    /// Room membership broadcast (host -> all) sent whenever it changes.
    Roster {
        /// Display name of the guest player seat, when occupied.
        player: Option<String>,
        /// Display names of connected spectators.
        spectators: Vec<String>,
    },
    /// A spectator asks to take the player seat (spectator -> host), and the
    /// host forwards it to the seated player for approval (host -> player).
    SeatChangeRequest {
        /// Requesting spectator's display name; empty when sent to the host,
        /// which already knows the sender.
        spectator: String,
    },
    /// Answer to `SeatChangeRequest` (player -> host), relayed to the
    /// requester on decline (host -> spectator).
    SeatChangeResponse {
        /// Whether the seat change was approved.
        accepted: bool,
    },
    /// The receiver's own role changed after an approved seat change (host -> client).
    RoleChanged {
        /// The receiver's new role.
        role: JoinRole,
    },
}

/// Tag values for each message type.
mod tag {
    pub const HELLO: u8 = 1;
    pub const CHALLENGE: u8 = 2;
    pub const PAIR_CODE: u8 = 3;
    pub const WELCOME: u8 = 4;
    pub const REJECT: u8 = 5;
    pub const PING: u8 = 6;
    pub const PONG: u8 = 7;
    pub const SWAP_REQUEST: u8 = 8;
    pub const SLOT_STATE: u8 = 9;
    pub const GAME_START: u8 = 10;
    pub const FRAME: u8 = 11;
    pub const AUDIO_CHUNK: u8 = 12;
    pub const INPUT: u8 = 13;
    pub const GAME_END: u8 = 14;
    pub const SWAP_RESPONSE: u8 = 15;
    pub const ROSTER: u8 = 16;
    pub const SEAT_CHANGE_REQUEST: u8 = 17;
    pub const SEAT_CHANGE_RESPONSE: u8 = 18;
    pub const ROLE_CHANGED: u8 = 19;
}

impl Message {
    /// Encodes, writes, and flushes a complete message asynchronously.
    pub async fn write_to(&self, w: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        let (tag_byte, payload) = self.encode()?;
        let total = 1 + u32::try_from(payload.len()).map_err(|_| invalid("消息过长"))?;
        w.write_all(&total.to_le_bytes()).await?;
        w.write_all(&[tag_byte]).await?;
        w.write_all(&payload).await?;
        w.flush().await
    }

    /// Reads and decodes a complete message asynchronously.
    pub async fn read_from(r: &mut (impl AsyncRead + Unpin)) -> io::Result<Self> {
        let mut len_bytes = [0u8; 4];
        r.read_exact(&mut len_bytes).await?;
        let len = u32::from_le_bytes(len_bytes);
        if len == 0 || len > MAX_MESSAGE_LEN {
            return Err(invalid(format!("非法消息长度: {len}")));
        }
        let mut buf = vec![0u8; len as usize];
        r.read_exact(&mut buf).await?;
        Self::decode(&buf)
    }

    /// Encodes to `(tag, payload)` as a synchronous pure function.
    fn encode(&self) -> io::Result<(u8, Vec<u8>)> {
        let mut payload = Vec::new();
        let tag_byte = match self {
            Message::Hello {
                version,
                name,
                role,
            } => {
                payload.extend_from_slice(&version.to_le_bytes());
                payload.push(role.to_wire());
                write_str(&mut payload, name)?;
                tag::HELLO
            }
            Message::Challenge => tag::CHALLENGE,
            Message::PairCode { code } => {
                write_str(&mut payload, code)?;
                tag::PAIR_CODE
            }
            Message::Welcome { rom_name } => {
                write_str(&mut payload, rom_name)?;
                tag::WELCOME
            }
            Message::Reject { reason } => {
                write_str(&mut payload, reason)?;
                tag::REJECT
            }
            Message::Ping => tag::PING,
            Message::Pong => tag::PONG,
            Message::SwapRequest => tag::SWAP_REQUEST,
            Message::SwapResponse { accepted } => {
                payload.push(u8::from(*accepted));
                tag::SWAP_RESPONSE
            }
            Message::SlotState { host_is_p1 } => {
                payload.push(u8::from(*host_is_p1));
                tag::SLOT_STATE
            }
            Message::GameStart {
                rom_name,
                sample_rate,
            } => {
                payload.extend_from_slice(&sample_rate.to_le_bytes());
                write_str(&mut payload, rom_name)?;
                tag::GAME_START
            }
            Message::Frame {
                seq,
                keyframe,
                data,
            } => {
                payload.extend_from_slice(&seq.to_le_bytes());
                payload.push(u8::from(*keyframe));
                payload.extend_from_slice(data);
                tag::FRAME
            }
            Message::AudioChunk { data } => {
                payload.extend_from_slice(data);
                tag::AUDIO_CHUNK
            }
            Message::Input { buttons } => {
                payload.push(*buttons);
                tag::INPUT
            }
            Message::GameEnd => tag::GAME_END,
            Message::SeatChangeRequest { spectator } => {
                write_str(&mut payload, spectator)?;
                tag::SEAT_CHANGE_REQUEST
            }
            Message::SeatChangeResponse { accepted } => {
                payload.push(u8::from(*accepted));
                tag::SEAT_CHANGE_RESPONSE
            }
            Message::RoleChanged { role } => {
                payload.push(role.to_wire());
                tag::ROLE_CHANGED
            }
            Message::Roster { player, spectators } => {
                match player {
                    Some(name) => {
                        payload.push(1);
                        write_str(&mut payload, name)?;
                    }
                    None => payload.push(0),
                }
                let count =
                    u8::try_from(spectators.len()).map_err(|_| invalid("观众数量超出编码上限"))?;
                payload.push(count);
                for name in spectators {
                    write_str(&mut payload, name)?;
                }
                tag::ROSTER
            }
        };
        Ok((tag_byte, payload))
    }

    /// Decodes a `[tag][payload]` buffer as a synchronous pure function.
    fn decode(buf: &[u8]) -> io::Result<Self> {
        let (&tag_byte, mut rest) = buf.split_first().ok_or_else(|| invalid("空消息"))?;
        let msg = match tag_byte {
            tag::HELLO => {
                let version_bytes = take(&mut rest, 2)?;
                let version = u16::from_le_bytes([version_bytes[0], version_bytes[1]]);
                let role = JoinRole::from_wire(take(&mut rest, 1)?[0])?;
                let name = read_str(&mut rest)?;
                Message::Hello {
                    version,
                    name,
                    role,
                }
            }
            tag::CHALLENGE => Message::Challenge,
            tag::PAIR_CODE => Message::PairCode {
                code: read_str(&mut rest)?,
            },
            tag::WELCOME => Message::Welcome {
                rom_name: read_str(&mut rest)?,
            },
            tag::REJECT => Message::Reject {
                reason: read_str(&mut rest)?,
            },
            tag::PING => Message::Ping,
            tag::PONG => Message::Pong,
            tag::SWAP_REQUEST => Message::SwapRequest,
            tag::SWAP_RESPONSE => {
                let flag = take(&mut rest, 1)?;
                Message::SwapResponse {
                    accepted: flag[0] != 0,
                }
            }
            tag::SLOT_STATE => {
                let flag = take(&mut rest, 1)?;
                Message::SlotState {
                    host_is_p1: flag[0] != 0,
                }
            }
            tag::GAME_START => {
                let rate = take(&mut rest, 4)?;
                Message::GameStart {
                    sample_rate: u32::from_le_bytes([rate[0], rate[1], rate[2], rate[3]]),
                    rom_name: read_str(&mut rest)?,
                }
            }
            tag::FRAME => {
                let seq_bytes = take(&mut rest, 4)?;
                let flag = take(&mut rest, 1)?;
                Message::Frame {
                    seq: u32::from_le_bytes([
                        seq_bytes[0],
                        seq_bytes[1],
                        seq_bytes[2],
                        seq_bytes[3],
                    ]),
                    keyframe: flag[0] != 0,
                    data: rest.to_vec(),
                }
            }
            tag::AUDIO_CHUNK => Message::AudioChunk {
                data: rest.to_vec(),
            },
            tag::INPUT => {
                let b = take(&mut rest, 1)?;
                Message::Input { buttons: b[0] }
            }
            tag::GAME_END => Message::GameEnd,
            tag::SEAT_CHANGE_REQUEST => Message::SeatChangeRequest {
                spectator: read_str(&mut rest)?,
            },
            tag::SEAT_CHANGE_RESPONSE => {
                let flag = take(&mut rest, 1)?;
                Message::SeatChangeResponse {
                    accepted: flag[0] != 0,
                }
            }
            tag::ROLE_CHANGED => Message::RoleChanged {
                role: JoinRole::from_wire(take(&mut rest, 1)?[0])?,
            },
            tag::ROSTER => {
                let player = match take(&mut rest, 1)?[0] {
                    0 => None,
                    _ => Some(read_str(&mut rest)?),
                };
                let count = usize::from(take(&mut rest, 1)?[0]);
                let mut spectators = Vec::with_capacity(count);
                for _ in 0..count {
                    spectators.push(read_str(&mut rest)?);
                }
                Message::Roster { player, spectators }
            }
            other => return Err(invalid(format!("未知消息 tag: {other}"))),
        };
        Ok(msg)
    }
}

/// Writes a length-prefixed string; protocol strings must fit in a u16 length.
fn write_str(buf: &mut Vec<u8>, s: &str) -> io::Result<()> {
    let len = u16::try_from(s.len()).map_err(|_| invalid("字符串过长"))?;
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Reads a length-prefixed string from a slice cursor.
fn read_str(rest: &mut &[u8]) -> io::Result<String> {
    let len_bytes = take(rest, 2)?;
    let len = usize::from(u16::from_le_bytes([len_bytes[0], len_bytes[1]]));
    let bytes = take(rest, len)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| invalid("字符串不是合法 UTF-8"))
}

/// Takes the next `n` bytes from a slice cursor.
fn take<'a>(rest: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    if rest.len() < n {
        return Err(invalid("消息载荷不完整"));
    }
    let (head, tail) = rest.split_at(n);
    *rest = tail;
    Ok(head)
}

/// Constructs an `InvalidData` error.
fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Every message type survives the full async I/O round trip.
    #[tokio::test]
    async fn message_roundtrip() {
        let cases = vec![
            Message::Hello {
                version: PROTOCOL_VERSION,
                name: "Zero's MacBook".to_string(),
                role: JoinRole::Player,
            },
            Message::Hello {
                version: PROTOCOL_VERSION,
                name: "onlooker".to_string(),
                role: JoinRole::Spectator,
            },
            Message::Challenge,
            Message::PairCode {
                code: "0427".to_string(),
            },
            Message::Welcome {
                rom_name: "Contra.nes".to_string(),
            },
            Message::Reject {
                reason: "invalid pairing code".to_string(),
            },
            Message::Ping,
            Message::Pong,
            Message::SwapRequest,
            Message::SwapResponse { accepted: true },
            Message::SwapResponse { accepted: false },
            Message::SlotState { host_is_p1: false },
            Message::GameStart {
                rom_name: "River City Ransom.nes".to_string(),
                sample_rate: 48_000,
            },
            Message::Frame {
                seq: 12345,
                keyframe: true,
                data: vec![1, 2, 3, 4, 5],
            },
            Message::AudioChunk {
                data: vec![0x10, 0x20, 0x30],
            },
            Message::Input {
                buttons: 0b1000_0001,
            },
            Message::GameEnd,
            Message::Roster {
                player: None,
                spectators: Vec::new(),
            },
            Message::Roster {
                player: Some("挑战者".to_string()),
                spectators: vec!["观众A".to_string(), "观众B".to_string()],
            },
            Message::SeatChangeRequest {
                spectator: "观众A".to_string(),
            },
            Message::SeatChangeResponse { accepted: true },
            Message::SeatChangeResponse { accepted: false },
            Message::RoleChanged {
                role: JoinRole::Player,
            },
        ];
        for original in cases {
            let mut wire = Vec::new();
            original.write_to(&mut wire).await.unwrap();
            let decoded = Message::read_from(&mut wire.as_slice()).await.unwrap();
            assert_eq!(decoded, original);
        }
    }

    /// Multiple consecutive messages can be read from the same stream.
    #[tokio::test]
    async fn multiple_messages_in_stream() {
        let mut wire = Vec::new();
        Message::Ping.write_to(&mut wire).await.unwrap();
        Message::Pong.write_to(&mut wire).await.unwrap();
        let mut cursor = wire.as_slice();
        assert_eq!(
            Message::read_from(&mut cursor).await.unwrap(),
            Message::Ping
        );
        assert_eq!(
            Message::read_from(&mut cursor).await.unwrap(),
            Message::Pong
        );
    }

    /// Corrupted lengths and unknown tags return errors instead of panicking.
    #[tokio::test]
    async fn malformed_input_is_rejected() {
        // The length field claims 32 MiB, exceeding the limit.
        let huge = (MAX_MESSAGE_LEN * 2).to_le_bytes();
        assert!(Message::read_from(&mut huge.as_slice()).await.is_err());
        // Unknown tag.
        let unknown: &[u8] = &[1, 0, 0, 0, 250];
        assert!(Message::read_from(&mut &*unknown).await.is_err());
        // Zero length.
        let zero: &[u8] = &[0, 0, 0, 0];
        assert!(Message::read_from(&mut &*zero).await.is_err());
    }
}
