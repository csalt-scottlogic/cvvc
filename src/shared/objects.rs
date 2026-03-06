use anyhow::{anyhow, Context};
use chrono::{DateTime, TimeZone};
use indexmap::IndexMap;
use sha1::{Digest, Sha1};
use std::{
    cmp::Ordering,
    fmt::Display,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::shared::{
    errors::InvalidObjectError,
    helpers::timestamped_name,
    index::IndexEntry,
    repo::Repository,
    stores::pack_store::{PackStore, PackedObjectMetadata},
};

pub struct RawObject {
    data: Vec<u8>,
    hash: String,
    pub pack_metadata: Option<PackedObjectMetadata>,
}

impl RawObject {
    pub fn new(data: Vec<u8>, hash: &str, metadata: Option<PackedObjectMetadata>) -> Self {
        RawObject {
            data,
            hash: hash.to_string(),
            pack_metadata: metadata,
        }
    }

    pub fn from_git_object(obj: &impl GitObject) -> Self {
        let mut data = Vec::<u8>::new();
        obj.serialise(&mut data);
        let mut content = obj.object_type_code().to_vec();
        content.extend(b" ");
        content.extend(data.len().to_string().into_bytes());
        content.extend(b"\x00");
        content.extend(data);

        let mut hasher = Sha1::new();
        hasher.update(&content);
        let hash = hex::encode(hasher.finalize());
        Self {
            data: content,
            hash,
            pack_metadata: None,
        }
    }

    pub fn content(&self) -> &[u8] {
        &self.data
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }
}

pub enum ObjectKind {
    Blob,
    Commit,
    Tree,
    Tag,
}

pub enum StoredObject {
    Blob(Blob),
    Commit(Commit),
    Tree(Tree),
    Tag(Tag),
}

impl StoredObject {
    pub fn serialise(&self, buf: &mut Vec<u8>) {
        match self {
            StoredObject::Blob(x) => x.serialise(buf),
            StoredObject::Commit(x) => x.serialise(buf),
            StoredObject::Tree(x) => x.serialise(buf),
            StoredObject::Tag(x) => x.serialise(buf),
        }
    }
}

pub trait GitObject {
    type Implementation;
    fn _kind(&self) -> ObjectKind;
    fn object_type_code(&self) -> &'static [u8];
    fn serialise(&self, buf: &mut Vec<u8>);
    fn deserialise(data: &[u8]) -> Self::Implementation
    where
        Self: Sized;
}

pub struct Blob {
    data: Vec<u8>,
}

impl Blob {
    pub fn new_from_read(source: &mut impl Read) -> Result<Self, anyhow::Error> {
        let mut buf: Vec<u8> = Vec::new();
        source
            .read_to_end(&mut buf)
            .context("Failed to read blob from source")?;
        Ok(Blob { data: buf })
    }

    pub fn new_from_path<P: AsRef<Path>>(source_path: P) -> Result<Self, anyhow::Error> {
        let mut file = std::fs::File::open(source_path).context("could not read file")?;
        Self::new_from_read(&mut file)
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl GitObject for Blob {
    type Implementation = Blob;

    fn _kind(&self) -> ObjectKind {
        ObjectKind::Blob
    }

    fn object_type_code(&self) -> &'static [u8] {
        b"blob"
    }

    fn serialise(&self, buf: &mut Vec<u8>) {
        buf.clear();
        buf.extend_from_slice(&self.data);
    }

    fn deserialise(data: &[u8]) -> Self::Implementation
    where
        Self: Sized,
    {
        Blob {
            data: data.to_vec(),
        }
    }
}

pub struct Commit {
    map: IndexMap<String, Vec<String>>,
    pub message: String,
}

impl Commit {
    pub fn map(&self) -> &IndexMap<String, Vec<String>> {
        &self.map
    }

    pub fn tree(&self) -> Result<String, InvalidObjectError> {
        let target = self.map.get("tree");
        let Some(target) = target else {
            return Err(InvalidObjectError {});
        };
        let target = target.first();
        let Some(target) = target else {
            return Err(InvalidObjectError {});
        };
        Ok(target.to_string())
    }

    pub fn new<Tz>(
        tree_id: &str,
        parent_id: Option<&str>,
        author: &str,
        committer: &str,
        timestamp: &DateTime<Tz>,
        message: &str,
    ) -> Self
    where
        Tz: TimeZone,
        Tz::Offset: Display,
    {
        let mut map = IndexMap::<String, Vec<String>>::new();
        map.insert(String::from("tree"), vec![String::from(tree_id)]);
        if let Some(parent_id) = parent_id {
            map.insert(String::from("parent"), vec![String::from(parent_id)]);
        }
        map.insert(
            String::from("author"),
            vec![timestamped_name(author, timestamp)],
        );
        map.insert(
            String::from("committer"),
            vec![timestamped_name(committer, timestamp)],
        );
        let message = message.trim().to_owned() + "\n";
        Commit { map, message }
    }
}

impl GitObject for Commit {
    type Implementation = Commit;

    fn _kind(&self) -> ObjectKind {
        ObjectKind::Commit
    }

    fn object_type_code(&self) -> &'static [u8] {
        b"commit"
    }

    fn serialise(&self, buf: &mut Vec<u8>) {
        kvlm_serialise(&self.map, &self.message, buf)
    }

    fn deserialise(data: &[u8]) -> Self::Implementation
    where
        Self: Sized,
    {
        let mut map = IndexMap::<String, Vec<String>>::new();
        let message = kvlm_parse(data, &mut map).expect("Failed to parse commit");
        Commit { map, message }
    }
}

pub struct Tag {
    map: IndexMap<String, Vec<String>>,
    pub message: String,
}

impl Tag {
    pub fn _map(&self) -> &IndexMap<String, Vec<String>> {
        &self.map
    }

    pub fn create(target: &str, name: &str) -> Self {
        let message = String::from("A tag created by Cait's RYAG");
        let mut map = IndexMap::<String, Vec<String>>::new();
        map.insert(String::from("object"), vec![target.to_string()]);
        map.insert(String::from("type"), vec![String::from("commit")]);
        map.insert(String::from("name"), vec![String::from(name)]);
        map.insert(
            String::from("tagger"),
            vec![String::from("Cait <cait@symbolicforest.com>")],
        );
        Tag { map, message }
    }

    pub fn target(&self) -> Result<String, InvalidObjectError> {
        let target = self.map.get("object");
        let Some(target) = target else {
            return Err(InvalidObjectError {});
        };
        let target = target.first();
        let Some(target) = target else {
            return Err(InvalidObjectError {});
        };
        Ok(target.to_string())
    }
}

impl GitObject for Tag {
    type Implementation = Tag;

    fn _kind(&self) -> ObjectKind {
        ObjectKind::Tag
    }

    fn object_type_code(&self) -> &'static [u8] {
        b"tag"
    }

    fn serialise(&self, buf: &mut Vec<u8>) {
        kvlm_serialise(&self.map, &self.message, buf)
    }

    fn deserialise(data: &[u8]) -> Self::Implementation
    where
        Self: Sized,
    {
        let mut map = IndexMap::<String, Vec<String>>::new();
        let message = kvlm_parse(data, &mut map).expect("Failed to parse tag");
        Tag { map, message }
    }
}

