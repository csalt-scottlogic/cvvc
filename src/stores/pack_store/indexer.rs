use std::{
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::anyhow;
use flate2::bufread::ZlibDecoder;

use super::helpers;

#[derive(Ord, PartialEq, PartialOrd, Eq)]
struct PackIndexEntry {
    object_id: String,
    pack_offset: u64,
    packed_length: u64,
    pack_order: u32,
    crc: u32,
}

impl PackIndexEntry {
    fn from_reader<R>(
        file: &mut BufReader<R>,
        idx: u64,
        file_len: u64,
        pack_order: u32
    ) -> Result<PackIndexEntry, anyhow::Error>
    where
        R: Read,
        R: Seek,
    {
        let object_metadata = helpers::get_packed_object_metadata(file, idx, file_len)?;
        file.seek(SeekFrom::Start(object_metadata.data_start_address))?;
        let mut decompressor = ZlibDecoder::new(file);
        let mut data = Vec::<u8>::with_capacity(object_metadata.size as usize);
        decompressor.read_to_end(&mut data)?;
        let data_start_address = object_metadata.data_start_address;
        let packed_length = (data_start_address - idx) + decompressor.total_in();
        let mut file = decompressor.into_inner();
        let raw_object = helpers::construct_raw_object_from_packed(object_metadata, data, &mut file, idx, file_len)?;
        let mut buf = vec![0u8; packed_length as usize];
        file.seek(SeekFrom::Start(data_start_address))?;
        file.read_exact(&mut buf)?;
        let crc = crc32fast::hash(&buf);

        Ok(PackIndexEntry { object_id: raw_object.object_id().to_string(), pack_offset: idx, packed_length, pack_order, crc })
    }

    // this fn assumes the input is sorted
    fn bucketify(entries: &[PackIndexEntry]) -> [u32; 256] {
        let mut buckets = [0u32; 256];
        let mut idx = 0;
        let mut last_bucket = 0;
        for entry in entries {
            idx += 1;
            let bucket = usize::from_str_radix(&entry.object_id[..2], 16).unwrap();
            buckets[bucket] = idx;
            last_bucket = bucket;
        }
        for i in (last_bucket + 1)..256 {
            buckets[i] = idx;
        }
        buckets
    }
}

pub fn index<P: AsRef<Path>>(base_path: P, pack_name: &str) -> Result<(), anyhow::Error> {
    let primary_file_path = helpers::primary_file_name(&base_path, pack_name);
    let primary_file_len = primary_file_path.metadata()?.len();
    let mut primary_file = helpers::open_file_from_path(primary_file_path)?;
    if !helpers::check_pack_version(&mut primary_file, None)? {
        return Err(anyhow!("unrecognised pack header or version"));
    }
    let item_count = helpers::read_u32_at(&mut primary_file, 8)?;
    let mut idx = 12;
    let mut index_entries: Vec<PackIndexEntry> = Vec::with_capacity(item_count as usize);
    for i in 0..item_count {
        primary_file.seek(SeekFrom::Start(idx))?;
        let next_entry = PackIndexEntry::from_reader(&mut primary_file, idx, primary_file_len, i)?;
        idx += next_entry.packed_length;
        index_entries.push(next_entry);
    }
    index_entries.sort();
    write_out_index(&base_path, pack_name, &index_entries)?;
    write_out_rev_index(&base_path, pack_name, &index_entries)?;
    Ok(())
}

fn write_out_index<P: AsRef<Path>>(
    base_path: P,
    pack_name: &str,
    entries: &[PackIndexEntry],
) -> Result<(), anyhow::Error> {
    Ok(())
}

fn write_out_rev_index<P: AsRef<Path>>(
    base_path: P,
    pack_name: &str,
    entries: &[PackIndexEntry],
) -> Result<(), anyhow::Error> {
    Ok(())
}
