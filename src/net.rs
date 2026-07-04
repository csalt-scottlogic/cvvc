use anyhow::{anyhow, Context};
use indexmap::IndexSet;
use reqwest::blocking::{Client, RequestBuilder, Response};
use std::{
    collections::{HashSet, VecDeque},
    fmt::Display,
    io::{self, Read},
    str::FromStr,
};
use url::{self, Url};

use crate::{
    helpers::escaped_byte_string,
    output::{OutputMessage, OutputService},
    repo::{is_partial_object_id, CommitIterator, Repository},
    stores::{RefSpec, RefTarget, TargetedRef},
};

type ReportingFn = Option<fn(&[u8])>;

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

impl From<&String> for PktLine {
    fn from(value: &String) -> Self {
        Self::from(value.as_str())
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
    line_data: VecDeque<u8>,
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

    fn skip_section(&mut self) -> Result<(), anyhow::Error> {
        loop {
            let Some(next_line) = self.next() else {
                break;
            };
            let next_line = next_line?;
            if !matches!(next_line, PktLine::Line(_)) {
                break;
            }
        }
        Ok(())
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

/// A reader which reads data from a sequence of Git pkt-line sideband packets, stripping their metadata and presenting them as a single
/// sequence of bytes, whilst reporting the content of the progress-message sideband back to the user.
///
/// This reader makes the following assumptions about its input data:
/// - each pkt-line is in the "sideband" format, with the value of the first data byte of the pkt-line indicating the sideband channel number
/// - sideband channel 1 is used for data
/// - sideband channel 2 is used for progress messages
/// - sideband channel 3 is used for error messages
/// - if a pkt-line is received on channel 3, it will be the final pkt-line of the sequence
///
/// Reading from the reader presents the channel 1 data as a continuous stream of bytes, with the line header and sideband number stripped.
///
/// When a pkt-line on channel 2 is encountered, its content is passed to a message handler function which can, for example, log the message or
/// display it to the user.
///
/// When a pkt-line on channel 3 is encountered, its content is passed to the same message handler function as channel 2, and the read immediately
/// terminates with an error.
///
/// If any normal lines without a valid channel number are encountered, they are treated as channel 1 data, but without stripping the invalid channel number.
///
/// Reading terminates successfully on finding a flush line, delimiter line, or response-end line.
///
/// Reading terminates with [`io::ErrorKind::UnexpectedEof`] if the final pkt-line in the sequence is a normal line.
pub struct PktLineSidebandReader {
    iter: PktLineIterator<Response>,
    cur_line: Option<Vec<u8>>,
    cur_pos: usize,
    message_handler: ReportingFn,
}

impl PktLineSidebandReader {
    fn new(iter: PktLineIterator<Response>, message_handler: ReportingFn) -> Self {
        Self {
            iter,
            cur_line: None,
            cur_pos: 0,
            message_handler,
        }
    }

    fn handle_message(&self, line_content: &[u8]) {
        if let Some(mh) = self.message_handler {
            mh(&line_content[1..]);
        }
    }
}

impl Read for PktLineSidebandReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.cur_line.is_none() {
            let the_line = match self.iter.next() {
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        anyhow!("flush or delim not found"),
                    ));
                }
                Some(Err(err)) => {
                    return Err(io::Error::other(err));
                }
                Some(Ok(line)) => line,
            };
            let PktLine::Line(line_content) = the_line else {
                return Ok(0);
            };
            match line_content[0] {
                2 => self.handle_message(&line_content),
                3 => {
                    self.handle_message(&line_content);
                    let msg = String::from_utf8_lossy(&line_content[1..]);
                    return Err(io::Error::other(anyhow!(msg.to_string())));
                }
                1 => {
                    self.cur_line = Some(line_content[1..].to_vec());
                }
                _ => {
                    self.cur_line = Some(line_content);
                }
            }
        }
        let Some(ref buffered) = self.cur_line else {
            return Ok(0);
        };
        let bytes_avail = buffered.len() - self.cur_pos;
        let to_copy = if bytes_avail > buf.len() {
            buf.len()
        } else {
            bytes_avail
        };
        buf[0..to_copy].copy_from_slice(&buffered[self.cur_pos..(self.cur_pos + to_copy)]);
        self.cur_pos += to_copy;
        if self.cur_pos >= buffered.len() {
            self.cur_line = None;
            self.cur_pos = 0;
        }
        Ok(to_copy)
    }
}

