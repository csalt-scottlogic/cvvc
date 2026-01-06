use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use flate2::{bufread::ZlibEncoder, read::ZlibDecoder, Compression};
use indexmap::IndexMap;
use ini::Ini;
use sha1::{Digest, Sha1};
use std::{
    cmp::Ordering,
    collections::HashMap,
    env,
    fs::{self, File},
    io::{BufReader, Cursor, Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::shared::{
    errors::{FindObjectError, InvalidIndexEntryError, InvalidIndexError, InvalidObjectError},
    ignore::{IgnoreInfo, IgnorePattern},
};

mod errors;
mod helpers;
mod ignore;

pub struct Repository {
    worktree: PathBuf,
    git_dir: PathBuf,
    conf: Ini,
}

impl Repository {
    pub fn new(worktree: &PathBuf, allow_invalid: bool) -> Result<Self, anyhow::Error> {
        let my_worktree = worktree.clone();
        let my_git_dir = worktree.join(Path::new(".git"));
        if !(allow_invalid || my_git_dir.is_dir()) {
            return Err(anyhow!("Not a git directory"));
        }
        let config_path = repo_path(&my_git_dir, Path::new("config"));
        let mut wrapped_config: Option<Ini> = None;
        if config_path.is_file() {
            let loaded_config = Ini::load_from_file(config_path);
            if loaded_config.is_err() {
                if !allow_invalid {
                    return Err(anyhow::Error::from(loaded_config.unwrap_err())
                        .context("Could not open configuration file"));
                }
            } else {
                wrapped_config = Some(loaded_config.unwrap());
            }
        } else if !allow_invalid {
            return Err(anyhow!("Configuration file missing"));
        }

        let config = wrapped_config.unwrap_or_else(|| default_config());

        if !allow_invalid {
            let core_section = match config.section(Some("core")) {
                Some(s) => s,
                None => {
                    return Err(anyhow!(
                        "Configuration file does not contain a [core] section"
                    ))
                }
            };
            let format_version_property = match core_section.get("repositoryformatversion") {
                Some(s) => s,
                None => {
                    return Err(anyhow!(
                        "Configuration file does not have the repository format version set"
                    ))
                }
            };
            let format_version = format_version_property
                .parse::<i32>()
                .context("repositoryformatversion is not an integer")?;
            if format_version != 0 {
                return Err(anyhow!("Unsupported repository version {format_version}"));
            }
        }

        Ok(Repository {
            worktree: my_worktree,
            git_dir: my_git_dir,
            conf: config,
        })
    }

    pub fn create(path: &PathBuf) -> Result<Self, anyhow::Error> {
        let repo = Repository::new(path, true)?;

        if repo.worktree.exists() {
            if !repo.worktree.is_dir() {
                return Err(anyhow!(format!(
                    "Path {} is not a directory",
                    repo.worktree.display()
                )));
            }
            if repo.git_dir.exists() {
                if !repo.git_dir.is_dir() {
                    return Err(anyhow!(format!(
                        "Path {} is not a directory",
                        repo.git_dir.display()
                    )));
                }
                let mut dir_contents = repo
                    .git_dir
                    .read_dir()
                    .context("Could not attempt to read contents of repository")?;
                if dir_contents.next().is_some() {
                    return Err(anyhow!("Repository directory is not empty"));
                }
            }
        } else {
            fs::create_dir_all(&repo.worktree)
                .context("Could not create all components of directory path")?;
        }

        repo.dir(Path::new("branches"), true)?;
        repo.dir(Path::new("objects"), true)?;
        repo.dir(&["refs", "tags"].iter().collect::<PathBuf>(), true)?;
        repo.dir(&["refs", "heads"].iter().collect::<PathBuf>(), true)?;

        fs::write(
            repo.file_unchecked(Path::new("description")),
            "Unnamed repository\n",
        )?;

        fs::write(
            repo.file_unchecked(Path::new("HEAD")),
            "ref: refs/heads/main\n",
        )?;

        repo.conf
            .write_to_file(repo.file_unchecked(Path::new("config")))?;

        Ok(repo)
    }

    pub fn _path(&self, path: &Path) -> PathBuf {
        repo_path(&self.git_dir, path)
    }

    pub fn file(&self, path: &Path, mkdir: bool) -> Result<Option<PathBuf>, anyhow::Error> {
        repo_file(&self.git_dir, path, mkdir)
    }

    pub fn file_unchecked(&self, path: &Path) -> PathBuf {
        self.file(path, false).unwrap().unwrap()
    }

    pub fn dir(&self, path: &Path, mkdir: bool) -> Result<Option<PathBuf>, anyhow::Error> {
        repo_dir(&self.git_dir, path, mkdir)
    }

    pub fn _dir_unchecked(&self, path: &Path) -> PathBuf {
        self.dir(path, false).unwrap().unwrap()
    }

    fn strip_git_dir(&self, path: &Path) -> PathBuf {
        if path.starts_with(&self.git_dir) {
            path.strip_prefix(&self.git_dir).unwrap().to_path_buf()
        } else {
            path.to_path_buf()
        }
    }

    pub fn find_object(
        &self,
        name: &str,
        kind: Option<ObjectKind>,
        follow_tags: bool,
    ) -> Result<String, anyhow::Error> {
        let resolve_result = self.resolve_object(name)?;
        if resolve_result.len() == 0 {
            return Err(anyhow::Error::from(FindObjectError::none()));
        }
        if resolve_result.len() > 1 {
            return Err(anyhow::Error::from(FindObjectError::some(&resolve_result)));
        }
        let Some(kind) = kind else {
            return Ok(resolve_result[0].to_string());
        };
        let mut current_target = resolve_result[0].to_string();
        loop {
            let obj = self.object_read(&current_target)?;
            let Some(obj) = obj else {
                return Err(anyhow::Error::from(FindObjectError::none()));
            };
            if stored_object_matches_kind(&kind, &obj) {
                return Ok(current_target);
            }
            if !follow_tags {
                return Err(anyhow::Error::from(FindObjectError::none()));
            }
            match obj {
                StoredObject::Tag(tag) => {
                    current_target = tag.target().context("chunky tag has invalid target")?;
                }
                StoredObject::Commit(commit) => {
                    if let ObjectKind::Tree = kind {
                        current_target = commit.tree().context("commit has no tree")?;
                    }
                }
                _ => {
                    return Err(anyhow::Error::from(FindObjectError::none()));
                }
            }
        }
    }

    fn resolve_object(&self, name: &str) -> Result<Vec<String>, anyhow::Error> {
        let name = name.trim();
        if name == "" {
            return Ok(Vec::<String>::new());
        }

        if name == "HEAD" {
            let head_ref = self.ref_resolve(name)?;
            return match head_ref {
                Some(hr) => Ok(vec![hr]),
                None => Err(anyhow!("Error: missing HEAD")),
            };
        }

        let mut collected = Vec::<String>::new();
        if is_partial_object_name(name) {
            let path = self.dir(&["objects", &name[..2]].iter().collect::<PathBuf>(), false)?;
            if let Some(path) = path {
                let dir_entries = fs::read_dir(&path)
                    .context(format!("Trying to read path {}", &path.to_string_lossy()))?
                    .collect::<Result<Vec<_>, std::io::Error>>()?;
                for mut f in dir_entries
                    .iter()
                    .map(|e| e.file_name().into_string().unwrap_or("".to_owned()))
                    .filter(|f| f.starts_with(&name[2..]) && is_object_file_name(f))
                {
                    f.insert_str(0, &name[..2]);
                    collected.push(f);
                }
            }
        }

        let potential_tag = self.ref_resolve(&("refs/tags/".to_string() + name))?;
        if let Some(potential_tag) = potential_tag {
            collected.push(potential_tag);
        }

        let potential_branch = self.ref_resolve(&("refs/heads/".to_string() + name))?;
        if let Some(potential_branch) = potential_branch {
            collected.push(potential_branch);
        }

        let potential_remote_branch = self.ref_resolve(&("refs/remotes/".to_string() + name))?;
        if let Some(potential_remote_branch) = potential_remote_branch {
            collected.push(potential_remote_branch);
        }

        Ok(collected)
    }

    pub fn object_read(&self, sha: &str) -> Result<Option<StoredObject>, anyhow::Error> {
        let path = self.file(
            &["objects", &sha[..2], &sha[2..]]
                .iter()
                .collect::<PathBuf>(),
            false,
        )?;
        let Some(path) = path else {
            return Ok(None);
        };
        if !path.is_file() {
            return Ok(None);
        }
        let file = fs::File::open(path)?;
        let mut decompressor = ZlibDecoder::new(file);
        let mut data: Vec<u8> = vec![];
        decompressor.read_to_end(&mut data)?;
        let type_end_index = data.iter().position(|&x| x == 0x20).ok_or(anyhow!(
            "Malformed object {sha}: end of object type code not found"
        ))?;
        let len_start_index = type_end_index + 1;
        let len_end_index = data
            .iter()
            .skip(len_start_index)
            .position(|&x| x == 0)
            .ok_or(anyhow!(
                "Malformed object {sha}: end of object length not found"
            ))?
            + len_start_index;
        let data_start_index = len_end_index + 1;
        let object_type = &data[..type_end_index];
        let object_len = std::str::from_utf8(&data[len_start_index..len_end_index])?
            .parse::<usize>()
            .context(format!(
                "Could not parse object length!  Object length string was {}",
                std::str::from_utf8(&data[len_start_index..len_end_index])?
            ))?;
        let actual_len = data.len() - data_start_index;
        if object_len != actual_len {
            return Err(anyhow!(
                "Malformed object {sha}: expected length {object_len}, actual length {actual_len}"
            ));
        }

        match object_type {
            b"blob" => Ok(Some(StoredObject::Blob(Blob::deserialise(
                &data[data_start_index..],
            )))),
            b"commit" => Ok(Some(StoredObject::Commit(Commit::deserialise(
                &data[data_start_index..],
            )))),
            b"tree" => Ok(Some(StoredObject::Tree(Tree::deserialise(
                &data[data_start_index..],
            )))),
            b"tag" => Ok(Some(StoredObject::Tag(Tag::deserialise(
                &data[data_start_index..],
            )))),
            _ => Err(anyhow!(format!(
                "Unrecognised object type {}",
                std::str::from_utf8(object_type).unwrap_or("[mangled]")
            ))),
        }
    }

    pub fn ref_resolve(&self, git_ref: &str) -> Result<Option<String>, anyhow::Error> {
        println!("Resolving {git_ref}");
        let path = self.file(&PathBuf::from_iter(git_ref.split("/")), false)?;
        let Some(path) = path else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let ref_conts = fs::read_to_string(path)?;
        if ref_conts.starts_with("ref: ") {
            return self.ref_resolve(&ref_conts[5..].trim());
        }
        Ok(Some(ref_conts.trim().to_string()))
    }

    pub fn ref_list_dir(
        &self,
        path: Option<&Path>,
    ) -> Result<IndexMap<String, String>, anyhow::Error> {
        let (path, root_path) = match path {
            Some(p) => (p, Some(&p.to_path_buf())),
            None => (Path::new("refs"), None),
        };
        self.ref_list_dir_internal(&path, root_path)
    }

    fn ref_list_dir_internal(
        &self,
        path: &Path,
        root_path: Option<&PathBuf>,
    ) -> Result<IndexMap<String, String>, anyhow::Error> {
        let path = self.dir(path, true);
        let Ok(path) = path else {
            return Err(path.err().unwrap());
        };
        let Some(path) = path else {
            return Err(anyhow!("Ref path has disappeared"));
        };
        let dir_entries = fs::read_dir(&path)
            .context(format!("Trying to read path {}", &path.to_string_lossy()))?
            .collect::<Result<Vec<_>, std::io::Error>>()?;
        let mut files = dir_entries
            .iter()
            .filter(|e| e.metadata().is_ok_and(|f| f.is_file()))
            .map(|e| e.path())
            .collect::<Vec<PathBuf>>();
        files.sort();
        let mut dirs = dir_entries
            .iter()
            .filter(|e| e.metadata().is_ok_and(|f| f.is_dir()))
            .map(|e| e.path())
            .collect::<Vec<PathBuf>>();
        dirs.sort();
        let mut output = IndexMap::<String, String>::new();
        for f in files {
            let mut stripped_path = self.strip_git_dir(&f);
            let ref_target = self.ref_resolve(&stripped_path.to_string_lossy())?;
            if let Some(rp) = root_path {
                stripped_path = stripped_path.strip_prefix(rp)?.to_path_buf();
            }
            if let Some(ref_target) = ref_target {
                output.insert(stripped_path.to_string_lossy().to_string(), ref_target);
            }
        }
        for d in dirs {
            let mut rec_result = self.ref_list_dir_internal(&self.strip_git_dir(&d), root_path)?;
            output.append(&mut rec_result);
        }
        Ok(output)
    }

    pub fn ref_create(&self, name: &str, target_name: &str) -> Result<(), anyhow::Error> {
        println!("Creating {name} pointing to {target_name}");
        let ref_file_path = self.file(&PathBuf::from_iter(["refs", name]), true)?;
        let Some(ref_file_path) = ref_file_path else {
            return Err(anyhow!("Failure to create ref path"));
        };
        println!("{}", ref_file_path.display());
        let mut ref_file = File::create(&ref_file_path)?;
        ref_file.write(target_name.as_bytes())?;
        ref_file.write("\n".as_bytes())?;
        Ok(())
    }

    pub fn index_read(&self) -> Result<Index, anyhow::Error> {
        let file = self.file(&Path::new("index"), false)?;
        let file = match file {
            Some(f) => f,
            None => {
                return Ok(Index::new());
            }
        };
        let data = std::fs::read(file).context("error loading index file")?;
        let index = Index::from_bytes(&data).context("malformed index file")?;
        Ok(index)
    }

    pub fn ignore_info_read(&self) -> Result<IgnoreInfo, anyhow::Error> {
        let mut absolute_ignores = Vec::<IgnorePattern>::new();
        let mut repo_wide_file: PathBuf = self.git_dir.join("info");
        repo_wide_file.push("exclude");
        if repo_wide_file.exists() {
            absolute_ignores.append(&mut ignore_file_read(&repo_wide_file)?);
        }

        let config_dir_var = env::var("XDG_CONFIG_HOME");
        let config_dir = match config_dir_var {
            Ok(var) => Some(PathBuf::from_str(&var).unwrap().join("git")),
            Err(_) => env::home_dir().and_then(|hd| Some(hd.join(".config").join("git"))),
        };
        if let Some(config_dir) = config_dir {
            let global_exclude_file = config_dir.join("ignore");
            if global_exclude_file.exists() {
                absolute_ignores.append(&mut ignore_file_read(&global_exclude_file)?);
            }
        }

        let mut dir_ignores = HashMap::<String, Vec<IgnorePattern>>::new();
        let index = self.index_read()?;
        for entry in index
            .entries()
            .iter()
            .filter(|e| e.object_name == ".gitignore" || e.object_name.ends_with("/.gitignore"))
        {
            let slash_idx = entry.object_name.rfind("/");
            let entry_dir = match slash_idx {
                Some(idx) => entry.object_name[..idx].to_string(),
                None => String::new(),
            };
            let contents = self.object_read(&entry.object_id)?;
            let Some(contents) = contents else {
                return Err(anyhow!(
                    "ignore file {} ({}) listed in index is not present in object store",
                    entry.object_name,
                    entry.object_id
                ));
            };
            let StoredObject::Blob(blob) = contents else {
                return Err(anyhow!(
                    "ignore file {} ({}) listed in index is not a blob",
                    entry.object_name,
                    entry.object_id
                ));
            };
            dir_ignores.insert(
                entry_dir,
                String::from_utf8_lossy(blob.data())
                    .lines()
                    .filter_map(|line| IgnorePattern::from_str(line))
                    .collect(),
            );
        }

        Ok(IgnoreInfo::new(absolute_ignores, dir_ignores))
    }
}

fn ignore_file_read(path: &PathBuf) -> Result<Vec<IgnorePattern>, anyhow::Error> {
    let file_contents = std::fs::read_to_string(path).context("error reading ignore file")?;
    Ok(file_contents
        .lines()
        .filter_map(|line| IgnorePattern::from_str(line))
        .collect())
}

fn is_partial_object_name(name: &str) -> bool {
    name.len() >= 4
        && name.len() <= 20
        && name
            .chars()
            .all(|c| c.is_ascii_hexdigit() && c.is_lowercase())
}

fn is_object_file_name(name: &str) -> bool {
    is_partial_object_name(name) && name.len() == 18
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

pub fn object_write<'a>(
    obj: &impl GitObject,
    repo: Option<&Repository>,
) -> Result<String, anyhow::Error> {
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

    if repo.is_some() {
        let the_repo = repo.unwrap();
        let path = the_repo.file(
            &["objects", &hash[0..2], &hash[2..]]
                .iter()
                .collect::<PathBuf>(),
            true,
        )?;
        if path.is_some() {
            let the_path = path.unwrap();
            if !the_path.exists() {
                let mut file = fs::File::create(the_path)?;
                let mut compressor =
                    ZlibEncoder::new(BufReader::new(Cursor::new(content)), Compression::best());
                std::io::copy(&mut compressor, &mut file)?;
            }
        }
    }

    Ok(hash)
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
        Commit {
            map: map,
            message: message,
        }
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
        Tag {
            map: map,
            message: message,
        }
    }
}

pub fn kvlm_parse<'a>(
    raw_data: &'a [u8],
    map: &mut IndexMap<String, Vec<String>>,
) -> Result<String, anyhow::Error> {
    let space_index = raw_data.iter().position(|x| *x == 0x20);
    let nl_index = raw_data.iter().position(|x| *x == 0x0a);

    if space_index.is_none()
        || nl_index.unwrap_or_else(|| usize::max_value()) < space_index.unwrap()
    {
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

pub fn kvlm_serialise(map: &IndexMap<String, Vec<String>>, message: &str, buf: &mut Vec<u8>) {
    buf.clear();
    for k in map.keys() {
        if *k == "" {
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

fn repo_path(git_dir: &PathBuf, path: &Path) -> PathBuf {
    git_dir.join(path)
}

fn repo_file(
    git_dir: &PathBuf,
    path: &Path,
    mkdir: bool,
) -> Result<Option<PathBuf>, anyhow::Error> {
    let file_name = path.file_name();
    if file_name.is_none() {
        return Err(anyhow!("Path must not be a directory"));
    }
    let base_path = path.parent().unwrap_or(Path::new(""));
    let dir_path = repo_dir(git_dir, base_path, mkdir)?;
    Ok(match dir_path {
        Some(the_path) => Some(the_path.join(file_name.unwrap())),
        None => None,
    })
}

fn repo_dir(git_dir: &PathBuf, path: &Path, mkdir: bool) -> Result<Option<PathBuf>, anyhow::Error> {
    let path = repo_path(git_dir, path);
    check_and_create_dir(path, mkdir)
}

fn check_and_create_dir(path: PathBuf, mkdir: bool) -> Result<Option<PathBuf>, anyhow::Error> {
    if path.exists() {
        if path.is_dir() {
            return Ok(Some(path));
        }
        return Err(anyhow!("Path exists but is not a directory"));
    }
    if mkdir {
        fs::create_dir_all(&path).context("Could not create all components of directory path")?;
        return Ok(Some(path));
    }
    Ok(None)
}

fn default_config() -> Ini {
    let mut conf = Ini::new();
    conf.with_section(Some("core"))
        .set("repositoryformatversion", "0")
        .set("filemode", "false")
        .set("bare", "false");
    conf
}

pub fn repo_find(path: &Path) -> Result<Option<Repository>, anyhow::Error> {
    let path_buf = path.to_path_buf().canonicalize()?;
    if path_buf.join(Path::new(".git")).is_dir() {
        return Ok(Some(Repository::new(&path_buf, false)?));
    }
    match path_buf.parent() {
        Some(p) => repo_find(p),
        None => Ok(None),
    }
}

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

pub struct TreeNode {
    pub mode: u32,
    pub path: PathBuf,
    pub object_name: String,
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
        let path_buf = PathBuf::from_str(path)?;
        let object_name =
            hex::encode(&data[(space_pos + null_pos + 2)..(space_pos + null_pos + 22)]);
        Ok(TreeNodeParsingResult {
            consumed: space_pos + null_pos + 22,
            node: TreeNode {
                mode: mode,
                path: path_buf,
                object_name: object_name,
            },
        })
    }

    pub fn compare(a: &TreeNode, b: &TreeNode) -> Ordering {
        a.ordering_path().cmp(&b.ordering_path())
    }

    fn ordering_path(&self) -> String {
        if self.mode >= 0o100000 {
            self.path.to_string_lossy().to_string()
        } else {
            self.path.to_string_lossy().to_string() + "/"
        }
    }
}

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
        self.entries.sort_by(|a, b| TreeNode::compare(a, b));
    }

    pub fn checkout(&self, repo: &Repository, path: &PathBuf) -> Result<(), anyhow::Error> {
        for entry in &self.entries {
            let obj = repo.object_read(&entry.object_name)?;
            let Some(obj) = obj else {
                return Err(anyhow!("Object {} not found", entry.object_name));
            };
            let path = path.join(&entry.path);
            match obj {
                StoredObject::Tree(tree) => {
                    fs::create_dir(&path)?;
                    tree.checkout(repo, &path)?;
                }
                StoredObject::Blob(blob) => {
                    fs::write(path, blob.data)?;
                }
                StoredObject::Tag(_) => (),
                StoredObject::Commit(_) => {
                    return Err(anyhow!(
                        "Submodules, like object {}, are not currently supported.",
                        entry.object_name
                    ));
                }
            }
        }
        Ok(())
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
            buf.append(hex::decode(&entry.object_name).unwrap().as_mut());
        }
    }

    fn deserialise(data: &[u8]) -> Self::Implementation
    where
        Self: Sized,
    {
        Tree::from_bytes(data).unwrap()
    }
}

fn stored_object_matches_kind(kind: &ObjectKind, obj: &StoredObject) -> bool {
    match kind {
        ObjectKind::Blob => {
            if let StoredObject::Blob(_) = obj {
                true
            } else {
                false
            }
        }
        ObjectKind::Tree => {
            if let StoredObject::Tree(_) = obj {
                true
            } else {
                false
            }
        }
        ObjectKind::Commit => {
            if let StoredObject::Commit(_) = obj {
                true
            } else {
                false
            }
        }
        ObjectKind::Tag => {
            if let StoredObject::Tag(_) = obj {
                true
            } else {
                false
            }
        }
    }
}

pub struct IndexEntry {
    pub ctime: DateTime<Utc>,
    pub mtime: DateTime<Utc>,
    pub dev: u32,
    pub ino: u32,
    pub mode_type: u16,
    pub mode_perms: u16,
    pub uid: u32,
    pub gid: u32,
    pub fsize: u32,
    pub flag_assume_valid: bool,
    pub flag_stage: u8,
    pub object_id: String,
    pub object_name: String,
}

impl IndexEntry {
    pub fn byte_length(&self) -> usize {
        // Round up to 8-byte boundary
        let blocks = (self.object_name.as_bytes().len() + 63) / 8 + 1;
        blocks * 8
    }

    pub fn from_bytes(data: &[u8]) -> Result<IndexEntry, InvalidIndexEntryError> {
        // Shortest possible index entry length, for a single-character filename.
        if data.len() < 64 {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::TooShort,
            });
        }
        let ctime_s = helpers::u32_from_be_bytes_unchecked(data, 0);
        let ctime_ns = helpers::u32_from_be_bytes_unchecked(data, 4);
        let ctime = DateTime::<Utc>::from_timestamp(ctime_s.into(), ctime_ns);
        let Some(ctime) = ctime else {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::UnparseableTimestamp(ctime_s, ctime_ns),
            });
        };
        let mtime_s = helpers::u32_from_be_bytes_unchecked(data, 8);
        let mtime_ns = helpers::u32_from_be_bytes_unchecked(data, 12);
        let mtime = DateTime::<Utc>::from_timestamp(mtime_s.into(), mtime_ns);
        let Some(mtime) = mtime else {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::UnparseableTimestamp(mtime_s, mtime_ns),
            });
        };
        let dev = helpers::u32_from_be_bytes_unchecked(data, 16);
        let ino = helpers::u32_from_be_bytes_unchecked(data, 20);
        let mode = helpers::u16_from_be_bytes_unchecked(data, 26);
        let mode_type = mode >> 12;
        if mode_type != 0b1000 && mode_type != 0b1010 && mode_type != 0b1110 {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::UnexpectedMode(mode_type),
            });
        }
        let mode_perms = mode & 0x1FF;
        if mode_perms != 0 && mode_perms != 0o755 && mode_perms != 0o644 {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::UnexpectedPermissions(mode_perms),
            });
        }
        let uid = helpers::u32_from_be_bytes_unchecked(data, 28);
        let gid = helpers::u32_from_be_bytes_unchecked(data, 32);
        let fsize = helpers::u32_from_be_bytes_unchecked(data, 36);
        let object_id = hex::encode(&data[40..60]);
        let flags = helpers::u16_from_be_bytes_unchecked(data, 60);
        let assume_valid = flags & 0x8000 != 0;
        let stage = u8::try_from((flags >> 12) & 3).unwrap();
        let name_len: usize = (flags & 0xFFF).into();
        let name;
        if data.len() < name_len + 63 {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::TooShort,
            });
        }
        if name_len < 0xFFF {
            if data[name_len + 62] != 0 {
                return Err(InvalidIndexEntryError {
                    error_kind: errors::InvalidIndexEntryKind::NameNotNullTerminated,
                });
            }
            name = String::from_utf8_lossy(&data[62..(name_len + 62)]);
        } else {
            let real_name_len = data[62..].iter().position(|x| *x == 0);
            let Some(real_name_len) = real_name_len else {
                return Err(InvalidIndexEntryError {
                    error_kind: errors::InvalidIndexEntryKind::NameNotNullTerminated,
                });
            };
            name = String::from_utf8_lossy(&data[62..(real_name_len + 62)]);
        }
        Ok(IndexEntry {
            ctime,
            mtime,
            dev,
            ino,
            mode_type,
            mode_perms,
            uid,
            gid,
            fsize,
            flag_assume_valid: assume_valid,
            flag_stage: stage,
            object_id,
            object_name: name.to_string(),
        })
    }
}

