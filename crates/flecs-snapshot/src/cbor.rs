//! A DAG-CBOR writer that appends to a byte buffer, byte-identical to `serde_ipld_dagcbor::to_vec` for the values the
//! library writes (SPEC.md, "Encoding"): minimal-length heads, floats always 64-bit, map keys sorted by length then
//! bytes, CIDs as tag 42 over a 0x00-prefixed byte string. The hot paths (host records, host documents) write through
//! it instead of building an `Ipld` tree; the tests check it against serde on every kind.

use cid::Cid;
use ipld_core::ipld::Ipld;

pub fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    if n < 24 {
        out.push(m | n as u8);
    } else if n <= u8::MAX as u64 {
        out.extend_from_slice(&[m | 24, n as u8]);
    } else if n <= u16::MAX as u64 {
        out.push(m | 25);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n <= u32::MAX as u64 {
        out.push(m | 26);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(m | 27);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

/// An integer in DAG-CBOR's range (-2^64 to 2^64 - 1). Callers only pass values that came from at most 64-bit
/// memory.
pub fn int(out: &mut Vec<u8>, v: i128) {
    if v >= 0 {
        head(out, 0, v as u64);
    } else {
        head(out, 1, (-1 - v) as u64);
    }
}

/// A finite float (checked by the caller), always as 64 bits; -0.0 is written as 0.0, as DAG-CBOR requires.
pub fn float(out: &mut Vec<u8>, v: f64) {
    let v = if v == 0.0 { 0.0 } else { v };
    out.push(0xfb);
    out.extend_from_slice(&v.to_bits().to_be_bytes());
}

pub fn text(out: &mut Vec<u8>, s: &str) {
    head(out, 3, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

pub fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    head(out, 2, b.len() as u64);
    out.extend_from_slice(b);
}

pub fn null(out: &mut Vec<u8>) {
    out.push(0xf6);
}

pub fn bool(out: &mut Vec<u8>, v: bool) {
    out.push(if v { 0xf5 } else { 0xf4 });
}

pub fn list(out: &mut Vec<u8>, n: usize) {
    head(out, 4, n as u64);
}

pub fn map(out: &mut Vec<u8>, n: usize) {
    head(out, 5, n as u64);
}

pub fn link(out: &mut Vec<u8>, c: &Cid) {
    out.extend_from_slice(&[0xd8, 0x2a]);
    let b = c.to_bytes();
    head(out, 2, b.len() as u64 + 1);
    out.push(0);
    out.extend_from_slice(&b);
}

/// DAG-CBOR's map key order: shorter keys first, then bytewise.
pub fn key_order(a: &str, b: &str) -> core::cmp::Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

/// Any value (floats must be finite, as everywhere in the library).
pub fn ipld(out: &mut Vec<u8>, v: &Ipld) {
    match v {
        Ipld::Null => null(out),
        Ipld::Bool(b) => bool(out, *b),
        Ipld::Integer(i) => int(out, *i),
        Ipld::Float(f) => float(out, *f),
        Ipld::String(s) => text(out, s),
        Ipld::Bytes(b) => bytes(out, b),
        Ipld::List(l) => {
            list(out, l.len());
            for x in l {
                ipld(out, x);
            }
        }
        Ipld::Map(m) => {
            map(out, m.len());
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by(|a, b| key_order(a, b));
            for k in keys {
                text(out, k);
                ipld(out, &m[k]);
            }
        }
        Ipld::Link(c) => link(out, c),
    }
}

/// Decodes one DAG-CBOR value that fills `b`, strictly: minimal-length heads, definite lengths, text map keys in
/// DAG-CBOR order with no duplicate, 64-bit finite floats with no -0.0, tag 42 only (a CID), and no trailing bytes.
pub fn decode(b: &[u8]) -> Result<Ipld, String> {
    let mut d = Decoder { b, at: 0 };
    let v = d.value(0)?;
    if d.at != b.len() {
        return Err(format!("{} trailing bytes after the value", b.len() - d.at));
    }
    Ok(v)
}

/// A strict DAG-CBOR reader over a byte slice (the rules of `decode`), for reading a value piece by piece.
pub struct Decoder<'a> {
    b: &'a [u8],
    pub at: usize,
}

/// Nesting deeper than this is refused rather than recursed into.
const MAX_DEPTH: usize = 512;

impl<'a> Decoder<'a> {
    pub fn new(b: &'a [u8]) -> Decoder<'a> {
        Decoder { b, at: 0 }
    }

    /// The bytes from `start` to the current position.
    pub fn since(&self, start: usize) -> &'a [u8] {
        &self.b[start..self.at]
    }

    pub fn done(&self) -> bool {
        self.at == self.b.len()
    }

    /// The next byte, not consumed.
    pub fn peek(&self) -> Result<u8, String> {
        match self.b.get(self.at) {
            Some(b) => Ok(*b),
            None => self.err("the input ends before a value"),
        }
    }

    /// A head of major type `major`; its argument.
    pub fn expect(&mut self, major: u8, what: &str) -> Result<u64, String> {
        if self.peek()? >> 5 != major || major == 7 {
            return self.err(&format!("{what} expected"));
        }
        Ok(self.head()?.1)
    }

    /// A list header of exactly `n` elements.
    pub fn list_of(&mut self, n: usize, what: &str) -> Result<(), String> {
        let got = self.expect(4, what)?;
        if got != n as u64 {
            return self.err(&format!("{what} has {got} elements, not {n}"));
        }
        Ok(())
    }

    /// A text string, borrowed.
    pub fn str(&mut self) -> Result<&'a str, String> {
        let n = self.expect(3, "a string")?;
        let n = self.len(n)?;
        let s = self.take(n)?;
        match std::str::from_utf8(s) {
            Ok(s) => Ok(s),
            Err(_) => self.err("a string that is not UTF-8"),
        }
    }

    /// The map key `k`, exactly.
    pub fn key(&mut self, k: &str) -> Result<(), String> {
        let got = self.str()?;
        if got != k {
            return self.err(&format!("the key {k:?} expected, found {got:?}"));
        }
        Ok(())
    }

    /// An integer (major 0 or 1).
    pub fn int(&mut self) -> Result<i128, String> {
        match self.peek()? >> 5 {
            0 => Ok(self.head()?.1 as i128),
            1 => Ok(-1 - self.head()?.1 as i128),
            _ => self.err("an integer expected"),
        }
    }

    /// A finite 64-bit float that is not -0.0.
    pub fn float(&mut self) -> Result<f64, String> {
        match self.peek()? {
            0xfb => match self.value(0)? {
                Ipld::Float(f) => Ok(f),
                _ => unreachable!("0xfb is a float"),
            },
            _ => self.err("a float expected"),
        }
    }

    /// Steps over one value, checking it as `value` does, and returns its bytes.
    pub fn skip(&mut self) -> Result<&'a [u8], String> {
        let start = self.at;
        self.skip_in(0)?;
        Ok(self.since(start))
    }

    fn skip_in(&mut self, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return self.err("nested too deep");
        }
        let ib = self.peek()?;
        match ib >> 5 {
            4 => {
                let n = self.head()?.1;
                let n = self.len(n)?;
                for _ in 0..n {
                    self.skip_in(depth + 1)?;
                }
                Ok(())
            }
            5 => {
                let n = self.head()?.1;
                let n = self.len(n)?;
                let mut last: Option<&str> = None;
                for _ in 0..n {
                    let k = self.str()?;
                    if let Some(l) = last
                        && key_order(l, k) != core::cmp::Ordering::Less
                    {
                        return self.err("map keys out of DAG-CBOR order, or repeated");
                    }
                    last = Some(k);
                    self.skip_in(depth + 1)?;
                }
                Ok(())
            }
            // Scalars, strings, bytes and CIDs are small: decode and drop.
            _ => self.value(depth).map(|_| ()),
        }
    }

    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("DAG-CBOR at byte {}: {m}", self.at))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.b.len() - self.at < n {
            return self.err("the input ends inside a value");
        }
        let b: &'a [u8] = self.b;
        let s = &b[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }

    /// A head: (major type, argument), the argument minimally encoded.
    fn head(&mut self) -> Result<(u8, u64), String> {
        let ib = self.take(1)?[0];
        let (major, info) = (ib >> 5, ib & 0x1f);
        let n = match info {
            0..=23 => return Ok((major, info as u64)),
            24 => self.take(1)?[0] as u64,
            25 => u16::from_be_bytes(self.take(2)?.try_into().expect("2")) as u64,
            26 => u32::from_be_bytes(self.take(4)?.try_into().expect("4")) as u64,
            27 => u64::from_be_bytes(self.take(8)?.try_into().expect("8")),
            _ => return self.err("an indefinite length or reserved head"),
        };
        if major != 7 {
            let min = match info {
                24 => 24,
                25 => 0x100,
                26 => 0x1_0000,
                _ => 0x1_0000_0000,
            };
            if n < min {
                return self.err("an integer or length not in its shortest form");
            }
        }
        Ok((major, n))
    }

    fn len(&self, n: u64) -> Result<usize, String> {
        // Every element takes at least a byte, so a length past the input is malformed (and not allocated for).
        if n > (self.b.len() - self.at) as u64 {
            return self.err("a length past the end of the input");
        }
        Ok(n as usize)
    }

    fn text(&mut self, n: u64) -> Result<String, String> {
        let n = self.len(n)?;
        let s = self.take(n)?;
        match std::str::from_utf8(s) {
            Ok(s) => Ok(s.to_owned()),
            Err(_) => self.err("a string that is not UTF-8"),
        }
    }

    pub fn value(&mut self, depth: usize) -> Result<Ipld, String> {
        if depth > MAX_DEPTH {
            return self.err("nested too deep");
        }
        let start = self.at;
        let ib = *self
            .b
            .get(self.at)
            .ok_or("DAG-CBOR: the input ends before a value")?;
        if ib >> 5 == 7 {
            self.at += 1;
            return match ib & 0x1f {
                20 => Ok(Ipld::Bool(false)),
                21 => Ok(Ipld::Bool(true)),
                22 => Ok(Ipld::Null),
                27 => {
                    let f = f64::from_be_bytes(self.take(8)?.try_into().expect("8"));
                    if !f.is_finite() {
                        self.err("a NaN or infinite float")
                    } else if f == 0.0 && f.is_sign_negative() {
                        self.err("-0.0 (written as 0.0)")
                    } else {
                        Ok(Ipld::Float(f))
                    }
                }
                25 | 26 => self.err("a float narrower than 64 bits"),
                _ => self.err("a simple value other than false, true or null"),
            };
        }
        let (major, n) = self.head()?;
        match major {
            0 => Ok(Ipld::Integer(n as i128)),
            1 => Ok(Ipld::Integer(-1 - n as i128)),
            2 => {
                let n = self.len(n)?;
                Ok(Ipld::Bytes(self.take(n)?.to_vec()))
            }
            3 => self.text(n).map(Ipld::String),
            4 => {
                let n = self.len(n)?;
                let mut l = Vec::with_capacity(n);
                for _ in 0..n {
                    l.push(self.value(depth + 1)?);
                }
                Ok(Ipld::List(l))
            }
            5 => {
                let n = self.len(n)?;
                let mut m = std::collections::BTreeMap::new();
                let mut last: Option<String> = None;
                for _ in 0..n {
                    let (km, kn) = self.head()?;
                    if km != 3 {
                        return self.err("a map key that is not a string");
                    }
                    let k = self.text(kn)?;
                    if let Some(l) = &last
                        && key_order(l, &k) != core::cmp::Ordering::Less
                    {
                        return self.err("map keys out of DAG-CBOR order, or repeated");
                    }
                    let v = self.value(depth + 1)?;
                    last = Some(k.clone());
                    m.insert(k, v);
                }
                Ok(Ipld::Map(m))
            }
            6 => {
                if n != 42 {
                    return self.err("a tag other than 42");
                }
                let (bm, bn) = self.head()?;
                if bm != 2 {
                    return self.err("tag 42 over something other than bytes");
                }
                let bn = self.len(bn)?;
                let raw = self.take(bn)?;
                match raw.split_first() {
                    Some((0, c)) => match Cid::try_from(c) {
                        Ok(c) => Ok(Ipld::Link(c)),
                        Err(e) => Err(format!("DAG-CBOR at byte {start}: a bad CID: {e}")),
                    },
                    _ => Err(format!(
                        "DAG-CBOR at byte {start}: a CID without its 0x00 prefix"
                    )),
                }
            }
            _ => unreachable!("major 7 handled above"),
        }
    }
}

