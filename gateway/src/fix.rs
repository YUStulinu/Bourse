//! FIX 4.4 tag=value messages: framing with BodyLength (9) and CheckSum (10),
//! and the standard header.

pub const SOH: u8 = 0x01;
pub const BEGIN: &str = "FIX.4.4";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    pub fields: Vec<(u32, String)>,
}

impl Message {
    pub fn new(msg_type: &str) -> Message {
        Message { fields: vec![(35, msg_type.to_string())] }
    }
    pub fn get(&self, tag: u32) -> Option<&str> {
        self.fields.iter().find(|(t, _)| *t == tag).map(|(_, v)| v.as_str())
    }
    pub fn msg_type(&self) -> &str {
        self.get(35).unwrap_or("")
    }
    pub fn set(&mut self, tag: u32, v: impl ToString) -> &mut Self {
        self.fields.push((tag, v.to_string()));
        self
    }
    pub fn int(&self, tag: u32) -> Option<u64> {
        self.get(tag)?.parse().ok()
    }
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    Garbled(&'static str),
}

/// Take one complete message from the front of `buf`: Ok(Some((msg, used)))
/// when there is one, Ok(None) when more bytes are needed.
pub fn next_message(buf: &[u8]) -> Result<Option<(Message, usize)>, ParseError> {
    let prefix = b"8=FIX.4.4\x019=";
    if buf.len() < prefix.len() {
        return if prefix.starts_with(buf) { Ok(None) } else { Err(ParseError::Garbled("begin string")) };
    }
    if !buf.starts_with(prefix) {
        return Err(ParseError::Garbled("begin string"));
    }
    let mut i = prefix.len();
    let mut body_len = 0usize;
    loop {
        match buf.get(i) {
            None => return Ok(None),
            Some(&SOH) => break,
            Some(c @ b'0'..=b'9') => body_len = body_len * 10 + (c - b'0') as usize,
            _ => return Err(ParseError::Garbled("body length")),
        }
        i += 1;
        if i > prefix.len() + 7 {
            return Err(ParseError::Garbled("body length"));
        }
    }
    let body_start = i + 1;
    let trailer = body_start + body_len;
    if buf.len() < trailer + 7 {
        return Ok(None);
    }
    if &buf[trailer..trailer + 3] != b"10=" || buf[trailer + 6] != SOH {
        return Err(ParseError::Garbled("checksum field"));
    }
    let sum: u32 = buf[..trailer].iter().map(|&b| b as u32).sum::<u32>() % 256;
    let given: u32 = std::str::from_utf8(&buf[trailer + 3..trailer + 6]).ok().and_then(|s| s.parse().ok()).ok_or(ParseError::Garbled("checksum"))?;
    if sum != given {
        return Err(ParseError::Garbled("checksum mismatch"));
    }
    let mut fields = Vec::new();
    for f in buf[body_start..trailer].split(|&b| b == SOH).filter(|f| !f.is_empty()) {
        let eq = f.iter().position(|&b| b == b'=').ok_or(ParseError::Garbled("field"))?;
        let tag: u32 = std::str::from_utf8(&f[..eq]).ok().and_then(|s| s.parse().ok()).ok_or(ParseError::Garbled("tag"))?;
        fields.push((tag, String::from_utf8_lossy(&f[eq + 1..]).into_owned()));
    }
    if fields.first().map(|f| f.0) != Some(35) {
        return Err(ParseError::Garbled("MsgType must come first"));
    }
    Ok(Some((Message { fields }, trailer + 7)))
}

/// Serialize with the standard header (35, 49, 56, 34, 52 first) and trailer.
pub fn encode(m: &Message, sender: &str, target: &str, seq: u64, sending_time: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(256);
    let mut put = |tag: u32, v: &str| {
        body.extend_from_slice(tag.to_string().as_bytes());
        body.push(b'=');
        body.extend_from_slice(v.as_bytes());
        body.push(SOH);
    };
    put(35, m.msg_type());
    put(49, sender);
    put(56, target);
    put(34, &seq.to_string());
    put(52, sending_time);
    for (t, v) in &m.fields {
        if !matches!(*t, 35 | 49 | 56 | 34 | 52 | 8 | 9 | 10) {
            put(*t, v);
        }
    }
    let mut out = Vec::with_capacity(body.len() + 32);
    out.extend_from_slice(format!("8={}\x019={}\x01", BEGIN, body.len()).as_bytes());
    out.extend_from_slice(&body);
    let sum: u32 = out.iter().map(|&b| b as u32).sum::<u32>() % 256;
    out.extend_from_slice(format!("10={:03}\x01", sum).as_bytes());
    out
}

/// UTCTimestamp (YYYYMMDD-HH:MM:SS.sss) of nanoseconds since the epoch.
pub fn utc_timestamp(ns: u64) -> String {
    let secs = ns / 1_000_000_000;
    let ms = (ns / 1_000_000) % 1000;
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{:04}{:02}{:02}-{:02}:{:02}:{:02}.{:03}", y, m, d, rem / 3600, (rem / 60) % 60, rem % 60, ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut m = Message::new("D");
        m.set(11, "ord-1").set(55, "AAPL").set(54, "1").set(38, 100).set(40, "2").set(44, "154.32");
        let bytes = encode(&m, "CLIENT", "BOURSE", 3, "20190130-14:30:00.000");
        let (p, used) = next_message(&bytes).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(p.msg_type(), "D");
        assert_eq!(p.get(49), Some("CLIENT"));
        assert_eq!(p.int(34), Some(3));
        assert_eq!(p.get(44), Some("154.32"));
        // partial input waits for more
        assert_eq!(next_message(&bytes[..bytes.len() - 1]).unwrap(), None);
        assert_eq!(next_message(&bytes[..5]).unwrap(), None);
        // corrupted checksum
        let mut bad = bytes.clone();
        let k = bad.len() - 30;
        bad[k] ^= 1;
        assert!(next_message(&bad).is_err());
    }

    #[test]
    fn known_checksum() {
        // a heartbeat checked by hand: the sum of all bytes before "10=" mod 256
        let m = Message::new("0");
        let b = encode(&m, "A", "B", 1, "20000101-00:00:00.000");
        let s = String::from_utf8(b.clone()).unwrap();
        assert!(s.starts_with("8=FIX.4.4\x019=45\x0135=0\x0149=A\x0156=B\x0134=1\x0152=20000101-00:00:00.000\x01"));
        let sum: u32 = b[..b.len() - 7].iter().map(|&x| x as u32).sum::<u32>() % 256;
        assert!(s.ends_with(&format!("10={:03}\x01", sum)));
    }

    #[test]
    fn timestamps() {
        assert_eq!(utc_timestamp(0), "19700101-00:00:00.000");
        assert_eq!(utc_timestamp(1_548_858_600_123_000_000), "20190130-14:30:00.123");
        assert_eq!(utc_timestamp(951_782_400_000_000_000), "20000229-00:00:00.000");
    }
}
