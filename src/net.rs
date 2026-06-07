use anyhow::{anyhow, Context};
use reqwest::blocking::{Client, Response};
use std::{collections::VecDeque, fmt::Display, io::Read, str::FromStr};
use url::{self, Url};

use crate::{
    helpers::escaped_byte_string,
    repo::is_partial_object_id,
    stores::{RefSpec, RefTarget, TargetedRef},
};

/// A Git pkt-line, sent or received over the network.
#[derive(Debug, PartialEq)]
pub enum PktLine {
    /// A flush packet, sent and received as "0000".
    Flush,

    /// A delimiter packet, senf and received as "0001".
    Delimiter,

    /// A response-end packet, sent and received as "0002".
    ResponseEnd,

    /// A line, without its length header
    Line(Vec<u8>),
}

impl Display for PktLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PktLine::Flush => write!(f, "0000"),
            PktLine::Delimiter => write!(f, "0001"),
            PktLine::ResponseEnd => write!(f, "0002"),
            PktLine::Line(x) => write!(f, "{:04x}{}", x.len() + 4, escaped_byte_string(x)),
        }
    }
}

impl From<&str> for PktLine {
    fn from(value: &str) -> Self {
        Self::Line(value.bytes().collect())
    }
}

impl From<&[u8]> for PktLine {
    fn from(value: &[u8]) -> Self {
        Self::Line(value.to_vec())
    }
}

impl PktLine {
    fn bytes(&self) -> PktLineByteIterator {
        PktLineByteIterator::new(self)
    }
}

struct PktLineByteIterator {
    header: VecDeque<u8>,
    line_data: VecDeque<u8>
}

impl PktLineByteIterator {
    fn new(line: &PktLine) -> Self {
        let (header, line_data) = match line {
            PktLine::Flush => (vec![48, 48, 48, 48], vec![]),
            PktLine::Delimiter => (vec![48, 48, 48, 49], vec![]),
            PktLine::ResponseEnd => (vec![48, 48, 48, 50], vec![]),
            PktLine::Line(v) => (format!("{:04x}", v.len() + 4).bytes().collect(), v.clone()),
        };
        Self {
            header: header.into(),
            line_data: line_data.into(),
        }
    }
}

impl Iterator for PktLineByteIterator {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.header.is_empty() {
            self.header.pop_front()
        } else {
            self.line_data.pop_front()
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
        let len = match usize::from_str_radix(len_str, 16) {
            Ok(0) => return Some(Ok(PktLine::Flush)),
            Ok(1) => return Some(Ok(PktLine::Delimiter)),
            Ok(2) => return Some(Ok(PktLine::ResponseEnd)),
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
    pub capabilities: Vec<RemoteCapability>,
}

/// A capability of a remote server, consisting either of a single string, or a key and a number of values.
///
/// In the version 1 protocol, most capabilities are a single string without additional values.  In the version
/// 2 protocol, most capabilities are keys and values, with the key being a command the server accepts and
/// the values specifying the individual capabilities of that command.
#[derive(Clone)]
pub struct RemoteCapability {
    key: String,
    values: Vec<String>,
}

impl Display for RemoteCapability {
    // Allowed because we need to ensure we always write a LF, not a platform-dependent EOL
    #[allow(clippy::write_with_newline)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.values.len() {
            0 => self.key.fmt(f),
            _ => write!(f, "{}={}", self.key, self.values.join(" ")),
        }
    }
}

impl FromStr for RemoteCapability {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let sep = s.bytes().position(|x| x == 0x3d); // split on '='
        match sep {
            None => Ok(Self {
                key: s.to_string(),
                values: vec![],
            }),
            Some(x) => Ok(Self {
                key: s[..x].to_string(),
                values: s[(x + 1)..].trim().split(" ").map(|v| v.to_string()).collect(),
            }),
        }
    }
}

/// The version of the Git protocol in use.
#[derive(PartialEq)]
pub enum ProtocolVersion {
    /// Protocol version 1.
    V1,

    /// Protocol version 2.
    V2,
}

impl TryFrom<u32> for ProtocolVersion {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::V1),
            2 => Ok(Self::V2),
            x => Err(anyhow!("invalid protocol version {x}")),
        }
    }
}

impl From<&ProtocolVersion> for u32 {
    fn from(value: &ProtocolVersion) -> Self {
        match value {
            ProtocolVersion::V1 => 1,
            ProtocolVersion::V2 => 2,
        }
    }
}

impl Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", u32::from(self))
    }
}

/// The HTTP client used for fetching refs and packs from a remote server.
pub struct HttpFetchClient {
    client: Client,
    base_url: Url,
    version: Option<ProtocolVersion>,
    capabilities: Vec<RemoteCapability>,
}

