//! WebSocket, in as little code as RFC 6455 allows.
//!
//! The workspace has no external dependencies and this crate does not break
//! that. A WebSocket server needs exactly three things the standard library
//! does not give it — SHA-1 and base64 for the opening handshake, and the
//! frame codec — and all three are short. They are here, and nowhere else in
//! the crate is there any notion of a socket format.
//!
//! Only what a browser or `URLSessionWebSocketTask` actually sends is handled:
//! text and binary frames, continuation frames, ping, pong and close. Client
//! frames are always masked and this rejects them if they are not, as the RFC
//! requires. Server frames are never masked.

use std::io::{self, Read, Write};
use std::net::TcpStream;

/// The RFC 6455 GUID, appended to the client key before hashing.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A frame's worth of application data.
pub enum Msg {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

/// Largest client frame accepted. A deploy is source text; nothing legitimate
/// approaches this, and without a bound a bad frame header asks us to allocate
/// four gigabytes.
pub const MAX_FRAME: u64 = 8 << 20;

// ---------------------------------------------------------------------------
// Handshake

/// Read the opening HTTP request and complete the upgrade.
///
/// Returns the request target, so the caller can route on the path if it ever
/// wants to. Any failure leaves the socket in whatever state it reached; the
/// caller closes it.
pub fn accept(sock: &mut TcpStream) -> io::Result<String> {
    let head = read_head(sock)?;
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or("").to_string();
    let mut key = None;
    let mut upgrade = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "sec-websocket-key" => key = Some(value.to_string()),
            "upgrade" => upgrade = value.eq_ignore_ascii_case("websocket"),
            _ => {}
        }
    }
    let target = request.split_whitespace().nth(1).unwrap_or("/").to_string();
    let Some(key) = key.filter(|_| upgrade) else {
        let _ = sock.write_all(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a websocket upgrade",
        ));
    };
    let accept = b64(&sha1(format!("{key}{GUID}").as_bytes()));
    sock.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .as_bytes(),
    )?;
    sock.flush()?;
    Ok(target)
}

/// Read bytes until the blank line that ends an HTTP head, with a ceiling so a
/// client that never sends one cannot exhaust memory.
fn read_head(sock: &mut TcpStream) -> io::Result<String> {
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    while buf.len() < 16 * 1024 {
        let n = sock.read(&mut byte)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed during handshake",
            ));
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            return String::from_utf8(buf)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "handshake not utf-8"));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "handshake head too long",
    ))
}

// ---------------------------------------------------------------------------
// Frames

/// Read one message, reassembling continuation frames.
///
/// `Ok(None)` means the peer closed cleanly.
pub fn read(sock: &mut TcpStream) -> io::Result<Option<Msg>> {
    let mut acc: Vec<u8> = Vec::new();
    let mut acc_text = false;
    loop {
        let (fin, opcode, payload) = read_frame(sock)?;
        match opcode {
            0x0 => {
                // Continuation. Without a start frame this is a protocol error.
                if acc.is_empty() && !acc_text {
                    return Err(bad("continuation without a start frame"));
                }
                acc.extend_from_slice(&payload);
            }
            0x1 => {
                acc_text = true;
                acc = payload;
            }
            0x2 => {
                acc_text = false;
                acc = payload;
            }
            0x8 => return Ok(Some(Msg::Close)),
            0x9 => return Ok(Some(Msg::Ping(payload))),
            0xA => return Ok(Some(Msg::Pong(payload))),
            other => return Err(bad(&format!("unknown opcode {other:#x}"))),
        }
        if fin {
            return Ok(Some(if acc_text {
                Msg::Text(
                    String::from_utf8(acc).map_err(|_| bad("text frame was not valid utf-8"))?,
                )
            } else {
                Msg::Binary(acc)
            }));
        }
        if acc.len() as u64 > MAX_FRAME {
            return Err(bad("reassembled message too large"));
        }
    }
}

fn read_frame(sock: &mut TcpStream) -> io::Result<(bool, u8, Vec<u8>)> {
    let mut h = [0u8; 2];
    sock.read_exact(&mut h)?;
    let fin = h[0] & 0x80 != 0;
    let opcode = h[0] & 0x0F;
    let masked = h[1] & 0x80 != 0;
    let mut len = (h[1] & 0x7F) as u64;
    if len == 126 {
        let mut e = [0u8; 2];
        sock.read_exact(&mut e)?;
        len = u16::from_be_bytes(e) as u64;
    } else if len == 127 {
        let mut e = [0u8; 8];
        sock.read_exact(&mut e)?;
        len = u64::from_be_bytes(e);
    }
    if len > MAX_FRAME {
        return Err(bad("frame larger than the accepted maximum"));
    }
    // RFC 6455 §5.1: a server must close on an unmasked client frame.
    if !masked {
        return Err(bad("client frame was not masked"));
    }
    let mut mask = [0u8; 4];
    sock.read_exact(&mut mask)?;
    let mut payload = vec![0u8; len as usize];
    sock.read_exact(&mut payload)?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i & 3];
    }
    Ok((fin, opcode, payload))
}

/// Write one unfragmented, unmasked server frame.
pub fn write(sock: &mut TcpStream, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut head = Vec::with_capacity(10);
    head.push(0x80 | opcode);
    let n = payload.len();
    if n < 126 {
        head.push(n as u8);
    } else if n <= u16::MAX as usize {
        head.push(126);
        head.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        head.push(127);
        head.extend_from_slice(&(n as u64).to_be_bytes());
    }
    sock.write_all(&head)?;
    sock.write_all(payload)?;
    sock.flush()
}

pub fn write_text(sock: &mut TcpStream, s: &str) -> io::Result<()> {
    write(sock, 0x1, s.as_bytes())
}

pub fn write_close(sock: &mut TcpStream) -> io::Result<()> {
    // 1000: normal closure.
    write(sock, 0x8, &1000u16.to_be_bytes())
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

// ---------------------------------------------------------------------------
// SHA-1 and base64, for the handshake and for nothing else.
//
// SHA-1 is here because RFC 6455 names it, not because anything is being
// secured: the value is a fixed transformation of a public nonce, and the
// handshake would be no weaker with a CRC. Nothing else in the workspace
// hashes with it — content identity is BLAKE2b-256, in `k1ndl1ng-norm`.

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0,
    ];
    let bits = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());

    let mut w = [0u32; 80];
    for chunk in msg.chunks_exact(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn b64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
