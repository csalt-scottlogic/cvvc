use std::{
    fs::OpenOptions,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::anyhow;
use sha1::{Digest, Sha1};

use super::helpers;

#[derive(Ord, PartialEq, PartialOrd, Eq)]
struct PackIndexEntry {
    object_id: String,
    pack_offset: u64,
    packed_length: u64,
    pack_order: u32,
    index_order: u32,
    crc: u32,
}

impl PackIndexEntry {
    fn from_reader<R>(
        file: &mut BufReader<R>,
        address: u64,
        file_len: u64,
        pack_order: u32,
    ) -> Result<PackIndexEntry, anyhow::Error>
    where
        R: Read,
        R: Seek,
    {
        let (raw_object, packed_length) =
            helpers::read_raw_object_at_address(file, address, file_len)?;

        // let object_metadata = helpers::get_packed_object_metadata(file, idx, file_len)?;
        // file.seek(SeekFrom::Start(object_metadata.data_start_address))?;
        // let mut decompressor = ZlibDecoder::new(file);
        // let mut data = Vec::<u8>::with_capacity(object_metadata.unpacked_size as usize);
        // decompressor.read_to_end(&mut data)?;
        // let data_start_address = object_metadata.data_start_address;
        // let packed_length = (data_start_address - idx) + decompressor.total_in();
        // let mut file = decompressor.into_inner();
        // let raw_object = helpers::construct_raw_object_from_packed(
        //     object_metadata,
        //     data,
        //     &mut file,
        //     idx,
        //     file_len,
        // )?;

        let mut buf = vec![0u8; packed_length as usize];
        file.seek(SeekFrom::Start(address))?;
        file.read_exact(&mut buf)?;
        let crc = crc32fast::hash(&buf);

        Ok(PackIndexEntry {
            object_id: raw_object.object_id().to_string(),
            pack_offset: address,
            packed_length,
            pack_order,
            crc,
            index_order: 0,
        })
    }

    // this fn assumes the input is sorted
    fn bucketify(entries: &[PackIndexEntry]) -> [u32; 256] {
        let mut buckets = [0u32; 256];
        let mut entry_count = 0;
        for entry in entries {
            entry_count += 1;
            let bucket = usize::from_str_radix(&entry.object_id[..2], 16).unwrap();
            buckets[bucket] = entry_count;
        }
        let mut last_val = 0;
        for i in 0..256 {
            if buckets[i] < last_val {
                buckets[i] = last_val;
            } else {
                last_val = buckets[i];
            }
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
    for i in 0..index_entries.len() {
        index_entries[i].index_order = i as u32;
    }
    let mut pack_checksum = [0u8; 20];
    primary_file.seek(SeekFrom::Start(idx))?;
    primary_file.read_exact(&mut pack_checksum)?;
    write_out_index(&base_path, pack_name, &index_entries, &pack_checksum)?;
    write_out_rev_index(&base_path, pack_name, &index_entries, &pack_checksum)?;
    Ok(())
}

fn write_out_index<P: AsRef<Path>>(
    base_path: P,
    pack_name: &str,
    entries: &[PackIndexEntry],
    pack_checksum: &[u8],
) -> Result<(), anyhow::Error> {
    let index_file_path = helpers::index_file_name(base_path, pack_name);
    if index_file_path.exists() {
        println!("Index file path already exists; not overwriting");
        return Ok(());
    }
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(index_file_path)?;
    let mut writer = BufWriter::new(file);
    let mut hasher = Sha1::new();
    write_index_header(&mut writer, &mut hasher)?;
    write_buckets(&mut writer, &mut hasher, &entries)?;
    write_object_ids(&mut writer, &mut hasher, &entries)?;
    write_crcs(&mut writer, &mut hasher, &entries)?;
    write_offsets(&mut writer, &mut hasher, &entries)?;
    write_conclusion(&mut writer, hasher, pack_checksum)?;
    writer.flush()?;
    Ok(())
}

fn write_conclusion<R, H>(
    writer: &mut BufWriter<R>,
    hasher: H,
    pack_checksum: &[u8],
) -> Result<(), anyhow::Error>
where
    R: Write,
    H: Digest,
{
    let mut hasher = hasher;
    hasher.update(pack_checksum);
    writer.write_all(pack_checksum)?;
    let checksum = hasher.finalize();
    writer.write_all(&checksum)?;
    Ok(())
}

fn write_index_header<R, H>(writer: &mut BufWriter<R>, hasher: &mut H) -> Result<(), anyhow::Error>
where
    R: Write,
    H: Digest,
{
    hasher.update(&helpers::INDEX_HEADER);
    writer.write_all(&helpers::INDEX_HEADER)?;
    Ok(())
}

fn write_buckets<R, H>(
    writer: &mut BufWriter<R>,
    hasher: &mut H,
    entries: &[PackIndexEntry],
) -> Result<(), anyhow::Error>
where
    R: Write,
    H: Digest,
{
    let buckets = PackIndexEntry::bucketify(entries);
    let buckets = buckets.map(|v| u32::to_be_bytes(v));
    hasher.update(buckets.as_flattened());
    writer.write_all(buckets.as_flattened())?;
    Ok(())
}

fn write_object_ids<R, H>(
    writer: &mut BufWriter<R>,
    hasher: &mut H,
    entries: &[PackIndexEntry],
) -> Result<(), anyhow::Error>
where
    R: Write,
    H: Digest,
{
    for e in entries {
        let obj_id_bytes = hex::decode(&e.object_id)?;
        hasher.update(&obj_id_bytes);
        writer.write_all(&obj_id_bytes)?;
    }
    Ok(())
}

fn write_crcs<R, H>(
    writer: &mut BufWriter<R>,
    hasher: &mut H,
    entries: &[PackIndexEntry],
) -> Result<(), anyhow::Error>
where
    R: Write,
    H: Digest,
{
    for e in entries {
        let buf = e.crc.to_be_bytes();
        hasher.update(&buf);
        writer.write_all(&buf)?;
    }
    Ok(())
}

fn write_offsets<R, H>(
    writer: &mut BufWriter<R>,
    hasher: &mut H,
    entries: &[PackIndexEntry],
) -> Result<(), anyhow::Error>
where
    R: Write,
    H: Digest,
{
    let mut large_offset_count = 0u32;
    let mut large_offsets = Vec::<u64>::new();
    for e in entries {
        let value_to_write = if e.pack_offset >= 0x80000000 {
            large_offsets.push(e.pack_offset);
            large_offset_count += 1;
            large_offset_count | 0x80000000
        } else {
            e.pack_offset as u32
        };
        let buf = value_to_write.to_be_bytes();
        hasher.update(&buf);
        writer.write_all(&buf)?;
    }
    for loff in large_offsets {
        let buf = loff.to_be_bytes();
        hasher.update(&buf);
        writer.write_all(&buf)?;
    }
    Ok(())
}

fn write_out_rev_index<P: AsRef<Path>>(
    base_path: P,
    pack_name: &str,
    entries: &[PackIndexEntry],
    pack_checksum: &[u8],
) -> Result<(), anyhow::Error> {
    let rev_file_path = helpers::rev_index_file_name(base_path, pack_name);
    if rev_file_path.exists() {
        println!("Reverse index file already exists; not overwriting");
        return Ok(());
    }
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(rev_file_path)?;
    let mut writer = BufWriter::new(file);
    let mut hasher = Sha1::new();
    write_rev_index_header(&mut writer, &mut hasher)?;
    write_rev_index_entries(&mut writer, &mut hasher, &entries)?;
    write_conclusion(&mut writer, hasher, pack_checksum)?;
    writer.flush()?;
    Ok(())
}

fn write_rev_index_header<R, H>(
    writer: &mut BufWriter<R>,
    hasher: &mut H,
) -> Result<(), anyhow::Error>
where
    R: Write,
    H: Digest,
{
    hasher.update(&helpers::REV_INDEX_HEADER);
    writer.write_all(&helpers::REV_INDEX_HEADER)?;
    Ok(())
}

fn write_rev_index_entries<R, H>(
    writer: &mut BufWriter<R>,
    hasher: &mut H,
    entries: &[PackIndexEntry],
) -> Result<(), anyhow::Error>
where
    R: Write,
    H: Digest,
{
    let mut resorted_entries = entries.iter().collect::<Vec<_>>();
    resorted_entries.sort_by(|a, b| a.pack_order.cmp(&b.pack_order));
    for e in resorted_entries {
        let idx_data = e.index_order.to_be_bytes();
        hasher.update(&idx_data);
        writer.write_all(&idx_data)?;
    }
    Ok(())
}
