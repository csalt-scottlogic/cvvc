use anyhow::anyhow;
use flate2::bufread::ZlibDecoder;
use std::{
    cmp::Ordering,
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use crate::{
    objects::{ObjectKind, ObjectMetadata, RawObject},
    stores::ObjectStore,
};

pub struct PackStore {
    _pack_name: String,
    primary_file: PathBuf,
    index_file: PathBuf,
    item_count: u32,
    primary_file_len: u64,
}

impl PackStore {
    pub fn new<P: AsRef<Path>>(base_path: P, pack_name: &str) -> Result<Self, anyhow::Error> {
        let base_path = base_path.as_ref();
        if !base_path.is_dir() {
            return Err(anyhow!("base path is not a directory"));
        }
        let primary_file = base_path.join(pack_name).with_added_extension("pack");
        if !primary_file.is_file() {
            return Err(anyhow!("pack file does not exist"));
        }
        let index_file = base_path.join(pack_name).with_added_extension("idx");
        if !index_file.is_file() {
            return Err(anyhow!("index file does not exist"));
        }
        let mut index = Self::open_file_from_path(&index_file)?;
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
        Ok(buf == [255u8, 116, 79, 99, 0, 0, 0, 2])
    }

    fn check_pack_version<R>(&self, pack_file: &mut BufReader<R>) -> Result<bool, anyhow::Error>
    where
        R: Seek,
        R: Read,
    {
        pack_file.rewind()?;
        let mut buf = [0u8; 8];
        pack_file.read_exact(&mut buf)?;
        if buf != [80u8, 65, 67, 75, 0, 0, 0, 2] {
            return Ok(false);
        }
        let mut buf = [0u8; 4];
        pack_file.read_exact(&mut buf)?;
        if self.item_count != u32::from_be_bytes(buf) {
            Ok(false)
        } else {
            Ok(true)
        }
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
        Self::open_file_from_path(&self.index_file)
    }

    fn open_file_from_path<P: AsRef<Path>>(path: P) -> Result<BufReader<File>, anyhow::Error> {
        let file = OpenOptions::new().read(true).open(path)?;
        Ok(BufReader::new(file))
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
        Self::open_file_from_path(&self.primary_file)
    }

    fn get_packed_object_metadata<R>(
        &self,
        pack_file: &mut BufReader<R>,
        address: u64,
    ) -> Result<PackedObjectMetadata, anyhow::Error>
    where
        R: Read,
        R: Seek,
    {
        pack_file.seek(SeekFrom::Start(address))?;
        let mut buf = if self.primary_file_len - address > 30 {
            // enough data to encode a 64-bit length followed by an object ID.

            vec![0u8; 30]
        } else {
            // however, it's possible for a very small offset delta object to be less than
            // ten bytes, so a 30-byte buffer would take us past the end of the file, so...
            // (safe unwrap() - the result of the subtraction will be <1byte)
            vec![0u8; (self.primary_file_len - address).try_into().unwrap()]
        };

        pack_file.read_exact(&mut buf)?;
        let packed_object_type: PackedObjectTypeOnly = buf[0].try_into()?;
        let mut object_size: u64 = (buf[0] & 0xf).into();
        let mut bytes_read = 1;
        while buf[bytes_read - 1] > 0x80 {
            object_size |= ((buf[bytes_read] & 0x7f) as u64) << (4 + 7 * (bytes_read - 1));
            bytes_read += 1;
            if bytes_read >= buf.len() {
                break;
            }
        }

        let base_object = if let PackedObjectTypeOnly::NamedDelta = packed_object_type {
            let val = Some(hex::encode(&buf[bytes_read..(bytes_read + 20)]));
            bytes_read += 20;
            val
        } else {
            None
        };
        let delta_offset = if let PackedObjectTypeOnly::OffsetDelta = packed_object_type {
            let mut offset = 0u64;
            while buf[bytes_read] >= 0x80 {
                offset |= ((buf[bytes_read] & 0x7f) + 1) as u64;
                offset <<= 7;
                bytes_read += 1;
            }
            offset |= (buf[bytes_read] & 0x7f) as u64;
            bytes_read += 1;
            Some(offset)
        } else {
            None
        };
        let data_start_address = address + (bytes_read as u64);
        PackedObjectMetadata::try_from_type_only(
            packed_object_type,
            object_size,
            data_start_address,
            delta_offset,
            base_object,
        )
    }

    fn read_at_address<R>(
        &self,
        pack_file: &mut BufReader<R>,
        address: u64,
    ) -> Result<(PackedObjectMetadata, Vec<u8>), anyhow::Error>
    where
        R: Read,
        R: Seek,
    {
        let meta = self.get_packed_object_metadata(pack_file, address)?;
        pack_file.seek(SeekFrom::Start(meta.data_start_address))?;
        let mut decompressor = ZlibDecoder::new(pack_file);
        let mut data = Vec::<u8>::with_capacity(meta.size as usize);
        decompressor.read_to_end(&mut data)?;
        let reusable_file = decompressor.into_inner();
        if meta.is_base_object() {
            Ok((meta, data))
        } else if let PackedObjectType::OffsetDelta(offset) = meta.kind {
            let (base_meta, base_data) = self.read_at_address(reusable_file, address - offset)?;
            Ok((meta.combine(&base_meta), combine_data(&base_data, &data)))
        } else if let PackedObjectType::NamedDelta(oid) = meta.kind {
            Err(anyhow!(
                "cannot load named delta offset: relies on object {oid}"
            ))
        } else {
            Err(anyhow!("unsupported packed object type"))
        }
    }
}

impl ObjectStore for PackStore {
    fn create(&self) -> Result<(), anyhow::Error> {
        Err(anyhow!("pack creation not yet supported"))
    }

    fn _is_writeable(&self) -> bool {
        false
    }

    fn search_objects(&self, partial_object_id: &str) -> Result<Vec<String>, anyhow::Error> {
        let mut reader = self.open_index_file()?;
        if !Self::check_index_version(&mut reader)? {
            return Err(anyhow!("pack index file format not recognised"));
        }
        let found_objects = self.search_index_objects(&mut reader, partial_object_id)?;
        Ok(found_objects.into_iter().map(|x| x.object_id).collect())
    }

    fn has_object(&self, object_id: &str) -> Result<bool, anyhow::Error> {
        let mut reader = self.open_index_file()?;
        if !Self::check_index_version(&mut reader)? {
            return Err(anyhow!("pack index file format not recognised"));
        }
        let found_objects = self.search_index_objects(&mut reader, object_id)?;
        Ok(!found_objects.is_empty())
    }

    fn read_object(&self, object_id: &str) -> Result<Option<RawObject>, anyhow::Error> {
        let object_address = self.get_object_address(object_id)?;
        let Some(object_address) = object_address else {
            return Ok(None);
        };
        let mut pack_file = self.open_pack_file()?;
        if !self.check_pack_version(&mut pack_file)? {
            return Err(anyhow!("pack file format not recognised"));
        }
        let (meta, data) = self.read_at_address(&mut pack_file, object_address)?;
        if meta.is_base_object() {
            let metadata = ObjectMetadata::new(ObjectKind::try_from(meta.kind)?, data.len());
            Ok(Some(RawObject::from_headless_data(
                &data, object_id, metadata,
            )))
        } else if let PackedObjectType::OffsetDelta(offset) = meta.kind {
            let (base_meta, base_data) =
                self.read_at_address(&mut pack_file, object_address - offset)?;
            let combined_meta = meta.combine(&base_meta);
            let unpacked_metadata = ObjectMetadata::new(
                ObjectKind::try_from(combined_meta.kind)?,
                combined_meta.size as usize,
            );
            Ok(Some(RawObject::from_headless_data(
                &combine_data(&base_data, &data),
                object_id,
                unpacked_metadata,
            )))
        } else {
            todo!();
        }
    }

    fn write_raw_object(&self, _obj: &RawObject) -> Result<String, anyhow::Error> {
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
pub enum PackedObjectType {
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

pub struct PackedObjectMetadata {
    size: u64,
    data_start_address: u64,
    pub kind: PackedObjectType,
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
            size,
            data_start_address,
            kind,
        })
    }

    fn is_base_object(&self) -> bool {
        self.kind.is_base_object()
    }

    fn combine(&self, other: &Self) -> Self {
        Self {
            size: self.size,
            data_start_address: self.data_start_address,
            kind: other.kind.clone(),
        }
    }
}

fn combine_data(base_data: &[u8], apply_commands: &[u8]) -> Vec<u8> {
    let mut result = Vec::<u8>::new();
    let mut idx = 0;

    // The commands start with two sizes, the size of the base and the size of the output.
    // We could read these for verification, if I was a Good Girl, but that can be a job for later.
    // Instead, we need to find the byte following the second byte less than 128 and start working from there.
    let mut non_continuation = 0;
    while non_continuation < 2 {
        if apply_commands[idx] < 0x80 {
            non_continuation += 1;
        }
        idx += 1;
    }

    while idx < apply_commands.len() {
        let command = DeltaCommand::from_bytes(&apply_commands[idx..]);
        match command.kind {
            DeltaCommandType::Add(sz) => {
                result.extend_from_slice(&apply_commands[(idx + 1)..(idx + 1 + sz)])
            }
            DeltaCommandType::Copy { offset, size } => {
                result.extend_from_slice(&base_data[offset..(offset + size)])
            }
        }
        idx += command.len;
    }
    result
}

enum DeltaCommandType {
    Copy { offset: usize, size: usize },
    Add(usize),
}

struct DeltaCommand {
    len: usize,
    kind: DeltaCommandType,
}

impl DeltaCommand {
    fn from_bytes(data: &[u8]) -> Self {
        if data[0] < 0x80 {
            let size = data[0] & 0x7f;
            Self {
                len: size as usize + 1,
                kind: DeltaCommandType::Add(size as usize),
            }
        } else {
            let bits = data[0] & 0x7f;
            if bits == 0 {
                Self {
                    len: 1,
                    kind: DeltaCommandType::Copy {
                        offset: 0,
                        size: 0x10000,
                    },
                }
            } else {
                let mut offset = 0usize;
                let mut size = 0usize;
                let mut bit = 1u8;
                let mut idx = 1;
                for i in 0..4 {
                    if bits & bit != 0 {
                        offset |= (data[idx] as usize) << (i * 8);
                        idx += 1;
                    }
                    bit <<= 1;
                }
                for i in 4..7 {
                    if bits & bit != 0 {
                        size |= (data[idx] as usize) << ((i - 4) * 8);
                        idx += 1;
                    }
                    bit <<= 1;
                }
                if size == 0 {
                    size = 0x10000;
                }
                Self {
                    len: bits.count_ones() as usize + 1,
                    kind: DeltaCommandType::Copy { offset, size },
                }
            }
        }
    }
}