/// The result of a ref discovery process from a remote server.
pub struct RemoteServerInfo {
    /// A list of the advertised refs on this server
    pub refs: HashSet<TargetedRef>,
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.values.len() {
            0 => self.key.fmt(f),
            _ => write!(f, "{}={}", self.key, self.values.join(" ")),
        }
    }
}

impl RemoteCapability {
    fn new(key: &str) -> Self {
        Self {
            key: key.to_string(),
            values: vec![],
        }
    }

    fn new_with_values(key: &str, values: Vec<String>) -> Self {
        Self {
            key: key.to_string(),
            values,
        }
    }
}

impl FromStr for RemoteCapability {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let sep = s.bytes().position(|x| x == 0x3d); // split on '='
        match sep {
            None => Ok(Self::new(s)),
            Some(x) => Ok(Self::new_with_values(
                &s[..x],
                s[(x + 1)..]
                    .trim()
                    .split(" ")
                    .map(|v| v.to_string())
                    .collect(),
            )),
        }
    }
}

/// The version of the Git protocol in use.
#[derive(Clone, Copy, PartialEq)]
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
    protocol_version: Option<ProtocolVersion>,
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
    pub fn new(
        base_url: &str,
        protocol_version: Option<ProtocolVersion>,
    ) -> Result<Self, anyhow::Error> {
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
            protocol_version,
            capabilities: vec![],
        })
    }

    /// Get the protocol version which will be used for the next request.
    pub fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version.unwrap_or(ProtocolVersion::V2)
    }

    /// Get the remote server's capability list
    ///
    /// This will be populated after the first request made to the remote server.  
    /// Its content will vary wildly based on whether the first request to the
    /// server used protocol version 1 or 2, because many of the optional capabilities
    /// the server declares on protocol version 1 are are assumed to be supported by
    /// any version 2 server.
    ///
    /// Conversely, on a version 2 server each available server command is described as
    /// a separate capability, whereas on protocol version 1, the available commands
    /// are taken as assumed.
    pub fn capabilities(&self) -> &[RemoteCapability] {
        &self.capabilities
    }

    /// Get the value of a capability, if present.
    ///
    /// Capabilities without values will return an empty vector; if a capability is
    /// not present this method will return `None`.
    pub fn capability(&self, key: &str) -> Option<Vec<&str>> {
        self.capabilities
            .iter()
            .find(|c| c.key == key)
            .map(|rc| rc.values.iter().map(|v| v.as_str()).collect())
    }

    /// Fetch a pack from the remote server.
    ///
    /// This method takes a set of wanted commit IDs, and a reference to the repository which it uses
    /// to generate the set of commit IDs which we already have.  It returns a `Read` implementation
    /// which will return the pack data when read, printing any progress or error messages from the remote server
    /// as it is read.
    ///
    /// This method may make multiple requests to the remote server, as part of the Git pack negotiation protocol.
    ///
    /// # Errors
    ///
    /// This method will return an error under many conditions:
    ///
    /// - if the remote server does not respond
    /// - if the remote server returns an error status code
    /// - if the remote server does not return either a packfile, or a response acknowledging which objects are common
    ///   to remote and local repositories (which could be an empty list)
    /// - if the remote server returns a `ready` response on one request, but does not send a packfile on the following request
    /// - if we send a request ending in `done` to show we want to end negotiation, and the remote server does not respond with a packfile
    ///
    /// Note that this method does not read to the end of the final response to confirm that the packfile is properly terminated with a
    /// "flush" line.  This is handled by the returned reader.
    pub fn fetch_pack(
        &self,
        wants: &HashSet<&str>,
        repo: &Repository,
        printer: &dyn OutputService,
    ) -> Result<PktLineSidebandReader, anyhow::Error> {
        match self.protocol_version {
            Some(ProtocolVersion::V1) => Err(anyhow!("cvvc does not currently support network protocol v1")),
            Some(ProtocolVersion::V2) => self.fetch_pack_v2(wants, repo, printer),
            None => Err(anyhow!("get your client to sniff the protocol via fetch_refs_capabilities() before fetching a pack")),
        }
    }

    fn fetch_pack_v2(
        &self,
        wants: &HashSet<&str>,
        repo: &Repository,
        printer: &dyn OutputService,
    ) -> Result<PktLineSidebandReader, anyhow::Error> {
        let mut common_objects = IndexSet::<String>::new();
        let our_objects = repo.commits(None)?;
        let mut topup_size = our_objects.queue_length();
        let mut our_objects =
            Self::top_up_common_objects(&mut common_objects, our_objects, topup_size)?;
        let mut provide_done = false;
        loop {
            match self.fetch_pack_v2_call(wants, &common_objects, provide_done, printer)? {
                PackFetchResponse::InfoNeeded(acked) => {
                    if provide_done {
                        return Err(anyhow!("ran out of information to send"));
                    }
                    Self::scrub_common_objects(&mut common_objects, acked);
                }
                PackFetchResponse::Ready(acked) => {
                    Self::scrub_common_objects(&mut common_objects, acked);
                    provide_done = true;
                }
                PackFetchResponse::Pack(reader) => {
                    return Ok(reader);
                }
            }
            match our_objects {
                Some(iter) => {
                    topup_size = if topup_size < 400 {
                        topup_size * 2
                    } else {
                        topup_size + 400
                    };
                    our_objects =
                        Self::top_up_common_objects(&mut common_objects, iter, topup_size)?;
                }
                None => {
                    provide_done = true;
                }
            }
        }
    }

    fn fetch_pack_v2_call(
        &self,
        wants: &HashSet<&str>,
        common_objects: &IndexSet<String>,
        provide_done: bool,
        printer: &dyn OutputService,
    ) -> Result<PackFetchResponse, anyhow::Error> {
        let Some(_) = self.capability("fetch") else {
            return Err(anyhow!("server does not support fetch command"));
        };
        let fetch_url = self.base_url.join("git-upload-pack")?;
        let mut body_lines = vec![PktLine::from("command=fetch\x0a")];
        if self.capability("agent").is_some() {
            body_lines.push(PktLine::from(&Self::agent_string()));
        }
        body_lines.push(PktLine::Delimiter);
        body_lines.push(PktLine::from("include-tag\x0a"));
        body_lines.push(PktLine::from("ofs-delta\x0a"));
        body_lines.extend(
            wants
                .iter()
                .map(|s| PackFetchCommand::Want(s.to_string()).into()),
        );
        body_lines.extend(
            common_objects
                .iter()
                .map(|s| PackFetchCommand::Have(s.to_string()).into()),
        );
        if provide_done {
            body_lines.push(PktLine::from("done\x0a"));
        }
        body_lines.push(PktLine::Flush);
        for line in &body_lines {
            printer.println_verbose(&OutputMessage::plain(&format!("S: {line}")));
        }
        let request = add_git_protocol_header(self.client.post(fetch_url)).body(
            body_lines
                .iter()
                .flat_map(|n| n.bytes())
                .collect::<Vec<u8>>(),
        );
        let mut lines = PktLineIterator::from(response_check(request.send()?)?);
        let mut acked_objects = vec![];
        let mut acks_found = false;
        let mut is_ready = false;
        loop {
            let Some(section_header) = lines.next() else {
                break;
            };
            if let PktLine::Line(line_conts) = section_header? {
                if line_conts == b"acknowledgements\x0a" {
                    acks_found = true;
                    acked_objects.append(&mut Self::load_acked_objects(&mut lines)?);
                } else if line_conts == b"packfile\x0a" {
                    let reader = PktLineSidebandReader::new(
                        lines,
                        Some(|m| print!("{}", String::from_utf8_lossy(m))),
                    );
                    return Ok(PackFetchResponse::Pack(reader));
                } else if line_conts == b"ready\x0a" {
                    is_ready = true;
                } else {
                    lines.skip_section()?;
                }
            }
        }
        if !acks_found {
            Err(anyhow!("invalid response: no pack and no acknowledgements"))
        } else if is_ready {
            Ok(PackFetchResponse::Ready(acked_objects))
        } else {
            Ok(PackFetchResponse::InfoNeeded(acked_objects))
        }
    }

    fn load_acked_objects<R: Read>(
        lines: &mut PktLineIterator<R>,
    ) -> Result<Vec<String>, anyhow::Error> {
        let mut objs = vec![];
        loop {
            let Some(next_line) = lines.next() else {
                break;
            };
            let next_line = next_line?;
            let PktLine::Line(line_conts) = next_line else {
                break;
            };
            if line_conts == b"NAK\x0a" {
                continue;
            }
            let line_string = String::from_utf8_lossy(&line_conts);
            let Some(obj) = line_string.trim().strip_prefix("ACK ") else {
                return Err(anyhow!(
                    "unexpected line received in acknowledgements section"
                ));
            };
            objs.push(obj.to_string());
        }
        Ok(objs)
    }

    fn scrub_common_objects(common_objects: &mut IndexSet<String>, mut acked: Vec<String>) {
        common_objects.retain(|x| {
            acked.iter().position(|y| x == y).map_or(false, |pos| {
                acked.remove(pos);
                true
            })
        });
    }

    fn top_up_common_objects<'a>(
        common_objects: &mut IndexSet<String>,
        mut iter: CommitIterator<'a>,
        to_level: usize,
    ) -> Result<Option<CommitIterator<'a>>, anyhow::Error> {
        while common_objects.len() < to_level {
            match iter.next() {
                Some(res) => {
                    common_objects.insert(res?);
                }
                None => {
                    return Ok(None);
                }
            }
        }
        Ok(Some(iter))
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
        printer: &dyn OutputService,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        match self.protocol_version {
            Some(ProtocolVersion::V1) => self.fetch_refs_capabilities_v1(printer),
            Some(ProtocolVersion::V2) => self.fetch_refs_capabilities_v2(printer),
            None => self.fetch_refs_capabilities_sniff_protocol(printer),
        }
    }

    fn fetch_refs_capabilities_v1(
        &mut self,
        printer: &dyn OutputService,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        let (detected_version, first_line, lines) = self.make_initial_fetch_request(printer)?;
        if detected_version != ProtocolVersion::V1 {
            return Err(anyhow!("wrong protocol version detected"));
        }
        self.load_refs_capabilities_body_v1(first_line, lines, printer)
    }

    fn make_initial_fetch_request(
        &self,
        printer: &dyn OutputService,
    ) -> Result<(ProtocolVersion, PktLine, PktLineIterator<Response>), anyhow::Error> {
        let discovery_url = self.base_url.join("info/refs?service=git-upload-pack")?;
        printer.println_verbose(&OutputMessage::plain(&format!(
            "Discovery URL is {discovery_url}"
        )));
        let mut request = self.client.get(discovery_url);
        if self.protocol_version() == ProtocolVersion::V2 {
            request = add_git_protocol_header(request);
        }
        let response = response_check(request.send()?)?;
        let mut lines = PktLineIterator::from(response);
        if !Self::unwrap_and_test_line(
            lines.next(),
            &PktLine::Line(b"# service=git-upload-pack\x0a".to_vec()),
            printer,
        )? {
            return Err(anyhow!("response header not found"));
        }
        if !Self::unwrap_and_test_line(lines.next(), &PktLine::Flush, printer)? {
            return Err(anyhow!("end of header not found"));
        }
        let first_line = Self::unwrap_line(lines.next(), printer)?;
        let detected_version = if first_line == PktLine::Line(b"version 2\x0a".to_vec()) {
            ProtocolVersion::V2
        } else {
            ProtocolVersion::V1
        };
        Ok((detected_version, first_line, lines))
    }

    fn unwrap_line(
        line: Option<Result<PktLine, anyhow::Error>>,
        printer: &dyn OutputService,
    ) -> Result<PktLine, anyhow::Error> {
        let Some(line) = line else {
            return Err(anyhow!("unexpected end"));
        };
        let line = line?;
        printer.println_verbose(&OutputMessage::plain(&format!("R: {line}")));
        Ok(line)
    }

    fn unwrap_and_test_line(
        line: Option<Result<PktLine, anyhow::Error>>,
        test_line: &PktLine,
        printer: &dyn OutputService,
    ) -> Result<bool, anyhow::Error> {
        let line = Self::unwrap_line(line, printer)?;
        Ok(line == *test_line)
    }

    fn fetch_capabilities_v2(
        &mut self,
        printer: &dyn OutputService,
    ) -> Result<Vec<RemoteCapability>, anyhow::Error> {
        let (protocol_version, _, lines) = self.make_initial_fetch_request(printer)?;
        if protocol_version != ProtocolVersion::V2 {
            return Err(anyhow!("wrong protocol version"));
        }
        self.load_capabilities_body_v2(lines, printer)
    }

    fn fetch_refs_capabilities_v2(
        &mut self,
        printer: &dyn OutputService,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        self.capabilities = self.fetch_capabilities_v2(printer)?;
        self.fetch_refs_v2(printer)
    }

    fn fetch_refs_capabilities_sniff_protocol(
        &mut self,
        printer: &dyn OutputService,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        let (protocol_version, first_line, lines) = self.make_initial_fetch_request(printer)?;
        self.protocol_version = Some(protocol_version);
        match self.protocol_version {
            Some(ProtocolVersion::V1) => {
                self.load_refs_capabilities_body_v1(first_line, lines, printer)
            }
            Some(ProtocolVersion::V2) => {
                self.capabilities = self.load_capabilities_body_v2(lines, printer)?;
                self.fetch_refs_v2(printer)
            }
            _ => Err(anyhow!("impossible")),
        }
    }

    fn load_refs_capabilities_body_v1(
        &mut self,
        first_line: PktLine,
        lines: PktLineIterator<Response>,
        printer: &dyn OutputService,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        let mut refs = HashSet::<TargetedRef>::new();
        if let PktLine::Line(line_contents) = first_line {
            if let Some(parsed_first_line) =
                Self::load_single_v1_refs_capabilities_line(line_contents, &mut self.capabilities)?
            {
                refs.insert(parsed_first_line);
            }
        }
        for line in lines {
            let line = line.context("couldn't parse pkt-line")?;
            printer.println_verbose(&OutputMessage::plain(&format!("R:{line}")));
            if let PktLine::Line(line_contents) = line {
                if let Some(parsed_line) = Self::load_single_v1_refs_capabilities_line(
                    line_contents,
                    &mut self.capabilities,
                )? {
                    refs.insert(parsed_line);
                }
            }
        }
        Ok(RemoteServerInfo { refs })
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
        printer: &dyn OutputService,
    ) -> Result<Vec<RemoteCapability>, anyhow::Error> {
        let mut results = vec![];
        for line in lines {
            let line = line?;
            printer.println_verbose(&OutputMessage::plain(&format!("R: {line}")));
            if let PktLine::Line(content) = line {
                results.push(RemoteCapability::from_str(&String::from_utf8_lossy(
                    &content,
                ))?);
            }
        }
        Ok(results)
    }

    fn fetch_refs_v2(
        &self,
        printer: &dyn OutputService,
    ) -> Result<RemoteServerInfo, anyhow::Error> {
        let ref_url = self.base_url.join("git-upload-pack")?;
        let Some(ls_refs_args) = self.capability("ls-refs") else {
            return Err(anyhow!("server does not support ls-refs command"));
        };
        let mut body_lines = vec![PktLine::from("command=ls-refs\x0a")];
        if self.capability("agent").is_some() {
            body_lines.push(PktLine::from(&Self::agent_string()));
        }
        body_lines.push(PktLine::Delimiter);
        body_lines.push(PktLine::from("symrefs\x0a"));
        body_lines.push(PktLine::from("peel\x0a"));
        if ls_refs_args.contains(&"unborn") {
            body_lines.push(PktLine::from("unborn\x0a"));
        }
        body_lines.push(PktLine::Flush);
        for line in &body_lines {
            printer.println_verbose(&OutputMessage::plain(&format!("S: {line}")));
        }
        let request = add_git_protocol_header(self.client.post(ref_url)).body(
            body_lines
                .iter()
                .flat_map(|n| n.bytes())
                .collect::<Vec<u8>>(),
        );
        let lines = PktLineIterator::from(response_check(request.send()?)?);
        let mut refs = HashSet::<TargetedRef>::new();
        for line in lines {
            let line = Self::unwrap_line(Some(line), printer)?;
            let PktLine::Line(line_contents) = line else {
                continue;
            };
            let Some(id_len) = line_contents.iter().position(|x| *x == 32) else {
                return Err(anyhow!("line format: could not find space"));
            };
            let target_id =
                String::from_utf8(line_contents[..id_len].to_vec()).context("invalid target ID")?;
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
                        refs.insert(TargetedRef {
                            target: RefTarget::Object(peeled_ref_target.to_string()),
                            spec: peeled_ref_spec,
                        });
                    }
                    refs.insert(TargetedRef {
                        target: RefTarget::Object(target_id),
                        spec: primary_ref_spec,
                    });
                } else if let Some(symref_ref_target) =
                    secondary_ref_string.strip_prefix("symref-target:")
                {
                    refs.insert(TargetedRef {
                        target: RefTarget::SymbolicRef(RefSpec::from_str(symref_ref_target)?),
                        spec: primary_ref_spec,
                    });
                    refs.insert(TargetedRef {
                        target: RefTarget::Object(target_id),
                        spec: RefSpec::from_str(symref_ref_target)?,
                    });
                } else {
                    refs.insert(TargetedRef {
                        target: RefTarget::Object(target_id),
                        spec: primary_ref_spec,
                    });
                }
            } else {
                refs.insert(TargetedRef {
                    target: RefTarget::Object(target_id),
                    spec: primary_ref_spec,
                });
            }
        }
        Ok(RemoteServerInfo { refs })
    }

    fn agent_string() -> String {
        format!("agent=cvvc/{}\x0a", env!("CARGO_PKG_VERSION"))
    }
}

fn response_check(response: Response) -> Result<Response, anyhow::Error> {
    if !response.status().is_success() {
        Err(anyhow!(
            "Request failed: {} {}",
            &response.status(),
            response.text()?
        ))
    } else {
        Ok(response)
    }
}

fn add_git_protocol_header(builder: RequestBuilder) -> RequestBuilder {
    builder.header("Git-Protocol", "version=2")
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

impl From<PackFetchCommand> for PktLine {
    fn from(value: PackFetchCommand) -> Self {
        Self::from(&value)
    }
}

enum PackFetchResponse {
    InfoNeeded(Vec<String>),
    Ready(Vec<String>),
    Pack(PktLineSidebandReader),
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