impl HttpFetchClient {
    /// Create a new fetch client.
    ///
    /// The `base_url` parameter is normalised to end in a slash, if it does not already.
    ///
    /// If the `version` parameter is `None`, the client will attempt to sniff the supported
    /// server protocol, starting by making a Version 2 connection and switching to Version 1
    /// if it gets a Version 1 response.  If the `version` parameter is `Some(x)`, the client will
    /// only use the specified protocol version, and will error if the client does not appear to
    /// support it.
    ///
    /// # Errors
    ///
    /// This function returns an error if the `base_url` parameter cannot be parsed as a [`Url`].
    pub fn new(base_url: &str, version: Option<ProtocolVersion>) -> Result<Self, anyhow::Error> {
        let client = Client::new();
        let base_url = if base_url.ends_with("/") {
            base_url.to_string()
        } else {
            base_url.to_string() + "/"
        };
        let base_url = Url::parse(&base_url).context("remote url is invalid")?;
        Ok(Self {
            client,
            base_url,
            version,
            capabilities: vec![],
        })
    }

    /// Get the protocol version which will be used for the next request.
    pub fn version(&self) -> &ProtocolVersion {
        self.version.as_ref().unwrap_or(&ProtocolVersion::V2)
    }

    /// Fetch the server's capabilities and refs.
    ///
    /// These are grouped together in the API, because they are a single network request in the
    /// version 1 protocol.
    ///
    /// If the client's protocol has already been set, that protocol will be used.  If not, the code will
    /// send a protocol version 2 request, and determine whether it gets a version 1 or version 2 response
    /// in return.  If it gets a version 2 request, it will send a second network request to get the remote
    /// refs; and it will use version 2 requests for all operations from that point.
    ///
    /// # Errors
    ///
    /// This method can return an error if there are any issues with the network connection, or if there is
    /// an unexpected or unparseable response from the server.
    pub fn fetch_refs_capabilities(
        &mut self,
        net_dump: bool,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        match self.version {
            Some(ProtocolVersion::V1) => self.fetch_refs_capabilities_v1(net_dump),
            Some(ProtocolVersion::V2) => self.fetch_refs_capabilities_v2(net_dump),
            None => self.fetch_refs_capabilities_sniff_protocol(net_dump),
        }
    }