#[cfg(test)]
pub fn to_vec(v: &Ipld) -> Vec<u8> {
    let mut out = Vec::new();
    ipld(&mut out, v);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_serde() {
        let cid = crate::dag::cid_of(b"x");
        let mut big = std::collections::BTreeMap::new();
        for k in [
            "b",
            "aa",
            "a",
            "components",
            "id",
            "isa",
            "name",
            "tags",
            "pairs",
            "parent",
            "",
            "zz",
        ] {
            big.insert(k.to_string(), Ipld::Integer(k.len() as i128));
        }
        let vals = [
            Ipld::Null,
            Ipld::Bool(true),
            Ipld::Bool(false),
            Ipld::Integer(0),
            Ipld::Integer(23),
            Ipld::Integer(24),
            Ipld::Integer(255),
            Ipld::Integer(256),
            Ipld::Integer(65535),
            Ipld::Integer(65536),
            Ipld::Integer(u32::MAX as i128 + 1),
            Ipld::Integer(u64::MAX as i128),
            Ipld::Integer(-1),
            Ipld::Integer(-25),
            Ipld::Integer(i64::MIN as i128),
            Ipld::Integer(-(u64::MAX as i128) - 1),
            Ipld::Float(0.0),
            Ipld::Float(-0.0),
            Ipld::Float(1.5),
            Ipld::Float(f64::MAX),
            Ipld::String(String::new()),
            Ipld::String("x".repeat(300)),
            Ipld::Bytes(vec![1; 70000]),
            Ipld::List(vec![Ipld::Null; 30]),
            Ipld::Map(big),
            Ipld::Link(cid),
        ];
        for v in vals {
            let b = to_vec(&v);
            assert_eq!(b, serde_ipld_dagcbor::to_vec(&v).unwrap(), "{v:?}");
            assert_eq!(decode(&b).unwrap(), v, "decoding {v:?}");
        }
        for (bad, what) in [
            (&[0x18, 0x05][..], "not shortest"),
            (&[0xf9, 0, 0][..], "half float"),
            (&[0xfb, 0x80, 0, 0, 0, 0, 0, 0, 0][..], "-0.0"),
            (&[0xa2, 0x61, 0x62, 0x00, 0x61, 0x61, 0x00][..], "key order"),
            (&[0x9f, 0xff][..], "indefinite"),
            (&[0x01, 0x01][..], "trailing"),
            (&[0xc1, 0x00][..], "tag 1"),
            (&[0x5a, 0xff, 0xff, 0xff, 0xff][..], "length past the end"),
        ] {
            assert!(decode(bad).is_err(), "{what}");
        }
    }
}
