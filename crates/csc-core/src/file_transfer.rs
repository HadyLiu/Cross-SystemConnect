//! Chunked file transfer with xxHash64.
#![allow(dead_code)] // API ready; wire to data-channel sessions incrementally


use crate::path_safe::sanitize_relative_path;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use xxhash_rust::xxh64::Xxh64;

pub const CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRequest {
    pub transfer_id: u64,
    pub file_index: u32,
    pub offset: u64,
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileData {
    pub transfer_id: u64,
    pub file_index: u32,
    pub offset: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEnd {
    pub transfer_id: u64,
    pub file_index: u32,
    pub xxhash64: u64,
}

pub struct FileTransfer {
    next_id: u64,
    receive_root: PathBuf,
    active: HashMap<u64, ActiveRecv>,
}

struct ActiveRecv {
    path: PathBuf,
    file: File,
    hasher: Xxh64,
    expected_hash: Option<u64>,
}

impl FileTransfer {
    pub fn new(receive_root: PathBuf) -> Self {
        Self {
            next_id: 1,
            receive_root,
            active: HashMap::new(),
        }
    }

    pub fn set_receive_root(&mut self, root: PathBuf) {
        self.receive_root = root;
    }

    pub fn begin_send_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn read_chunk(path: &std::path::Path, offset: u64) -> anyhow::Result<(Vec<u8>, bool)> {
        let mut f = File::open(path)?;
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; CHUNK_SIZE];
        let n = f.read(&mut buf)?;
        buf.truncate(n);
        let eof = n < CHUNK_SIZE;
        Ok((buf, eof))
    }

    pub fn hash_file(path: &std::path::Path) -> anyhow::Result<u64> {
        let mut f = File::open(path)?;
        let mut hasher = Xxh64::new(0);
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher.digest())
    }

    pub fn begin_receive(
        &mut self,
        transfer_id: u64,
        relative: &str,
    ) -> anyhow::Result<()> {
        let rel = sanitize_relative_path(relative)
            .ok_or_else(|| anyhow::anyhow!("unsafe path: {relative}"))?;
        let path = self.receive_root.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = File::create(&path)?;
        self.active.insert(
            transfer_id,
            ActiveRecv {
                path,
                file,
                hasher: Xxh64::new(0),
                expected_hash: None,
            },
        );
        Ok(())
    }

    pub fn write_chunk(&mut self, transfer_id: u64, data: &[u8]) -> anyhow::Result<()> {
        let a = self
            .active
            .get_mut(&transfer_id)
            .ok_or_else(|| anyhow::anyhow!("unknown transfer"))?;
        a.file.write_all(data)?;
        a.hasher.update(data);
        Ok(())
    }

    pub fn finish(&mut self, transfer_id: u64, expected: u64) -> anyhow::Result<PathBuf> {
        let mut a = self
            .active
            .remove(&transfer_id)
            .ok_or_else(|| anyhow::anyhow!("unknown transfer"))?;
        a.file.flush()?;
        let got = a.hasher.digest();
        if got != expected {
            let _ = fs::remove_file(&a.path);
            anyhow::bail!("hash mismatch: got {got:#x} expected {expected:#x}");
        }
        Ok(a.path)
    }
}
