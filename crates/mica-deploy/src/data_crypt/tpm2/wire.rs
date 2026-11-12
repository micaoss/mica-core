//! TPM 2.0 marshalling: big-endian integers and size-prefixed buffers, with
//! every read bounds-checked (a response is untrusted input).

pub type Res<T> = Result<T, String>;

#[derive(Default)]
pub struct W(pub Vec<u8>);

impl W {
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn raw(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }

    /// A TPM2B: u16 size, then the bytes. Every buffer built here is far
    /// below 64 KiB.
    pub fn b(&mut self, v: &[u8]) -> &mut Self {
        self.u16(u16::try_from(v.len()).unwrap_or(u16::MAX));
        self.raw(v)
    }
}

pub struct R<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> R<'a> {
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    pub fn take(&mut self, n: usize) -> Res<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.buf.len())
            .ok_or_else(|| {
                format!(
                    "short response: {n} bytes wanted at {}, {} held",
                    self.at,
                    self.buf.len()
                )
            })?;
        let out = &self.buf[self.at..end];
        self.at = end;
        Ok(out)
    }

    pub fn u8(&mut self) -> Res<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Res<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Res<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn b(&mut self) -> Res<&'a [u8]> {
        let n = self.u16()? as usize;
        self.take(n)
    }

    pub fn rest(&mut self) -> &'a [u8] {
        let out = &self.buf[self.at..];
        self.at = self.buf.len();
        out
    }

    pub fn done(&self) -> Res<()> {
        if self.at == self.buf.len() {
            Ok(())
        } else {
            Err(format!("{} trailing bytes", self.buf.len() - self.at))
        }
    }

    pub const fn pos(&self) -> usize {
        self.at
    }
}
