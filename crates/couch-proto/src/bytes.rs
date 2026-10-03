//! Minimal big-endian reader and writer for fixed-layout headers.

pub(crate) struct Writer<'a>(pub &'a mut Vec<u8>);

impl Writer<'_> {
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
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
}

pub(crate) struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }
    pub fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|b| b[0])
    }
    pub fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_be_bytes)
    }
    pub fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_be_bytes)
    }
    pub fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_be_bytes)
    }
    pub fn rest(self) -> &'a [u8] {
        self.0
    }
}
