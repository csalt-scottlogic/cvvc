//! This module contains the parsing code for Git packfiles
//!
//! At present, this module only supports reading packfiles, and only supports packfiles with version 2 indexes,
//! and that use SHA-1 object IDs.

use anyhow::anyhow;
use std::{
    cmp::Ordering,
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use crate::{
    objects::{GitObject, ObjectKind, RawObject},
    stores::ObjectStore,
};

mod helpers;
mod indexer;

/// An object store which represents a packfile and its index.
pub struct PackStore {
    _pack_name: String,
    primary_file: PathBuf,
    index_file: PathBuf,
    item_count: u32,
    primary_file_len: u64,
}

impl PackStore {
    /// Create a new [`PackStore`] representing a packfile's metadata.
    ///
    /// This function takes two parameters, the base path (normally `.git/objects/pack`) and the pack name,
    /// which is the name of any of the files making up the entire pack, minus their extension.  For example, the
    /// primary packfile is normally `<pack_name>.pack`, and its associated index file is `<pack_name>.idx`.  
    /// By convention `<pack_name>` is `pack-<nnnnnnnn>` where the digits are the hexadecimal representation of
    /// the pack's checksum, which also forms the trailer of the primary packfile; the function does not confirm this.
    ///
    /// This function returns an error if:
    /// - the `base_path` is not a valid directory
    /// - the primary packfile does not exist
    /// - the index file does not exist
    /// - the index file cannot be opened and read
    /// - the primary packfile's length cannot be determined
    ///
    /// This function returns successfully if the packfile uses SHA-256 for object IDs.  However, as CVVC does not
    /// yet support SHA-256, other functions and methods in this module will likely error or give incorrect results
    /// when run against a SHA-256 packfile.
    pub fn new<P: AsRef<Path>>(base_path: P, pack_name: &str) -> Result<Self, anyhow::Error> {
        println!("DEBUG: loading pack {}", pack_name);
        let base_path = base_path.as_ref();
        if !base_path.is_dir() {
            return Err(anyhow!("base path is not a directory"));
        }
        let primary_file = helpers::primary_file_name(base_path, pack_name);
        if !primary_file.is_file() {
            return Err(anyhow!("pack file does not exist"));
        }
        let index_file = helpers::index_file_name(base_path, pack_name);
        if !index_file.is_file() {
            if !index_file.exists() {
                println!("Reindexing pack {}", pack_name);
                indexer::index(base_path, pack_name)?;
            } else {
                return Err(anyhow!("pack file exists but is not a file"));
            }
        }
        let mut index = helpers::open_file_from_path(&index_file)?;
        let item_count = Self::get_pack_item_count(&mut index)?;
        let primary_file_len = Self::get_path_len(&primary_file)?;
        Ok(PackStore {
            _pack_name: pack_name.to_string(),
            primary_file,
            index_file,
            item_count,
            primary_file_len,
        })
    }

    /// Find all packs in a given directory.
    ///
    /// This function iterates over the contents of a directory, and finds all of the files whose names appear
    /// to follow the packfile naming convention.  It then tries to load the metadata for each file stem it finds
    /// by calling [`PackStore::new`] for each file stem.
    ///
    /// This function will return an error if calling [`PackStore::new`] on any apparent packfile returns an error,
    /// regardless of whether or not other packfiles in the directory can be loaded successfully.  See the
    /// documentation for [`PackStore::new`] for reasons why this may happen.
    ///
    /// This function will also return an error if it encounters any filesystem errors when trying to identify potential
    /// packfiles.
    ///
    /// When searching for candidate packs, this function does not distinguish between packfiles that use SHA-1 and
    /// those that use SHA-256.  However, because CVVC does not at present support SHA-256 repositories, attempting to
    /// load SHA-256 packfiles is highly likely to cause runtime errors.
    pub fn find_packs<P: AsRef<Path>>(base_path: P) -> Result<Vec<Self>, anyhow::Error> {
        let base_path = base_path.as_ref();
        if !base_path.is_dir() {
            return Err(anyhow!("base path is not a directory"));
        }
        let mut pack_names = HashSet::<String>::new();
        for dir_entry in fs::read_dir(base_path)? {
            let dir_entry = dir_entry?;
            let file_type = dir_entry.file_type()?;
            if file_type.is_file() {
                if let Some(candidate_pack) = Path::new(&dir_entry.file_name()).file_prefix() {
                    let candidate_pack = candidate_pack.to_string_lossy().to_string();
                    if candidate_pack.starts_with("pack-")
                        && candidate_pack
                            .chars()
                            .skip(5)
                            .all(|x| x.is_ascii_hexdigit())
                    {
                        pack_names.insert(candidate_pack);
                    }
                }
            }
        }
        pack_names
            .iter()
            .map(|x| Self::new(base_path, x))
            .collect::<Result<Vec<Self>, anyhow::Error>>()
    }

    fn check_index_version<R>(index_file: &mut BufReader<R>) -> Result<bool, anyhow::Error>
    where
        R: Seek,
        R: Read,
    {
        index_file.rewind()?;
        let mut buf = [0u8; 8];
        index_file.read_exact(&mut buf)?;
        Ok(buf == helpers::INDEX_HEADER)
    }

    fn get_index_offset_range<R>(
        index_file: &mut BufReader<R>,
        partial_object_id: &str,
    ) -> Result<PackIndexOffsetRange, anyhow::Error>
    where
        R: Seek,
        R: Read,
    {
        if partial_object_id.len() < 2 || partial_object_id.chars().any(|c| !c.is_ascii_hexdigit())
        {
            return Err(anyhow!("object ID is invalid or insufficient"));
        }
        let object_id_start = hex::decode(&partial_object_id[..2])?[0];
        let start_idx = if object_id_start == 0 {
            0u32
        } else {
            Self::get_index_offset(index_file, object_id_start - 1)?
        };
        let end_idx = Self::get_index_offset(index_file, object_id_start)?;
        Ok(PackIndexOffsetRange { start_idx, end_idx })
    }

    fn get_index_offset<R>(
        index_file: &mut BufReader<R>,
        object_id_start: u8,
    ) -> Result<u32, anyhow::Error>
    where
        R: Seek,
        R: Read,
    {
        index_file.seek(std::io::SeekFrom::Start(u64::from(object_id_start) * 4 + 8))?;
        let mut buf = [0u8; 4];
        index_file.read_exact(&mut buf)?;
        Ok(u32::from_be_bytes(buf))
    }

    fn get_pack_item_count<R>(index_file: &mut BufReader<R>) -> Result<u32, anyhow::Error>
    where
        R: Read,
        R: Seek,
    {
        index_file.seek(SeekFrom::Start(1028))?;
        let mut buf = [0u8; 4];
        index_file.read_exact(&mut buf)?;
        Ok(u32::from_be_bytes(buf))
    }

    fn search_index_objects<R>(
        &self,
        index_file: &mut BufReader<R>,
        partial_object_id: &str,
    ) -> Result<Vec<PackIndexObject>, anyhow::Error>
    where
        R: Seek,
        R: Read,
    {
        let mut results = Vec::<PackIndexObject>::new();
        let starting_range = Self::get_index_offset_range(index_file, partial_object_id)?;
        if starting_range.is_empty() {
            return Ok(results);
        }
        let first_candidate =
            Self::find_index_object(index_file, partial_object_id, &starting_range)?;
        let Some(first_candidate) = first_candidate else {
            return Ok(results);
        };
        if partial_object_id.len() == 20 {
            // in other words, it's not partial
            return Ok(results);
        }
        let idx_in_range = first_candidate.idx;
        results.push(first_candidate);
        let mut idx_target = idx_in_range;
        loop {
            if idx_target == 0 {
                break;
            }
            idx_target -= 1;
            let obj_at_target = Self::get_index_object_id_at_pos(index_file, idx_target)?;
            if obj_at_target.starts_with(partial_object_id) {
                results.push(PackIndexObject {
                    object_id: obj_at_target,
                    idx: idx_target,
                });
            } else {
                break;
            }
        }
        idx_target = idx_in_range;
        loop {
            idx_target += 1;
            if idx_target >= self.item_count {
                break;
            }
            let obj_at_target = Self::get_index_object_id_at_pos(index_file, idx_target)?;
            if obj_at_target.starts_with(partial_object_id) {
                results.push(PackIndexObject {
                    object_id: obj_at_target,
                    idx: idx_target,
                });
            } else {
                break;
            }
        }
        Ok(results)
    }

    fn find_index_object<R>(
        index_file: &mut BufReader<R>,
        partial_object_id: &str,
        obj_range: &PackIndexOffsetRange,
    ) -> Result<Option<PackIndexObject>, anyhow::Error>
    where
        R: Seek,
        R: Read,
    {
        if obj_range.is_empty() {
            return Ok(None);
        }
        if obj_range.size() == 1 {
            let obj_id = Self::get_index_object_id_at_pos(index_file, obj_range.start_idx)?;
            if obj_id.starts_with(partial_object_id) {
                return Ok(Some(PackIndexObject {
                    object_id: obj_id,
                    idx: obj_range.start_idx,
                }));
            } else {
                return Ok(None);
            }
        }
        let mid = obj_range.mid();
        let obj_id = Self::get_index_object_id_at_pos(index_file, mid)?;
        if obj_id.starts_with(partial_object_id) {
            return Ok(Some(PackIndexObject {
                object_id: obj_id,
                idx: mid,
            }));
        }
        let recurse_range = match partial_object_id.cmp(&obj_id) {
            Ordering::Less => PackIndexOffsetRange {
                start_idx: obj_range.start_idx,
                end_idx: mid,
            },
            Ordering::Greater => PackIndexOffsetRange {
                start_idx: mid + 1,
                end_idx: obj_range.end_idx,
            },
            Ordering::Equal => {
                return Ok(Some(PackIndexObject {
                    object_id: obj_id,
                    idx: mid,
                })); // here to satisfy the compiler; we've already ruled this out
            }
        };
        Self::find_index_object(index_file, partial_object_id, &recurse_range)
    }

    fn index_object_idx_to_id_offset(item_idx: u32) -> u64 {
        u64::from(item_idx) * 20 + 1032
    }

    fn index_object_idx_to_address_offset(&self, item_idx: u32) -> u64 {
        1032u64 + u64::from(self.item_count) * 24 + u64::from(item_idx) * 4
    }

    fn index_object_idx_to_large_address_offset(&self, large_offset_index: u32) -> u64 {
        1032u64 + u64::from(self.item_count) * 28 + u64::from(large_offset_index) * 8
    }

    fn get_object_address_from_index<R>(
        &self,
        index_file: &mut BufReader<R>,
        item_idx: u32,
    ) -> Result<u64, anyhow::Error>
    where
        R: Seek,
        R: Read,
    {
        index_file.seek(SeekFrom::Start(
            self.index_object_idx_to_address_offset(item_idx),
        ))?;
        let mut buf = [0u8; 4];
        index_file.read_exact(&mut buf)?;
        let small_offset = u32::from_be_bytes(buf);
        if small_offset < 0x80000000 {
            return Ok(u64::from(small_offset));
        }
        let large_offset_index = small_offset & 0x80000000;
        index_file.seek(SeekFrom::Start(
            self.index_object_idx_to_large_address_offset(large_offset_index),
        ))?;
        let mut buf = [0u8; 8];
        index_file.read_exact(&mut buf)?;
        Ok(u64::from_be_bytes(buf))
    }

    fn get_object_address(&self, object_id: &str) -> Result<Option<u64>, anyhow::Error> {
        let mut index_file = self.open_index_file()?;
        if !Self::check_index_version(&mut index_file)? {
            return Err(anyhow!("pack index file format not recognised"));
        }
        let targets = self.search_index_objects(&mut index_file, object_id)?;
        if targets.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.get_object_address_from_index(
            &mut index_file,
            targets[0].idx,
        )?))
    }

    fn get_index_object_id_at_pos<R>(
        index_file: &mut BufReader<R>,
        idx: u32,
    ) -> Result<String, anyhow::Error>
    where
        R: Seek,
        R: Read,
    {
        index_file.seek(SeekFrom::Start(Self::index_object_idx_to_id_offset(idx)))?;
        let mut buf = [0u8; 20];
        index_file.read_exact(&mut buf)?;
        Ok(hex::encode(buf))
    }

    fn open_index_file(&self) -> Result<BufReader<File>, anyhow::Error> {
        helpers::open_file_from_path(&self.index_file)
    }

    fn get_file_len(f: &File) -> Result<u64, anyhow::Error> {
        let metadata = f.metadata()?;
        Ok(metadata.len())
    }

    fn get_path_len<P: AsRef<Path>>(path: P) -> Result<u64, anyhow::Error> {
        let file = OpenOptions::new().read(true).open(path)?;
        Self::get_file_len(&file)
    }

    fn open_pack_file(&self) -> Result<BufReader<File>, anyhow::Error> {
        helpers::open_file_from_path(&self.primary_file)
    }
}

