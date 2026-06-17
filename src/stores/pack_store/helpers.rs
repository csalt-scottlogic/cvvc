use std::{
    fs::{File, OpenOptions},
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use flate2::bufread::ZlibDecoder;
use sha1::{Digest, Sha1};

use crate::objects::{combine_object_delta_data, ObjectKind, ObjectMetadata, RawObjectData};

use super::{PackedObjectMetadata, PackedObjectType, PackedObjectTypeOnly};

pub const INDEX_HEADER: [u8; 8] = [255, 116, 79, 99, 0, 0, 0, 2];
pub const REV_INDEX_HEADER: [u8; 12] = [0x52, 0x49, 0x44, 0x58, 0, 0, 0, 1, 0, 0, 0, 1];

/// Given a directory and a pack name, returns the full path to the primary packfile.
pub fn primary_file_name<P: AsRef<Path>>(base_path: P, pack_name: &str) -> PathBuf {
    file_path_with_extension(base_path, pack_name, "pack")
}

/// Given a directory and a pack name, returns the full path to the pack index file.
pub fn index_file_name<P: AsRef<Path>>(base_path: P, pack_name: &str) -> PathBuf {
    file_path_with_extension(base_path, pack_name, "idx")
}

/// Given a directory and a pack name, returns the full path to the pack reverse index file.
pub fn rev_index_file_name<P: AsRef<Path>>(base_path: P, pack_name: &str) -> PathBuf {
    file_path_with_extension(base_path, pack_name, "rev")
}

fn file_path_with_extension<P: AsRef<Path>>(base_path: P, pack_name: &str, ext: &str) -> PathBuf {
    base_path.as_ref().join(pack_name).with_added_extension(ext)
}

/// Generate a string containing a large random number, which (we assume) can be used as a temporary filename
/// for a new pack being downloaded, before we know what its ID is.
pub fn randomish_string() -> String {
    let x: [u64; 4] = [
        rand::random(),
        rand::random(),
        rand::random(),
        rand::random(),
    ];
    format!("cvvc-cgt-{:x}{:x}{:x}{:x}", x[0], x[1], x[2], x[3])
}

/// Open a file and return a [`BufReader<File>`].
pub fn open_file_from_path<P: AsRef<Path>>(path: P) -> Result<BufReader<File>, anyhow::Error> {
    let file = OpenOptions::new().read(true).open(path)?;
    Ok(BufReader::new(file))
}

/// Copies the content of a reader to a new file.
///
/// # Errors
///
/// This function returns an error if the file already exists, if it cannot be created, or if
/// any errors occur reading from the reader or writing to the file.
pub fn store_from_reader<P: AsRef<Path>, R: Read>(
    mut reader: R,
    path: P,
) -> Result<u64, anyhow::Error> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let size = io::copy(&mut reader, &mut file)?;
    println!("{size} bytes downloaded");
    Ok(size)
}

pub fn confirm_pack_name<P: AsRef<Path>>(
    filename: P,
    expected_size: u64,
) -> Result<String, anyhow::Error> {
    let mut to_read = expected_size - 20;
    let mut file = OpenOptions::new().read(true).open(filename)?;
    const BLOCK_SZ: usize = 65536;
    let mut read_buffer = [0u8; BLOCK_SZ];
    let mut hasher = Sha1::new();
    loop {
        let block_size = if to_read as usize > BLOCK_SZ {
            BLOCK_SZ
        } else {
            to_read as usize
        };
        match file.read(&mut read_buffer[..block_size])? {
            0 => return Err(anyhow!("unexpected end of file")),
            BLOCK_SZ => {
                hasher.update(read_buffer);
                to_read -= BLOCK_SZ as u64;
            }
            x => {
                hasher.update(&read_buffer[..x]);
                to_read -= x as u64;
            }
        }
        if to_read == 0 {
            break;
        }
    }
    let checksum = hasher.finalize();
    let mut pack_checksum = [0u8; 20];
    file.read_exact(&mut pack_checksum)?;
    if checksum != pack_checksum.into() {
        Err(anyhow!("incorrect pack checksum"))
    } else {
        Ok(format!("pack-{}", hex::encode(checksum)))
    }
}

/// Check the magic header and version number of a `.pack` file.
///
/// At present, CVVC only understands pack version 2.
pub fn check_pack_version<R>(
    pack_file: &mut BufReader<R>,
    item_count: Option<u32>,
) -> Result<bool, anyhow::Error>
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
    if let Some(item_count) = item_count {
        let mut buf = [0u8; 4];
        pack_file.read_exact(&mut buf)?;
        if item_count != u32::from_be_bytes(buf) {
            Ok(false)
        } else {
            Ok(true)
        }
    } else {
        Ok(true)
    }
}

/// Seek to a given location in a [`BufReader<T>`] and read a `u32` value stored in network order,
/// starting at that location.
///
/// # Errors
///
/// This function assumes it will be able to read 4 bytes from the [`BufReader<T>`], and errors if it cannot.
pub fn read_u32_at<R>(file: &mut BufReader<R>, pos: u64) -> Result<u32, anyhow::Error>
where
    R: Seek,
    R: Read,
{
    file.seek(SeekFrom::Start(pos))?;
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf)?;
    Ok(u32::from_be_bytes(buf))
}