pub struct Index {
    pub version: u32,
    entries: Vec<IndexEntry>,
}

impl Index {
    pub fn new() -> Self {
        Index {
            version: 2,
            entries: Vec::<IndexEntry>::new(),
        }
    }

    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    pub fn from_bytes(data: &[u8]) -> Result<Index, InvalidIndexError> {
        if data.len() < 12 {
            return Err(InvalidIndexError {
                error_kind: errors::InvalidIndexKind::TooShort,
            });
        }
        if data[..4] != *b"DIRC" {
            return Err(InvalidIndexError {
                error_kind: errors::InvalidIndexKind::MissingMagic,
            });
        }
        let version = helpers::u32_from_be_bytes_unchecked(data, 4);
        if version != 2 {
            return Err(InvalidIndexError {
                error_kind: errors::InvalidIndexKind::UnsupportedVersion(version),
            });
        }
        let count = usize::try_from(helpers::u32_from_be_bytes_unchecked(data, 8)).unwrap();
        let mut entries = Vec::<IndexEntry>::with_capacity(count);
        let mut idx = 12;
        for _ in 0..count {
            let entry = IndexEntry::from_bytes(&data[idx..]);
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    return Err(InvalidIndexError {
                        error_kind: errors::InvalidIndexKind::InvalidEntry(e),
                    })
                }
            };
            idx += entry.byte_length();
            entries.push(entry);
        }
        Ok(Index { version, entries })
    }
}