#[derive(Clone)]
pub struct TreeNode {
    pub mode: u32,
    pub path: PathBuf,
    pub object_id: String,
}

pub struct TreeNodeParsingResult {
    consumed: usize,
    node: TreeNode,
}

impl TreeNode {
    pub fn from_bytes(data: &[u8]) -> Result<TreeNodeParsingResult, anyhow::Error> {
        let space_pos = data.iter().position(|x| *x == 0x20);
        let Some(space_pos) = space_pos else {
            return Err(anyhow!("Mode terminator character not found in tree entry"));
        };
        if space_pos != 5 && space_pos != 6 {
            return Err(anyhow!("Mode field of tree entry is incorrect length"));
        }
        let mode_str = str::from_utf8(&data[..space_pos])
            .context("Could not parse mode field of tree entry as valid UTF8")?;
        let mode = u32::from_str_radix(mode_str, 8)
            .context("Could not parse mode field of tree entry as valid octal integer")?;
        let null_pos = &data[(space_pos + 1)..].iter().position(|x| *x == 0);
        let Some(null_pos) = null_pos else {
            return Err(anyhow!("Path terminator character not found in tree entry"));
        };
        if space_pos + null_pos + 21 >= data.len() {
            return Err(anyhow!(
                "Tree entry is too short to contain valid object name"
            ));
        }
        let path = str::from_utf8(&data[(space_pos + 1)..(space_pos + null_pos + 1)])
            .context("Could not parse path field of tree entry as valid UTF8")?;
        let path_buf = PathBuf::from(path);
        let object_id = hex::encode(&data[(space_pos + null_pos + 2)..(space_pos + null_pos + 22)]);
        Ok(TreeNodeParsingResult {
            consumed: space_pos + null_pos + 22,
            node: TreeNode {
                mode,
                path: path_buf,
                object_id,
            },
        })
    }

