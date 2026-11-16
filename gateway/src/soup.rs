//! SoupBinTCP 4.0 framing: every packet is a big-endian u16 length (of what
//! follows), a type byte and a payload.

pub const LOGIN_ACCEPTED: u8 = b'A';
pub const LOGIN_REJECTED: u8 = b'J';
pub const SEQUENCED: u8 = b'S';
pub const SERVER_HEARTBEAT: u8 = b'H';
pub const END_OF_SESSION: u8 = b'Z';
pub const DEBUG: u8 = b'+';
pub const LOGIN_REQUEST: u8 = b'L';
pub const UNSEQUENCED: u8 = b'U';
pub const CLIENT_HEARTBEAT: u8 = b'R';
pub const LOGOUT_REQUEST: u8 = b'O';

/// Append a packet to `out`.
pub fn packet(out: &mut Vec<u8>, kind: u8, payload: &[u8]) {
    let n = payload.len() + 1;
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
}

/// Take the next complete packet from the front of `buf` (type, payload
/// range); None when more bytes are needed.
pub fn next_packet(buf: &[u8]) -> Option<(u8, std::ops::Range<usize>, usize)> {
    if buf.len() < 2 {
        return None;
    }
    let n = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    if n == 0 || buf.len() < 2 + n {
        return if n == 0 && buf.len() >= 2 { Some((0, 2..2, 2)) } else { None };
    }
    Some((buf[2], 3..2 + n, 2 + n))
}

/// A field padded with spaces on the right (alpha) to `len` bytes.
pub fn alpha(s: &str, len: usize) -> Vec<u8> {
    let mut v: Vec<u8> = s.bytes().take(len).collect();
    v.resize(len, b' ');
    v
}

/// A number as ASCII padded with spaces on the left to `len` bytes.
pub fn numeric(x: u64, len: usize) -> Vec<u8> {
    format!("{:>width$}", x, width = len).into_bytes()
}

pub struct LoginRequest {
    pub username: String,
    pub password: String,
    pub session: String,
    pub sequence: u64,
}

pub fn parse_login(p: &[u8]) -> Option<LoginRequest> {
    if p.len() < 46 {
        return None;
    }
    let s = |r: std::ops::Range<usize>| String::from_utf8_lossy(&p[r]).trim().to_string();
    Some(LoginRequest {
        username: s(0..6),
        password: s(6..16),
        session: s(16..26),
        sequence: s(26..46).parse().unwrap_or(0),
    })
}

pub fn login_request(user: &str, password: &str, session: &str, sequence: u64) -> Vec<u8> {
    let mut p = alpha(user, 6);
    p.extend(alpha(password, 10));
    p.extend(format!("{:>10}", session).into_bytes());
    p.extend(numeric(sequence, 20));
    let mut out = Vec::new();
    packet(&mut out, LOGIN_REQUEST, &p);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing() {
        let mut out = Vec::new();
        packet(&mut out, SEQUENCED, b"hello");
        packet(&mut out, SERVER_HEARTBEAT, b"");
        let (k, r, used) = next_packet(&out).unwrap();
        assert_eq!((k, &out[r.clone()]), (SEQUENCED, &b"hello"[..]));
        let (k2, r2, used2) = next_packet(&out[used..]).unwrap();
        assert_eq!((k2, r2.len(), used + used2), (SERVER_HEARTBEAT, 0, out.len()));
        assert!(next_packet(&out[..4]).is_none());
    }

    #[test]
    fn login() {
        let l = login_request("ALICE", "secret", "", 7);
        let (k, r, _) = next_packet(&l).unwrap();
        assert_eq!(k, LOGIN_REQUEST);
        let req = parse_login(&l[r]).unwrap();
        assert_eq!((req.username.as_str(), req.password.as_str(), req.session.as_str(), req.sequence), ("ALICE", "secret", "", 7));
    }
}