impl ObjectStore for PackStore {
    /// Attempt to create a new packfile.  As this is unsupported, this method always returns an error.
    fn create(&self) -> Result<(), anyhow::Error> {
        Err(anyhow!("pack creation not yet supported"))
    }

    /// Confirm whether or not this packfile is writeable.  This method always returns false.
    fn _is_writeable(&self) -> bool {
        false
    }

    /// Search the packfile for objects whose IDs begin with a given prefix.
    ///
    /// This method returns a [`Vec<String>`] of complete object IDs which begin with the given prefix.  This
    /// can be a empty, if no such objects exist in this packfile.
    ///
    /// This method only searches the packfile index, and assumes that the index is valid.  It does not
    /// confirm that the index is valid, that the index points correctly to the listed objects, or that the objects
    /// can be successfully read from the packfile.
    ///
    /// This method will return an error if the index is not a version 2 index, or if any filesystem errors
    /// occur whilst reading the index.  It also returns an error if the parameter is not a valid partial object ID,
    /// or if it only consists of a single character.
    fn search_objects(&self, partial_object_id: &str) -> Result<Vec<String>, anyhow::Error> {
        let mut reader = self.open_index_file()?;
        if !Self::check_index_version(&mut reader)? {
            return Err(anyhow!("pack index file format not recognised"));
        }
        let found_objects = self.search_index_objects(&mut reader, partial_object_id)?;
        Ok(found_objects.into_iter().map(|x| x.object_id).collect())
    }

