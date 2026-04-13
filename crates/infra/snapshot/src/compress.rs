//! Tar + zstd + blake3 streaming compression for snapshot archives.

use std::{
    io::{self, Write},
    path::Path,
};

use eyre::{Result, WrapErr};
use tracing::debug;

/// Output from compressing a single archive.
#[derive(Debug)]
pub struct CompressedArchive {
    /// The compressed bytes ready for upload.
    pub data: Vec<u8>,
    /// Blake3 hash of the compressed archive.
    pub blake3_hash: String,
    /// Original uncompressed size in bytes.
    pub uncompressed_size: u64,
}

/// Compresses directories and files into tar.zst archives with blake3 checksums.
#[derive(Debug)]
pub struct Compressor {
    zstd_level: i32,
}

impl Compressor {
    /// Creates a new compressor with the given zstd compression level.
    pub const fn new(zstd_level: i32) -> Self {
        Self { zstd_level }
    }

    /// Compress a directory into a tar.zst archive.
    ///
    /// All files in the directory are added to the tar at the root level,
    /// preserving only the filename (not the full path).
    pub fn compress_directory(&self, dir: &Path, archive_name: &str) -> Result<CompressedArchive> {
        let mut uncompressed_size = 0u64;
        let buf = Vec::new();
        let mut hasher = HashingWriter::new(buf);

        {
            let zstd_encoder = zstd::Encoder::new(&mut hasher, self.zstd_level)
                .wrap_err("failed to create zstd encoder")?;
            let mut tar_builder = tar::Builder::new(zstd_encoder);

            Self::append_dir_contents(&mut tar_builder, dir, &mut uncompressed_size)?;

            let zstd_encoder = tar_builder.into_inner().wrap_err("failed to finish tar")?;
            zstd_encoder.finish().wrap_err("failed to finish zstd")?;
        }

        let blake3_hash = hasher.finalize_hex();
        let data = hasher.into_inner();

        debug!(
            archive = archive_name,
            compressed_size = data.len(),
            uncompressed_size,
            blake3 = %blake3_hash,
            "compressed archive",
        );

        Ok(CompressedArchive { data, blake3_hash, uncompressed_size })
    }

    /// Compress a list of files into a tar.zst archive.
    ///
    /// Files are added using only their filename, not the full path.
    pub fn compress_files(
        &self,
        files: &[impl AsRef<Path>],
        archive_name: &str,
    ) -> Result<CompressedArchive> {
        let mut uncompressed_size = 0u64;
        let buf = Vec::new();
        let mut hasher = HashingWriter::new(buf);

        {
            let zstd_encoder = zstd::Encoder::new(&mut hasher, self.zstd_level)
                .wrap_err("failed to create zstd encoder")?;
            let mut tar_builder = tar::Builder::new(zstd_encoder);

            for file_path in files {
                let file_path = file_path.as_ref();
                let filename = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");

                let metadata = std::fs::metadata(file_path).wrap_err_with(|| {
                    format!("failed to read metadata for {}", file_path.display())
                })?;
                uncompressed_size += metadata.len();

                tar_builder
                    .append_path_with_name(file_path, filename)
                    .wrap_err_with(|| format!("failed to add {} to tar", file_path.display()))?;
            }

            let zstd_encoder = tar_builder.into_inner().wrap_err("failed to finish tar")?;
            zstd_encoder.finish().wrap_err("failed to finish zstd")?;
        }

        let blake3_hash = hasher.finalize_hex();
        let data = hasher.into_inner();

        debug!(
            archive = archive_name,
            compressed_size = data.len(),
            uncompressed_size,
            blake3 = %blake3_hash,
            "compressed archive",
        );

        Ok(CompressedArchive { data, blake3_hash, uncompressed_size })
    }

    fn append_dir_contents(
        tar_builder: &mut tar::Builder<impl Write>,
        dir: &Path,
        uncompressed_size: &mut u64,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();

            if path.is_dir() {
                tar_builder
                    .append_dir_all(&name, &path)
                    .wrap_err_with(|| format!("failed to add dir {} to tar", path.display()))?;
                *uncompressed_size += dir_size_recursive(&path)?;
            } else {
                let metadata = std::fs::metadata(&path)?;
                *uncompressed_size += metadata.len();
                tar_builder
                    .append_path_with_name(&path, &name)
                    .wrap_err_with(|| format!("failed to add {} to tar", path.display()))?;
            }
        }
        Ok(())
    }
}

fn dir_size_recursive(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            total += dir_size_recursive(&path)?;
        } else {
            total += std::fs::metadata(&path)?.len();
        }
    }
    Ok(total)
}

struct HashingWriter<W> {
    inner: W,
    hasher: blake3::Hasher,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner, hasher: blake3::Hasher::new() }
    }

    fn finalize_hex(&self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_and_verify_blake3() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.dat"), b"hello world").unwrap();

        let compressor = Compressor::new(1);
        let result = compressor.compress_directory(dir.path(), "test.tar.zst").unwrap();

        assert!(!result.data.is_empty());
        assert!(!result.blake3_hash.is_empty());
        assert_eq!(result.uncompressed_size, 11);

        let expected_hash = blake3::hash(&result.data).to_hex().to_string();
        assert_eq!(result.blake3_hash, expected_hash);
    }

    #[test]
    fn compress_files_individually() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.dat");
        let f2 = dir.path().join("b.dat");
        std::fs::write(&f1, b"file_a").unwrap();
        std::fs::write(&f2, b"file_b").unwrap();

        let compressor = Compressor::new(1);
        let result = compressor.compress_files(&[&f1, &f2], "multi.tar.zst").unwrap();

        assert!(!result.data.is_empty());
        assert_eq!(result.uncompressed_size, 12);
    }

    #[test]
    fn roundtrip_decompress() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), b"roundtrip test data").unwrap();

        let compressor = Compressor::new(3);
        let result = compressor.compress_directory(dir.path(), "rt.tar.zst").unwrap();

        let decoder = zstd::Decoder::new(result.data.as_slice()).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let extract_dir = tempfile::tempdir().unwrap();
        archive.unpack(extract_dir.path()).unwrap();

        let content = std::fs::read_to_string(extract_dir.path().join("hello.txt")).unwrap();
        assert_eq!(content, "roundtrip test data");
    }
}