    pub fn from_index_entry(ixe: &IndexEntry) -> Self {
        Self {
            mode: ixe.mode(),
            path: Path::new(&ixe.object_file_name()).to_path_buf(),
            object_id: ixe.object_id.to_string(),
        }
    }

    pub fn from_subtree(dir_name: &str, object_id: &str) -> Self {
        Self {
            mode: 0o40000,
            path: Path::new(dir_name).to_path_buf(),
            object_id: object_id.to_string(),
        }
    }

    fn ordering_path(&self) -> String {
        if self.mode >= 0o100000 {
            self.path.to_string_lossy().to_string()
        } else {
            self.path.to_string_lossy().to_string() + "/"
        }
    }
}

impl Ord for TreeNode {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ordering_path().cmp(&other.ordering_path())
    }
}

impl PartialOrd for TreeNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TreeNode {
    fn eq(&self, rhs: &Self) -> bool {
        matches!(self.cmp(rhs), Ordering::Equal)
    }
}

impl Eq for TreeNode {}

pub struct Tree {
    entries: Vec<TreeNode>,
}

impl Tree {
    pub fn new() -> Tree {
        Tree {
            entries: Vec::<TreeNode>::new(),
        }
    }

    pub fn entries(&self) -> &[TreeNode] {
        &self.entries
    }

    pub fn _add_entry(&mut self, entry: TreeNode) {
        self.entries.push(entry);
        self.sort();
    }

    pub fn add_entries(&mut self, entries: &mut Vec<TreeNode>) {
        self.entries.append(entries);
        self.sort();
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, anyhow::Error> {
        let mut entries = Vec::<TreeNode>::new();
        let mut pos: usize = 0;
        let data_len = data.len();
        while pos < data_len {
            let node = TreeNode::from_bytes(&data[pos..])?;
            entries.push(node.node);
            pos += node.consumed;
        }

        let mut tree = Self::new();
        tree.add_entries(&mut entries);
        Ok(tree)
    }

    fn sort(&mut self) {
        self.entries.sort();
    }

    pub fn checkout(&self, repo: &Repository, path: &Path) -> Result<Vec<String>, anyhow::Error> {
        let mut objects_checked_out = Vec::<String>::new();
        for entry in &self.entries {
            let obj = repo.read_object(&entry.object_id)?;
            let Some(obj) = obj else {
                return Err(anyhow!("Object {} not found", entry.object_id));
            };
            let path = path.join(&entry.path);
            match obj {
                StoredObject::Tree(tree) => {
                    fs::create_dir(&path)?;
                    let mut subdir_checked_out = tree.checkout(repo, &path)?;
                    objects_checked_out.append(&mut subdir_checked_out);
                }
                StoredObject::Blob(blob) => {
                    fs::write(path, blob.data)?;
                    objects_checked_out.push(entry.object_id.to_string());
                }
                StoredObject::Tag(_) => (),
                StoredObject::Commit(_) => {
                    return Err(anyhow!(
                        "Submodules, like object {}, are not currently supported.",
                        entry.object_id
                    ));
                }
            }
        }
        Ok(objects_checked_out)
    }
}

impl GitObject for Tree {
    type Implementation = Tree;

