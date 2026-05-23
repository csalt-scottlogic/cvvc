use std::io::Read;

/// A Git pkt-line, sent or received over the network.
#[derive(Debug, PartialEq)]
pub enum PktLine {
    /// A flush packet, sent and received as "0000".
    Flush,

    /// A line, without its length header
    Line(Vec<u8>)
}

struct PktLineIterator<R: Read> {
    reader: R,
    has_ended: bool
}

impl<R: Read> PktLineIterator<R> {
    // A std::io::Read::read_exact variant that returns an option.
    // 
    // It returns `None` if the first `read` call returns `Ok(0)`, to indicate
    // EOF has been reached without having to sniff the error kind.  It returns 
    // `Some(Err(...))` for other error conditions, including EOF on subsequent reads.
    fn read_exact(&mut self, mut buf: &mut [u8]) -> Option<Result<(), std::io::Error>> {
        match self.reader.read(buf) {
            Ok(0) => return None,
            Err(e) => return Some(Err(e)),
            Ok(x) => buf = &mut buf[x..],
        }
        Some(self.reader.read_exact(buf))
    }
}

impl<R: Read> Iterator for PktLineIterator<R> {
    type Item = Result<PktLine, anyhow::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.has_ended {
            return None;
        }
        let mut buf = [0u8; 4];
        match self.read_exact(&mut buf) {
            None => {self.has_ended = true; return None},
            Some(Err(e)) => return Some(Err(e.into())),
            _ => ()
        };
        let len_str = match str::from_utf8(&buf) {
            Ok(x) => x,
            Err(e) => {
                self.has_ended = true;
                return Some(Err(e.into()));
            }
        };
        let len = match usize::from_str_radix(&len_str, 16) {
            Ok(0) => return Some(Ok(PktLine::Flush)),
            Ok(x) => x - 4,
            Err(e) => {
                self.has_ended = true;
                return Some(Err(e.into()));
            }
        };

        let mut line_buf = vec![0u8; len];
        match self.reader.read_exact(&mut line_buf) {
            Ok(()) => Some(Ok(PktLine::Line(line_buf))),
            Err(e) => {
                self.has_ended = true;
                Some(Err(e.into()))
            }
        }
    }
}

impl<R: Read> From<R> for PktLineIterator<R> {
    fn from(value: R) -> Self {
        PktLineIterator { reader: value, has_ended: false }
    }
}

#[cfg(test)]
mod tests {
    use super::{PktLine, PktLineIterator};

    #[test]
    fn iterator_succeeds_on_valid_data() {
        let test_data = b"000dBiscuits\x0a000aCakes\x0a";

        let test_object: PktLineIterator<_> = (test_data[..]).into();
        let test_output = test_object.map(|x| x.unwrap()).collect::<Vec<PktLine>>();

        assert_eq!(test_output, vec![PktLine::Line(b"Biscuits\x0a".to_vec()), PktLine::Line(b"Cakes\x0a".to_vec())]);
    }

    #[test]
    fn iterator_succeeds_on_valid_data_with_flush() {
        let test_data = b"000dBiscuits\x0a0000000aCakes\x0a";

        let test_object: PktLineIterator<_> = (test_data[..]).into();
        let test_output = test_object.map(|x| x.unwrap()).collect::<Vec<PktLine>>();

        assert_eq!(test_output, vec![PktLine::Line(b"Biscuits\x0a".to_vec()), PktLine::Flush, PktLine::Line(b"Cakes\x0a".to_vec())]);
    }
}
