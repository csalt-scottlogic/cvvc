use anyhow::{anyhow, Context};
use std::{fmt::Display, io::Read, str::FromStr};
use url::{self, Url};

use crate::{
    repo::is_partial_object_id,
    stores::{RefSpec, TargetedRef},
};

/// A Git pkt-line, sent or received over the network.
#[derive(Debug, PartialEq)]
pub enum PktLine {
    /// A flush packet, sent and received as "0000".
    Flush,

    /// A line, without its length header
    Line(Vec<u8>),
}

impl Display for PktLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PktLine::Flush => write!(f, "0000"),
            PktLine::Line(x) => write!(f, "{:04x} {}", x.len() + 4, String::from_utf8_lossy(x)),
        }
    }
}

struct PktLineIterator<R: Read> {
    reader: R,
    has_ended: bool,
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
            None => {
                self.has_ended = true;
                return None;
            }
            Some(Err(e)) => return Some(Err(e.into())),
            _ => (),
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
        PktLineIterator {
            reader: value,
            has_ended: false,
        }
    }
}

/// The result of a ref discovery process from a remote server.
pub struct RemoteServerInfo {
    /// A list of the advertised refs on this server
    pub refs: Vec<TargetedRef>,

    /// A list of the advertised capabilities of this server.
    pub capabilities: Vec<String>,
}

/// Load the advertised capabilities and refs of a remote server
///
/// The `base_url` parameter should be the server URL entered by the user, without `info/refs` added.
pub fn fetch_remote_refs(base_url: &str) -> Result<RemoteServerInfo, anyhow::Error> {
    let base_url = if base_url.ends_with("/") {
        base_url.to_string()
    } else {
        base_url.to_string() + "/"
    };
    let parsed_base = Url::parse(&base_url).context("remote url is invalid")?;
    let ref_url = parsed_base.join("info/refs?service=git-upload-pack")?;
    println!("Disccovery URL is {ref_url}");
    let response = reqwest::blocking::get(ref_url)?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "Request failed: {} {}",
            response.status(),
            response.text()?
        ));
    }
    let lines = PktLineIterator::from(response);
    let mut seen_header = false;
    let mut capabilities: Vec<String> = vec![];
    let mut refs: Vec<TargetedRef> = vec![];
    for line in lines {
        let line = line.context("couldn't parse pkt-line")?;
        if !seen_header {
            if !matches!(line, PktLine::Flush) {
                continue;
            }
            seen_header = true;
        } else if let PktLine::Line(line_contents) = line {
            if line_contents[40] != 32 {
                return Err(anyhow!("line format: could not find space"));
            }
            let target_id =
                String::from_utf8(line_contents[..40].to_vec()).context("invalid target ID")?;
            if !is_partial_object_id(&target_id) {
                return Err(anyhow!("invalid target id {}", target_id));
            }
            let mut line_end = if line_contents.last() == Some(&0xa) {
                line_contents.len() - 1
            } else {
                line_contents.len()
            };
            let cap_list_start = line_contents.iter().position(|x| *x == 0);
            if let Some(cap_list_start) = cap_list_start {
                let cap_list =
                    String::from_utf8(line_contents[(cap_list_start + 1)..line_end].to_vec())?;
                capabilities.append(&mut cap_list.split(" ").map(|x| x.to_string()).collect());
                line_end = cap_list_start;
            }
            let refspec = String::from_utf8(line_contents[41..line_end].to_vec())?;
            if refspec != "HEAD" && !refspec.ends_with("^{}") {
                let spec = RefSpec::from_str(&refspec)?;
                refs.push(TargetedRef { target_id, spec });
            } else {
                println!("Skipping ref '{refspec}' (this is a to-do)");
            }
        }
    }
    Ok(RemoteServerInfo { refs, capabilities })
}

#[cfg(test)]
mod tests {
    use super::{PktLine, PktLineIterator};

    #[test]
    fn iterator_succeeds_on_valid_data() {
        let test_data = b"000dBiscuits\x0a000aCakes\x0a";

        let test_object: PktLineIterator<_> = (test_data[..]).into();
        let test_output = test_object.map(|x| x.unwrap()).collect::<Vec<PktLine>>();

        assert_eq!(
            test_output,
            vec![
                PktLine::Line(b"Biscuits\x0a".to_vec()),
                PktLine::Line(b"Cakes\x0a".to_vec())
            ]
        );
    }

    #[test]
    fn iterator_succeeds_on_valid_data_with_flush() {
        let test_data = b"000dBiscuits\x0a0000000aCakes\x0a";

        let test_object: PktLineIterator<_> = (test_data[..]).into();
        let test_output = test_object.map(|x| x.unwrap()).collect::<Vec<PktLine>>();

        assert_eq!(
            test_output,
            vec![
                PktLine::Line(b"Biscuits\x0a".to_vec()),
                PktLine::Flush,
                PktLine::Line(b"Cakes\x0a".to_vec())
            ]
        );
    }
}