/// Get the metadata header of a packed object inside a packfile.
///
/// The [`PackedObjectMetadata`] struct returned by this function includes a `data_start_address` member which can
/// be used to determine the offset in the packfile that marks the start of the compressed object data.
///
/// The metadata also includes the unpacked object length and the object type.  If the object is a delta object, it also
/// references the object it is derived from, either as an address offset to an object in the same packfile, or the ID of
/// an object stored elsewhere.
///
/// Although the address offset is unsigned, it should be understood to represent the magnitude of a negative number,
/// because address offsets can only refer to objects located earlier in the pack.
pub fn get_packed_object_metadata<R>(
    pack_file: &mut BufReader<R>,
    address: u64,
    pack_file_len: u64,
) -> Result<PackedObjectMetadata, anyhow::Error>
where
    R: Read,
    R: Seek,
{
    pack_file.seek(SeekFrom::Start(address))?;
    let mut buf = if pack_file_len - address > 30 {
        // enough data to encode a 64-bit length followed by an object ID.

        vec![0u8; 30]
    } else {
        // however, it's possible for a very small offset delta object to be less than
        // ten bytes, so a 30-byte buffer would take us past the end of the file, so...
        // (safe unwrap() - the result of the subtraction will be <1byte)
        vec![0u8; (pack_file_len - address).try_into().unwrap()]
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
        let offset_nidx = delta_offset_read(&mut buf, bytes_read);
        bytes_read = offset_nidx.1;
        Some(offset_nidx.0)
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

fn delta_offset_read(buf: &Vec<u8>, mut idx: usize) -> (u64, usize) {
    let mut offset = 0u64;
    while buf[idx] >= 0x80 {
        offset |= (buf[idx] & 0x7f) as u64;
        offset += 1;
        offset <<= 7;
        idx += 1;
    }
    offset |= (buf[idx] & 0x7f) as u64;
    idx += 1;
    (offset, idx)
}

pub fn read_raw_object_at_address<R>(
    pack_file: &mut BufReader<R>,
    address: u64,
    file_len: u64,
) -> Result<(RawObjectData, u64), anyhow::Error>
where
    R: Read,
    R: Seek,
{
    let (meta, data) = read_at_address(pack_file, address, file_len)?;
    let Some(packed_size) = meta.packed_size else {
        return Err(anyhow!(
            "object packed size not present - this shouldn't happen"
        ));
    };
    Ok((
        construct_raw_object_from_packed(meta, data, pack_file, address, file_len)?,
        packed_size,
    ))
}

fn construct_raw_object_from_packed<R>(
    metadata: PackedObjectMetadata,
    data: Vec<u8>,
    pack_file: &mut BufReader<R>,
    address: u64,
    file_len: u64,
) -> Result<RawObjectData, anyhow::Error>
where
    R: Read,
    R: Seek,
{
    if let PackedObjectType::OffsetDelta(offset) = metadata.kind {
        let (base_meta, base_data) = read_at_address(pack_file, address - offset, file_len)?;
        let combined_meta = metadata.combine(&base_meta);
        let unpacked_metadata = ObjectMetadata::new(
            ObjectKind::try_from(combined_meta.kind)?,
            combined_meta.unpacked_size as usize,
        );
        Ok(RawObjectData::new(
            &combine_object_delta_data(&base_data, &data),
            unpacked_metadata,
        ))
    } else {
        let metadata = ObjectMetadata::new(ObjectKind::try_from(metadata.kind)?, data.len());
        Ok(RawObjectData::new(&data, metadata))
    }
}

fn read_at_address<R>(
    pack_file: &mut BufReader<R>,
    address: u64,
    file_len: u64,
) -> Result<(PackedObjectMetadata, Vec<u8>), anyhow::Error>
where
    R: Read,
    R: Seek,
{
    let mut meta = get_packed_object_metadata(pack_file, address, file_len)?;
    pack_file.seek(SeekFrom::Start(meta.data_start_address))?;
    let mut decompressor = ZlibDecoder::new(pack_file);
    let mut data = Vec::<u8>::with_capacity(meta.unpacked_size as usize);
    decompressor.read_to_end(&mut data)?;
    let compressed_data_length = decompressor.total_in();
    let packed_size = (meta.data_start_address - address) + compressed_data_length;
    meta.packed_size = Some(packed_size);
    let reusable_file = decompressor.into_inner();
    if let PackedObjectType::OffsetDelta(offset) = meta.kind {
        let (base_meta, base_data) = read_at_address(reusable_file, address - offset, file_len)?;
        Ok((
            meta.combine(&base_meta),
            combine_object_delta_data(&base_data, &data),
        ))
    } else {
        Ok((meta, data))
    }
}

#[cfg(test)]
mod tests {
    use crate::stores::pack_store::helpers::delta_offset_read;

    #[test]
    fn delta_offset_reads_succeeds_for_test_case() {
        let buf = vec![
            0xe3u8, 4, 0x80, 0xff, 0xa, 0x78, 0x9c, 0xeb, 0x56, 0x7d, 0xa4, 0x3c, 0x61, 0xcf, 0xc6,
            0xb3,
        ];
        let idx = 2;
        let expected_value = (32778u64, 5usize);

        let test_output = delta_offset_read(&buf, idx);

        assert_eq!(expected_value, test_output);
    }
}
