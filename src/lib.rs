pub mod cli;
pub mod differ;
pub mod mdparser;
pub mod uploader;

use anyhow::{Context, Result};
use log::debug;
use md5::Digest;
use std::io::Read;
use std::path::Path;
use walkdir::WalkDir;

/// Read the file list under the specified path
/// # Arguments
/// - `path` - The path to read
/// - `depth` - Recursion depth
/// # Return
/// - `Result<Vec<walkdir::DirEntry>, std::io::Error>` - Returns the file list or an error
pub fn read_file_list<P: AsRef<Path> + std::fmt::Debug>(
    path: &P,
    depth: &usize,
) -> Result<Vec<walkdir::DirEntry>> {
    // Create file list
    let mut file_list: Vec<walkdir::DirEntry> = Vec::new();
    // Traverse directory
    debug!("traversing directory: {:?}", path);
    for item in WalkDir::new(path).max_depth(*depth) {
        file_list.push(item?);
    }
    Ok(file_list)
}

/// Computes the MD5 checksum of a file.
///
/// The file is hashed in streaming fashion with a fixed-size buffer, so large
/// images never need to fit into memory, and any read error is propagated to
/// the caller instead of silently producing the checksum of empty content.
///
/// # Arguments
/// - `path` - Path of the file to hash.
///
/// # Return
/// - 32-character lowercase hexadecimal MD5 string.
pub fn get_file_md5<P: AsRef<Path>>(path: P) -> Result<String> {
    let path = path.as_ref();
    let mut file =
        std::fs::File::open(path).with_context(|| format!("Failed to open {:?}", path))?;

    let mut hasher = md5::Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("Failed to read {:?}", path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Maps a local image path to its cloud path.
///
/// Images inside the markdown source tree keep their relative structure
/// (e.g. `posts/img/a.png` -> `posts/img/a.png`). Images referenced from
/// outside the tree (absolute paths or symlinked locations) fall back to a
/// flat cloud path built from the file name, so they are still uploaded
/// instead of crashing the run.
///
/// # Arguments
/// - `local` - Absolute local path of the image.
/// - `src_root` - Canonicalized markdown source root.
///
/// # Return
/// - Cloud path relative to the remote root, without a leading slash.
pub fn to_cloud_path(local: &Path, src_root: &Path) -> String {
    if let Ok(rel) = local.strip_prefix(src_root) {
        let rel = rel.to_string_lossy();
        return rel.trim_start_matches('/').to_string();
    }

    // Outside the source tree: fall back to the bare file name.
    local
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| local.to_string_lossy().to_string())
}

/// Normalizes a cloud path reported by a storage service.
///
/// Some services (e.g. S3) list entries with a leading slash while others
/// (e.g. the memory backend) do not; diffing requires one canonical form.
pub fn normalize_cloud_path(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

/// Normalizes the MD5 checksum reported by a storage service.
///
/// S3-compatible services expose the MD5 through the object ETag. Two quirks
/// must be handled:
/// - The value is usually quoted (`"d41d8..."`).
/// - Objects uploaded via multipart upload have an ETag of the form
///   `d41d8...-7`, which is NOT the content MD5. Such values are reported as
///   `None` so callers treat the checksum as unknown instead of endlessly
///   replacing unchanged files.
pub fn remote_md5(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"');
    if trimmed.is_empty() || trimmed.contains('-') {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_file_md5_known_values() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello").unwrap();
        assert_eq!(
            get_file_md5(&file).unwrap(),
            "5d41402abc4b2a76b9719d911017c592"
        );

        let empty = dir.path().join("empty.bin");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            get_file_md5(&empty).unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
    }

    #[test]
    fn get_file_md5_streams_large_file_consistently() {
        // Larger than the internal read buffer to exercise the streaming loop.
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.bin");
        std::fs::write(&file, &data).unwrap();

        let mut reference = md5::Md5::new();
        md5::Digest::update(&mut reference, &data);

        assert_eq!(
            get_file_md5(&file).unwrap(),
            format!("{:x}", reference.finalize())
        );
    }

    #[test]
    fn get_file_md5_propagates_errors() {
        // Regression: read errors must not silently hash empty content.
        assert!(get_file_md5("/nonexistent/no-such-file.png").is_err());

        let dir = tempfile::tempdir().unwrap();
        assert!(get_file_md5(dir.path()).is_err());
    }

    #[test]
    fn to_cloud_path_keeps_relative_structure() {
        assert_eq!(
            to_cloud_path(Path::new("/blog/posts/img/a.png"), Path::new("/blog")),
            "posts/img/a.png"
        );
    }

    #[test]
    fn to_cloud_path_falls_back_to_filename_outside_tree() {
        // Regression: images outside the source tree used to panic on
        // strip_prefix; they must map to a flat cloud path instead.
        assert_eq!(
            to_cloud_path(Path::new("/pics/logo.png"), Path::new("/blog")),
            "logo.png"
        );
    }

    #[test]
    fn to_cloud_path_accepts_root_equal_path() {
        // A file can never equal the source root, but the degenerate case
        // should not panic and should normalize to the empty cloud path.
        assert_eq!(to_cloud_path(Path::new("/blog"), Path::new("/blog")), "");
    }

    #[test]
    fn normalize_cloud_path_strips_leading_slash() {
        assert_eq!(normalize_cloud_path("/sub/pic.png"), "sub/pic.png");
        assert_eq!(normalize_cloud_path("sub/pic.png"), "sub/pic.png");
        assert_eq!(normalize_cloud_path("/"), "");
    }

    #[test]
    fn remote_md5_normalizes_quoted_and_case() {
        assert_eq!(remote_md5("\"ABC123\""), Some("abc123".to_string()));
        assert_eq!(remote_md5("ABC123"), Some("abc123".to_string()));
    }

    #[test]
    fn remote_md5_rejects_multipart_etags_and_empty() {
        // Multipart ETags (d41d8...-7) are not content MD5s.
        assert_eq!(remote_md5("d41d8cd98f00b204e9800998ecf8427e-7"), None);
        assert_eq!(remote_md5(""), None);
        assert_eq!(remote_md5("\"\""), None);
    }
}
