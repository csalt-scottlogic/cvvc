use anyhow::anyhow;
use std::{
    fs,
    io::{stdout, Write},
    path::Path,
};

use crate::shared::{object_write, repo_find, Blob, Repository};

pub fn cat_file(obj_type: &str, obj_name: &str) -> Result<(), anyhow::Error> {
    let repo = repo_find(Path::new("."))?;
    match repo {
        Some(repo) => cat_file_from_repo(repo, obj_type, obj_name),
        None => Ok(()),
    }
}

fn cat_file_from_repo(
    repo: Repository,
    _obj_type: &str,
    obj_name: &str,
) -> Result<(), anyhow::Error> {
    let obj = repo.object_read(repo.find_object(obj_name))?;
    if obj.is_some() {
        let mut buf = Vec::<u8>::new();
        obj.unwrap().serialise(&mut buf);
        stdout().write_all(&buf)?;
    }
    Ok(())
}

pub fn object_hash(write: bool, obj_type: &str, filename: &str) -> Result<(), anyhow::Error> {
    let repo: Option<Repository>;
    if write {
        repo = repo_find(Path::new("."))?;
    } else {
        repo = None
    }

    let mut file = fs::File::open(filename)?;

    let sha = object_hash_file(&mut file, obj_type, repo.as_ref())?;
    if sha.is_some() {
        println!("{}", sha.unwrap());
    }
    Ok(())
}

fn object_hash_file(
    file: &mut fs::File,
    obj_type: &str,
    repo: Option<&Repository>,
) -> Result<Option<String>, anyhow::Error> {
    match obj_type {
        "blob" => object_write(&Blob::new_from_read(file)?, repo),
        _ => Err(anyhow!("Unknown object type {obj_type}")),
    }
}