    /// Confirm whether or not the given object exists in the packfile.
    ///
    /// On success, this method determines whether or not an object with the given ID is present in the packfile index.
    /// It assumes that the index is valid.  It does not confirm whether or not the index is valid, that the index
    /// points correctly to the object, or that the object can be successfully read from the packfile.
    ///
    /// This method will return an error if the index is not a version 2 index, or if any filesystem errors occur whilst
    /// reading the index.  It also returns an error if the parameter is not a valid partial object ID.  If the parameter
    /// is a valid partial object ID longer than 1 character, it returns `Ok(false)`.
    fn has_object(&self, object_id: &str) -> Result<bool, anyhow::Error> {
        let mut reader = self.open_index_file()?;
        if !Self::check_index_version(&mut reader)? {
            return Err(anyhow!("pack index file format not recognised"));
        }
        let found_objects = self.search_index_objects(&mut reader, object_id)?;
        Ok(found_objects.len() == 1)
    }

    /// Read a [`RawObject`] from the packfile.
    ///
    /// This method reads a [`RawObject`] from the packfile, if it exists in the packfile.  If the given ID is a legal
    /// partial object ID but is not the full object ID of an object in this packfile, the method returns `Ok(None)`.
    ///
    /// An error is returned if the object is present in the packfile, but is a "named delta" (also known as an
    /// OBJ_REF_DELTA) object.  These objects are found in "thin packs", so called because they can consist solely
    /// of deltafied objects and do not need to contain the deltafied objects' dependencies.  CVVC does not at present
    /// support thin packs, which should normally only be encountered when transferring packs over the network
    ///
    /// An error is also returned if:
    /// - the object ID is not a legal partial object ID longer than 1 character
    /// - the packfile or pack index file cannot be read, or another filesystem error occurs
    /// - the packfile fails basic format checks
    /// - the index file fails basic format checks
    /// - the index file is not a version 2 index
    /// - the object's metadata cannot be successfully loaded from the packfile
    /// - the object's packfile data cannot be successfully decompressed
    /// - the object is a delta object and one of its ancestors cannot be loaded successfully from the packfile
    fn read_raw_object(&self, object_id: &str) -> Result<Option<RawObject>, anyhow::Error> {
        let object_address = self.get_object_address(object_id)?;
        let Some(object_address) = object_address else {
            return Ok(None);
        };
        let mut pack_file = self.open_pack_file()?;
        if !helpers::check_pack_version(&mut pack_file, Some(self.item_count))? {
            return Err(anyhow!("pack file format not recognised"));
        }
        let (raw_object, _) = helpers::read_raw_object_at_address(
            &mut pack_file,
            object_address,
            self.primary_file_len,
        )?;
        Ok(Some(raw_object))
    }