    fn make_initial_fetch_request(
        &self,
        net_dump: bool,
    ) -> Result<(ProtocolVersion, PktLine, PktLineIterator<Response>), anyhow::Error> {
        let discovery_url = self.base_url.join("info/refs?service=git-upload-pack")?;
        if net_dump {
            println!("Discovery URL is {discovery_url}");
        }
        let mut request = self.client.get(discovery_url);
        if *self.version() == ProtocolVersion::V2 {
            request = request.header("Git-Protocol", "version=2");
        }
        let response = request.send()?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Request failed: {} {}",
                response.status(),
                response.text()?
            ));
        }
        let mut lines = PktLineIterator::from(response);
        if !Self::unwrap_and_test_line(
            lines.next(),
            &PktLine::Line(b"# service=git-upload-pack\x0a".to_vec()),
            net_dump,
        )? {
            return Err(anyhow!("response header not found"));
        }
        if !Self::unwrap_and_test_line(lines.next(), &PktLine::Flush, net_dump)? {
            return Err(anyhow!("end of header not found"));
        }
        let first_line = Self::unwrap_line(lines.next(), net_dump)?;
        let detected_version = if first_line == PktLine::Line(b"version 2\x0a".to_vec()) {
            ProtocolVersion::V2
        } else {
            ProtocolVersion::V1
        };
        Ok((detected_version, first_line, lines))
    }

    fn unwrap_line(
        line: Option<Result<PktLine, anyhow::Error>>,
        net_dump: bool,
    ) -> Result<PktLine, anyhow::Error> {
        let Some(line) = line else {
            return Err(anyhow!("unexpected end"));
        };
        let line = line?;
        if net_dump {
            println!("R: {line}");
        }
        Ok(line)
    }

    fn unwrap_and_test_line(
        line: Option<Result<PktLine, anyhow::Error>>,
        test_line: &PktLine,
        net_dump: bool,
    ) -> Result<bool, anyhow::Error> {
        let line = Self::unwrap_line(line, net_dump)?;
        Ok(line == *test_line)
    }

    fn fetch_capabilities_v2(
        &mut self,
        net_dump: bool,
    ) -> Result<Vec<RemoteCapability>, anyhow::Error> {
        let (protocol_version, _, lines) = self.make_initial_fetch_request(net_dump)?;
        if protocol_version != ProtocolVersion::V2 {
            return Err(anyhow!("wrong protocol version"));
        }
        self.load_capabilities_body_v2(lines, net_dump)
    }

    fn fetch_refs_capabilities_v2(
        &mut self,
        net_dump: bool,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        self.capabilities = self.fetch_capabilities_v2(net_dump)?;
        self.fetch_refs_v2(net_dump)
    }

    fn fetch_refs_capabilities_sniff_protocol(
        &mut self,
        net_dump: bool,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        let (protocol_version, first_line, lines) = self.make_initial_fetch_request(net_dump)?;
        self.version = Some(protocol_version);
        match self.version {
            Some(ProtocolVersion::V1) => {
                self.load_refs_capabilities_body_v1(first_line, lines, net_dump)
            }
            Some(ProtocolVersion::V2) => {
                self.capabilities = self.load_capabilities_body_v2(lines, net_dump)?;
                self.fetch_refs_v2(net_dump)
            }
            _ => Err(anyhow!("impossible")),
        }
    }

    fn load_refs_capabilities_body_v1(
        &mut self,
        first_line: PktLine,
        lines: PktLineIterator<Response>,
        net_dump: bool,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        let mut capabilities: Vec<RemoteCapability> = vec![];
        let mut refs: Vec<TargetedRef> = vec![];
        if let PktLine::Line(line_contents) = first_line {
            if let Some(parsed_first_line) =
                Self::load_single_v1_refs_capabilities_line(line_contents, &mut capabilities)?
            {
                refs.push(parsed_first_line);
            }
        }
        for line in lines {
            let line = line.context("couldn't parse pkt-line")?;
            if net_dump {
                println!("R:{line}");
            }
            if let PktLine::Line(line_contents) = line {
                if let Some(parsed_line) =
                    Self::load_single_v1_refs_capabilities_line(line_contents, &mut capabilities)?
                {
                    refs.push(parsed_line);
                }
            }
        }
        self.capabilities = capabilities.clone();
        Ok(RemoteServerInfo { refs, capabilities })
    }

    fn load_single_v1_refs_capabilities_line(
        line_contents: Vec<u8>,
        capabilities: &mut Vec<RemoteCapability>,
    ) -> Result<Option<TargetedRef>, anyhow::Error> {
        let Some(id_len) = line_contents.iter().position(|x| *x == 32) else {
            return Err(anyhow!("line format: could not find space"));
        };
        let target_id =
            String::from_utf8(line_contents[..id_len].to_vec()).context("invalid target ID")?;
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
            for cap in cap_list.split(" ") {
                capabilities.push(RemoteCapability::from_str(cap)?);
            }
            line_end = cap_list_start;
        }
        let refspec = String::from_utf8(line_contents[(id_len + 1)..line_end].to_vec())?;
        let spec = RefSpec::from_str(&refspec).ok();
        Ok(spec.map(|s| TargetedRef {
            target: RefTarget::Object(target_id),
            spec: s,
        }))
    }

    fn load_capabilities_body_v2(
        &mut self,
        lines: PktLineIterator<Response>,
        net_dump: bool,
    ) -> Result<Vec<RemoteCapability>, anyhow::Error> {
        let mut results = vec![];
        for line in lines {
            let line = line?;
            if net_dump {
                println!("R: {line}");
            }
            if let PktLine::Line(content) = line {
                results.push(RemoteCapability::from_str(&String::from_utf8_lossy(
                    &content,
                ))?);
            }
        }
        if net_dump {
            println!("Capabilities:");
            for c in &results {
                println!("\t{c}");
            }
        }
        Ok(results)
    }

    fn fetch_refs_v2(&self, net_dump: bool) -> Result<RemoteServerInfo, anyhow::Error> {
        let ref_url = self.base_url.join("git-upload-pack")?;
        let ls_refs_args = self.capabilities.iter().filter(|c| c.key == "ls-refs").next();
        let Some(ls_refs_args) = ls_refs_args else {
            return Err(anyhow!("server does not support ls-refs command"));
        };
        let mut body_lines = vec![PktLine::from("command=ls-refs\x0a")];
        if self.capabilities.iter().any(|c| c.key == "agent") {
            body_lines.push(PktLine::from(format!("agent=cvvc/{}\x0a", env!("CARGO_PKG_VERSION")).as_str()));
        }
        body_lines.push(PktLine::Delimiter);
        body_lines.push(PktLine::from("symrefs\x0a"));
        body_lines.push(PktLine::from("peel\x0a"));
        if ls_refs_args.values.contains(&"unborn".to_string()) {
            body_lines.push(PktLine::from("unborn\x0a"));
        }
        body_lines.push(PktLine::Flush);
        if net_dump {
            for line in &body_lines {
                println!("S: {line}");
            }
        }
        let body = body_lines.iter().map(|n| n.bytes()).flatten().collect::<Vec<u8>>();
        let request = self.client.post(ref_url).header("Git-Protocol", "version=2").body(body);
        let response = request.send()?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Request failed: {} {}",
                response.status(),
                response.text()?
            ));
        }
        let lines = PktLineIterator::from(response);
        let mut refs = Vec::<TargetedRef>::new();
        for line in lines {
            let line = Self::unwrap_line(Some(line), net_dump)?;
            let PktLine::Line(line_contents) = line else {
                continue;
            };
            let Some(id_len) = line_contents.iter().position(|x| *x == 32) else {
                return Err(anyhow!("line format: could not find space"));
            };
            let target_id = String::from_utf8(line_contents[..id_len].to_vec()).context("invalid target ID")?;
            let full_refspec = String::from_utf8(line_contents[id_len..].to_vec())?;
            let mut ref_parts = full_refspec.trim().split(" ");
            let Some(primary_ref_str) = ref_parts.next() else {
                continue;
            };
            let Ok(primary_ref_spec) = RefSpec::from_str(primary_ref_str) else {
                continue;
            };
            if let Some(secondary_ref_string) = ref_parts.next() {
                if let Some(peeled_ref_target) = secondary_ref_string.strip_prefix("peeled:") {
                    if let Some(peeled_ref_spec) = primary_ref_spec.peel_tag() {
                        refs.push(TargetedRef { target: RefTarget::Object(peeled_ref_target.to_string()), spec: peeled_ref_spec })
                    }
                    refs.push(TargetedRef { target: RefTarget::Object(target_id), spec: primary_ref_spec });
                } else if let Some(symref_ref_target) = secondary_ref_string.strip_prefix("symref-target:") {
                    refs.push(TargetedRef { target: RefTarget::SymbolicRef(RefSpec::from_str(symref_ref_target)?), spec: primary_ref_spec });
                    refs.push(TargetedRef { target: RefTarget::Object(target_id), spec: RefSpec::from_str(symref_ref_target)?});
                } else {
                    refs.push(TargetedRef { target: RefTarget::Object(target_id), spec: primary_ref_spec });
                }
            } else {
                refs.push(TargetedRef { target: RefTarget::Object(target_id), spec: primary_ref_spec });
            }
        }
        let capabilities = self.capabilities.clone();
        Ok(RemoteServerInfo {
            refs,
            capabilities,
        })
    }

    fn fetch_refs_capabilities_v1(
        &mut self,
        net_dump: bool,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        let (detected_version, first_line, lines) = self.make_initial_fetch_request(net_dump)?;
        if detected_version != ProtocolVersion::V1 {
            return Err(anyhow!("wrong protocol version detected"));
        }
        self.load_refs_capabilities_body_v1(first_line, lines, net_dump)
    }
}

