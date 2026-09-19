pub mod cli;
pub mod differ;
pub mod mdparser;
pub mod pipeline;
pub mod uploader;

use anyhow::{Context, Result};
use md5::Digest;
use std::fmt;
use std::io::Read;
use std::ops::Deref;
use std::path::Path;

/// A normalized cloud object path (no leading slash), as constructed from a
/// local image or a cloud listing.
///
/// Every [`CloudPath`] is canonicalized by [`CloudPath::new`], which trims any
/// leading slashes. Storage services disagree on whether listed paths start
/// with `/` (S3 does, the memory backend does not), and locally derived paths
/// used to be normalized by call-site discipline only; the newtype moves that
/// invariant to the type system so downstream code (diff, transfers, URL
/// building) can never observe a non-normalized path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CloudPath(String);

impl CloudPath {
    /// Creates a cloud path, trimming any leading slashes so listings and
    /// locally derived paths share one canonical form.
    ///
    /// ```
    /// use mdluploader::CloudPath;
    ///
    /// assert_eq!(CloudPath::new("/sub/a.png").as_str(), "sub/a.png");
    /// assert_eq!(CloudPath::new("sub/a.png").as_str(), "sub/a.png");
    /// ```
    pub fn new(path: impl Into<String>) -> Self {
        let path = path.into();
        Self(path.trim_start_matches('/').to_string())
    }

    /// Views the canonical path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for CloudPath {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for CloudPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Default concurrency for parallel cloud transfers.
///
/// CPU count * 2 (transfers are I/O-bound), clamped to [4, 32].
pub fn default_concurrency() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get() * 2)
        .unwrap_or(8)
        .clamp(4, 32)
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

/// Guesses the Content-Type of a file from its extension.
///
/// Covers the image formats an image bed typically serves plus a few
/// non-image formats found next to them in markdown trees. Extensions
/// are matched case-insensitively; unknown or missing extensions fall
/// back to `application/octet-stream`, matching what S3 stores when no
/// Content-Type is sent along with the upload.
///
/// # Arguments
/// - `path` - Path of the file to inspect.
///
/// # Return
/// - The MIME type as a static string.
pub fn guess_content_type<P: AsRef<Path>>(path: P) -> &'static str {
    let ext = path
        .as_ref()
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "json" => "application/json",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
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
/// - [`CloudPath`] relative to the remote root, normalized at construction.
pub fn to_cloud_path(local: &Path, src_root: &Path) -> CloudPath {
    if let Ok(rel) = local.strip_prefix(src_root) {
        return CloudPath::new(rel.to_string_lossy());
    }

    // Outside the source tree: fall back to the bare file name.
    let fallback = local
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| local.to_string_lossy().to_string());
    CloudPath::new(fallback)
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
            to_cloud_path(Path::new("/blog/posts/img/a.png"), Path::new("/blog")).as_str(),
            "posts/img/a.png"
        );
    }

    #[test]
    fn to_cloud_path_falls_back_to_filename_outside_tree() {
        // Regression: images outside the source tree used to panic on
        // strip_prefix; they must map to a flat cloud path instead.
        assert_eq!(
            to_cloud_path(Path::new("/pics/logo.png"), Path::new("/blog")).as_str(),
            "logo.png"
        );
    }

    #[test]
    fn to_cloud_path_accepts_root_equal_path() {
        // A file can never equal the source root, but the degenerate case
        // should not panic and should normalize to the empty cloud path.
        assert_eq!(
            to_cloud_path(Path::new("/blog"), Path::new("/blog")).as_str(),
            ""
        );
    }

    #[test]
    fn cloud_path_trims_leading_slashes() {
        // S3-style listings report a leading slash; the memory backend does
        // not. Both forms must canonicalize to the same path.
        assert_eq!(CloudPath::new("/sub/pic.png").as_str(), "sub/pic.png");
        assert_eq!(CloudPath::new("sub/pic.png").as_str(), "sub/pic.png");
        assert_eq!(CloudPath::new("//sub/pic.png").as_str(), "sub/pic.png");
        assert_eq!(CloudPath::new("///").as_str(), "");
    }

    #[test]
    fn cloud_path_accepts_and_keeps_empty_path() {
        assert_eq!(CloudPath::new("").as_str(), "");
        assert_eq!(CloudPath::new("/").as_str(), "");
    }

    #[test]
    fn cloud_path_compares_and_displays_as_its_string() {
        // Equality and ordering follow the canonical string, and Display
        // renders it directly so log lines stay unchanged.
        assert_eq!(CloudPath::new("a.png"), CloudPath::new("/a.png"));
        assert!(CloudPath::new("a.png") < CloudPath::new("b.png"));
        assert_eq!(CloudPath::new("a.png").to_string(), "a.png");
    }

    #[test]
    fn guess_content_type_maps_common_image_formats() {
        assert_eq!(guess_content_type(Path::new("a.png")), "image/png");
        assert_eq!(guess_content_type(Path::new("a.jpg")), "image/jpeg");
        assert_eq!(guess_content_type(Path::new("a.jpeg")), "image/jpeg");
        assert_eq!(guess_content_type(Path::new("a.gif")), "image/gif");
        assert_eq!(guess_content_type(Path::new("a.webp")), "image/webp");
        assert_eq!(guess_content_type(Path::new("a.svg")), "image/svg+xml");
        assert_eq!(guess_content_type(Path::new("a.avif")), "image/avif");
        assert_eq!(guess_content_type(Path::new("a.bmp")), "image/bmp");
        assert_eq!(guess_content_type(Path::new("a.ico")), "image/x-icon");
    }

    #[test]
    fn guess_content_type_maps_a_few_non_image_formats() {
        assert_eq!(guess_content_type(Path::new("doc.pdf")), "application/pdf");
        assert_eq!(guess_content_type(Path::new("notes.txt")), "text/plain");
        assert_eq!(
            guess_content_type(Path::new("data.json")),
            "application/json"
        );
        assert_eq!(guess_content_type(Path::new("clip.mp4")), "video/mp4");
    }

    #[test]
    fn guess_content_type_is_case_insensitive() {
        assert_eq!(guess_content_type(Path::new("PIC.PNG")), "image/png");
        assert_eq!(guess_content_type(Path::new("pic.Jpg")), "image/jpeg");
        assert_eq!(guess_content_type(Path::new("sub/pic.WebP")), "image/webp");
    }

    #[test]
    fn guess_content_type_falls_back_to_octet_stream() {
        // Unknown extension.
        assert_eq!(
            guess_content_type(Path::new("archive.xyz")),
            "application/octet-stream"
        );
        // No extension at all.
        assert_eq!(
            guess_content_type(Path::new("README")),
            "application/octet-stream"
        );
        // Only the last extension segment counts.
        assert_eq!(
            guess_content_type(Path::new("photo.png.exe")),
            "application/octet-stream"
        );
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