    /// Fails to write a [`RawObject`] to the packfile.  This method always returns an error, because packfiles are not
    /// editable.
    fn write_raw_object(&self, _obj: &RawObject) -> Result<String, anyhow::Error> {
        Err(anyhow!("writing to packs not implemented"))
    }

    /// Fails to write a [`GitObject`] to the packfile.  This method always returns an error, because packfiles are not
    /// editable.
    fn write_object(&self, _obj: &impl GitObject) -> Result<String, anyhow::Error> {
        Err(anyhow!("writing to packs not implemented"))
    }
}

#[derive(Debug)]
struct PackIndexOffsetRange {
    start_idx: u32,
    end_idx: u32,
}

impl PackIndexOffsetRange {
    fn is_empty(&self) -> bool {
        self.start_idx == self.end_idx
    }

    fn size(&self) -> u32 {
        self.end_idx - self.start_idx
    }

    fn mid(&self) -> u32 {
        self.start_idx + self.size() / 2
    }
}

struct PackIndexObject {
    object_id: String,
    idx: u32,
}

#[derive(Clone)]
enum PackedObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
    OffsetDelta(u64),
    NamedDelta(String),
}

impl TryFrom<PackedObjectType> for ObjectKind {
    type Error = anyhow::Error;

    fn try_from(value: PackedObjectType) -> Result<Self, Self::Error> {
        match value {
            PackedObjectType::Blob => Ok(ObjectKind::Blob),
            PackedObjectType::Commit => Ok(ObjectKind::Commit),
            PackedObjectType::Tree => Ok(ObjectKind::Tree),
            PackedObjectType::Tag => Ok(ObjectKind::Tag),
            _ => Err(anyhow!("unpacked objects must be undeltafied")),
        }
    }
}