    fn _kind(&self) -> ObjectKind {
        ObjectKind::Tree
    }

    fn object_type_code(&self) -> &'static [u8] {
        b"tree"
    }

    fn serialise(&self, buf: &mut Vec<u8>) {
        for entry in self.entries() {
            let mode_str = format!("{:05o}", entry.mode);
            buf.append(Vec::from_iter(mode_str.bytes()).as_mut());
            buf.push(0x20);
            buf.append(entry.path.to_string_lossy().as_bytes().to_vec().as_mut());
            buf.push(0);
            buf.append(hex::decode(&entry.object_id).unwrap().as_mut());
        }
    }

    fn deserialise(data: &[u8]) -> Self::Implementation
    where
        Self: Sized,
    {
        Tree::from_bytes(data).unwrap()
    }
}

pub fn stored_object_matches_kind(kind: &ObjectKind, obj: &StoredObject) -> bool {
    match kind {
        ObjectKind::Blob => {
            matches!(obj, StoredObject::Blob(_))
        }
        ObjectKind::Tree => {
            matches!(obj, StoredObject::Tree(_))
        }
        ObjectKind::Commit => {
            matches!(obj, StoredObject::Commit(_))
        }
        ObjectKind::Tag => {
            matches!(obj, StoredObject::Tag(_))
        }
    }
}

fn kvlm_parse(
    raw_data: &[u8],
    map: &mut IndexMap<String, Vec<String>>,
) -> Result<String, anyhow::Error> {
    let space_index = raw_data.iter().position(|x| *x == 0x20);
    let nl_index = raw_data.iter().position(|x| *x == 0x0a);

    if space_index.is_none() || nl_index.unwrap_or(usize::MAX) < space_index.unwrap() {
        let message = String::from_utf8(raw_data[1..].to_vec())?;
        return Ok(message);
    }
    let space_index = space_index.unwrap();

    let key = str::from_utf8(&raw_data[0..space_index])?;
    let end = find_without(&raw_data[(space_index + 1)..], 0x0a, 0x20);
    let data_slice = str::from_utf8(match end {
        Some(x) => &raw_data[(space_index + 1)..(space_index + 1 + x)],
        None => &raw_data[(space_index + 1)..],
    })?
    .replace("\n ", "\n");

    if map.contains_key(key) {
        map[key].push(data_slice);
    } else {
        map.insert(key.to_string(), vec![data_slice]);
    }

    if let Some(end) = end {
        return kvlm_parse(&raw_data[(end + space_index + 2)..], map);
    }
    Ok(String::new())
}

fn kvlm_serialise(map: &IndexMap<String, Vec<String>>, message: &str, buf: &mut Vec<u8>) {
    buf.clear();
    for k in map.keys() {
        if k.is_empty() {
            continue;
        }
        let val = &map[k];
        for v in val.iter() {
            buf.append(k.as_bytes().to_vec().as_mut());
            buf.push(0x20);
            buf.append(
                v.replace("\n", "\n ")
                    .trim_end()
                    .as_bytes()
                    .to_vec()
                    .as_mut(),
            );
            buf.push(0x0a);
        }
    }
    buf.push(0x0a);
    buf.append(message.as_bytes().to_vec().as_mut());
}

/// Find the first index in a slice of a particular value, where it's not followed immediately by another specific value.
fn find_without(data: &[u8], with: u8, without: u8) -> Option<usize> {
    let mut next_with = 0;
    loop {
        next_with = data[next_with..].iter().position(|x| *x == with)?;
        if data[next_with + 1] != without {
            break;
        }
    }
    Some(next_with)
}
