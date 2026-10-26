//! Focused XDR primitives (RFC 4506). All variable lengths are checked before slicing.

#[derive(Debug)]
pub(crate) struct Malformed;
pub(crate) type Result<T, E = Malformed> = std::result::Result<T, E>;

pub(crate) struct Reader<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    pub(crate) fn fixed(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(length).ok_or(Malformed)?;
        let value = self.input.get(self.pos..end).ok_or(Malformed)?;
        self.pos = end;
        Ok(value)
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.fixed(4)?.try_into().map_err(|_| Malformed)?,
        ))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.fixed(8)?.try_into().map_err(|_| Malformed)?,
        ))
    }

    pub(crate) fn boolean(&mut self) -> Result<bool> {
        match self.u32()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Malformed),
        }
    }

    pub(crate) fn opaque(&mut self, maximum: usize) -> Result<&'a [u8]> {
        let length = self.u32()? as usize;
        if length > maximum {
            return Err(Malformed);
        }
        let value = self.fixed(length)?;
        let padding = self.fixed((4 - length % 4) % 4)?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(Malformed);
        }
        Ok(value)
    }

    pub(crate) fn finish(&self) -> Result<()> {
        if self.pos == self.input.len() {
            Ok(())
        } else {
            Err(Malformed)
        }
    }
}

#[derive(Default)]
pub(crate) struct Writer(pub(crate) Vec<u8>);

impl Writer {
    pub(crate) fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn boolean(&mut self, value: bool) {
        self.u32(u32::from(value));
    }

    pub(crate) fn fixed(&mut self, value: &[u8]) {
        self.0.extend_from_slice(value);
    }

    pub(crate) fn opaque(&mut self, value: &[u8]) {
        self.u32(value.len() as u32);
        self.fixed(value);
        self.0.resize(self.0.len() + (4 - value.len() % 4) % 4, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unbounded_truncated_and_noncanonical_values() {
        assert!(Reader::new(&u32::MAX.to_be_bytes()).opaque(64).is_err());
        assert!(Reader::new(&[0, 0, 0, 3, 1, 2]).opaque(64).is_err());
        assert!(Reader::new(&[0, 0, 0, 1, 7, 1, 0, 0]).opaque(64).is_err());
        assert!(Reader::new(&2_u32.to_be_bytes()).boolean().is_err());
        assert!(Reader::new(&[0]).finish().is_err());
        let mut writer = Writer::default();
        writer.opaque(b"abc");
        writer.u64(u64::MAX);
        let mut reader = Reader::new(&writer.0);
        assert_eq!(reader.opaque(3).unwrap(), b"abc");
        assert_eq!(reader.u64().unwrap(), u64::MAX);
        reader.finish().unwrap();
    }
}