impl PackedObjectType {
    fn is_base_object(&self) -> bool {
        !matches!(
            self,
            PackedObjectType::OffsetDelta(_) | PackedObjectType::NamedDelta(_)
        )
    }
}

enum PackedObjectTypeOnly {
    Commit,
    Tree,
    Blob,
    Tag,
    OffsetDelta,
    NamedDelta,
}

impl TryFrom<u8> for PackedObjectTypeOnly {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value & 0x70 {
            0x10 => PackedObjectTypeOnly::Commit,
            0x20 => PackedObjectTypeOnly::Tree,
            0x30 => PackedObjectTypeOnly::Blob,
            0x40 => PackedObjectTypeOnly::Tag,
            0x60 => PackedObjectTypeOnly::OffsetDelta,
            0x70 => PackedObjectTypeOnly::NamedDelta,
            _ => {
                return Err(anyhow!("invalid packed object type"));
            }
        })
    }
}

struct PackedObjectMetadata {
    unpacked_size: u64,
    data_start_address: u64,
    pub kind: PackedObjectType,
    packed_size: Option<u64>,
}

impl PackedObjectMetadata {
    fn try_from_type_only(
        kind: PackedObjectTypeOnly,
        size: u64,
        data_start_address: u64,
        delta_offset: Option<u64>,
        base_object: Option<String>,
    ) -> Result<Self, anyhow::Error> {
        let kind = match kind {
            PackedObjectTypeOnly::Commit => PackedObjectType::Commit,
            PackedObjectTypeOnly::Tree => PackedObjectType::Tree,
            PackedObjectTypeOnly::Blob => PackedObjectType::Blob,
            PackedObjectTypeOnly::Tag => PackedObjectType::Tag,
            PackedObjectTypeOnly::NamedDelta => match base_object {
                Some(base) => PackedObjectType::NamedDelta(base),
                None => return Err(anyhow!("base object name not provided")),
            },
            PackedObjectTypeOnly::OffsetDelta => match delta_offset {
                Some(offset) => PackedObjectType::OffsetDelta(offset),
                None => return Err(anyhow!("delta offset not provided")),
            },
        };
        Ok(PackedObjectMetadata {
            unpacked_size: size,
            data_start_address,
            kind,
            packed_size: None,
        })
    }

    fn is_base_object(&self) -> bool {
        self.kind.is_base_object()
    }

    fn combine(&self, other: &Self) -> Self {
        Self {
            unpacked_size: self.unpacked_size,
            data_start_address: self.data_start_address,
            kind: other.kind.clone(),
            packed_size: self.packed_size,
        }
    }
}