enum PackFetchCommand {
    Want(String),
    Have(String),
}

impl From<&PackFetchCommand> for PktLine {
    fn from(value: &PackFetchCommand) -> Self {
        let command = match value {
            PackFetchCommand::Want(id) => format!("want {id}\x0a"),
            PackFetchCommand::Have(id) => format!("have {id}\x0a"),
        };
        Self::Line(command.bytes().collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::net::PackFetchCommand;

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

    #[test]
    fn pkt_line_from_pack_fetch_command_succeeds_for_have() {
        let test_input =
            PackFetchCommand::Have("1234123412341234123412341234123412341234".to_string());

        let test_output = PktLine::from(&test_input);

        assert_eq!(
            PktLine::Line(vec![
                104, 97, 118, 101, 32, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51,
                52, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51, 52,
                49, 50, 51, 52, 10
            ]),
            test_output
        );
    }

    #[test]
    fn pkt_line_from_pack_fetch_command_succeeds_for_want() {
        let test_input =
            PackFetchCommand::Want("1234123412341234123412341234123412341234".to_string());

        let test_output = PktLine::from(&test_input);

        assert_eq!(
            PktLine::Line(vec![
                119, 97, 110, 116, 32, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51,
                52, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51, 52, 49, 50, 51, 52,
                49, 50, 51, 52, 10
            ]),
            test_output
        );
    }

    #[test]
    fn pkt_line_byte_iterator_works_for_flush_packet() {
        let test_input = PktLine::Flush;

        let test_output: Vec<u8> = test_input.bytes().collect();

        assert_eq!(*b"0000", *test_output);
    }

    #[test]
    fn pkt_line_byte_iterator_works_for_delim_packet() {
        let test_input = PktLine::Delimiter;

        let test_output: Vec<u8> = test_input.bytes().collect();

        assert_eq!(*b"0001", *test_output);
    }

    #[test]
    fn pkt_line_byte_iterator_works_for_line_packet() {
        let test_input = PktLine::Line(b"command=ls-refs\x0a".to_vec());

        let test_output: Vec<u8> = test_input.bytes().collect();

        assert_eq!(*b"0014command=ls-refs\x0a", *test_output);
    }
}
