use crate::fs_utils::io_err;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// 8 MiB chunks — matches find_duplicates.py's CHUNK_SIZE, big enough to be
/// fast on spinning disks, small enough not to blow up memory on huge files.
const CHUNK_SIZE: usize = 8 * 1024 * 1024;

/// Full-content SHA-256, used whenever we need a "these bytes are identical" guarantee
/// (Find Duplicates' step 2, TriPhotosVideo's dedup).
pub fn sha256_file(path: &Path) -> crate::Result<String> {
    let file = File::open(path).map_err(io_err(path))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = reader.read(&mut buf).map_err(io_err(path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Cheap partial hash (first + last 64 KiB) for the "quick mode" duplicate scan
/// advanced option. Much faster on huge video files, at the cost of a
/// (very small) theoretical false-positive rate that step-2 full hashing on
/// the *confirmed* candidates would still catch if the caller chases up.
pub fn partial_hash_file(path: &Path) -> crate::Result<String> {
    const EDGE: u64 = 64 * 1024;
    let file = File::open(path).map_err(io_err(path))?;
    let len = file.metadata().map_err(io_err(path))?.len();
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();

    let mut head = vec![0u8; EDGE.min(len) as usize];
    reader.read_exact(&mut head).map_err(io_err(path))?;
    hasher.update(&head);

    if len > EDGE * 2 {
        use std::io::{Seek, SeekFrom};
        reader
            .seek(SeekFrom::End(-(EDGE as i64)))
            .map_err(io_err(path))?;
        let mut tail = vec![0u8; EDGE as usize];
        reader.read_exact(&mut tail).map_err(io_err(path))?;
        hasher.update(&tail);
    }
    hasher.update(len.to_le_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn identical_content_same_hash() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        std::fs::File::create(&a).unwrap().write_all(b"hello world").unwrap();
        std::fs::File::create(&b).unwrap().write_all(b"hello world").unwrap();
        assert_eq!(sha256_file(&a).unwrap(), sha256_file(&b).unwrap());
    }

    #[test]
    fn different_content_different_hash() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        std::fs::File::create(&a).unwrap().write_all(b"hello world").unwrap();
        std::fs::File::create(&b).unwrap().write_all(b"hello WORLD").unwrap();
        assert_ne!(sha256_file(&a).unwrap(), sha256_file(&b).unwrap());
    }
}
