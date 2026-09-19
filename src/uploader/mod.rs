pub mod s3;
use anyhow::{bail, Context, Result};
use futures::StreamExt;
use log::debug;
use opendal::Operator;
use std::future::Future;
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncReadExt;

use crate::CloudPath;

/// Chunk size used when streaming uploads: bytes read from disk and pushed to
/// the cloud writer per iteration.
///
/// 1 MiB bounds peak memory per concurrent transfer to roughly a megabyte
/// while amortizing the per-request overhead far better than the 64 KiB
/// buffer used for local MD5 hashing (each chunk becomes a network write,
/// not just a hash update).
const UPLOAD_CHUNK_SIZE: usize = 1024 * 1024;

/// Represents a file to be uploaded to the cloud storage.
#[derive(Debug, Clone)]
pub struct UpFile {
    /// The local file path to be uploaded.
    pub local_path: PathBuf,
    /// The destination path in the cloud storage.
    pub cloud_path: CloudPath,
}

impl UpFile {
    /// Creates a new `UpFile` from an explicit local/cloud path pair.
    pub fn new(local_path: PathBuf, cloud_path: CloudPath) -> Self {
        Self {
            local_path,
            cloud_path,
        }
    }
}

/// Represents an uploader that interacts with a cloud storage system.
///
/// This struct provides methods to upload, list, and delete files in the cloud.
#[derive(Clone)]
pub struct Uploader {
    /// The operator instance used to interact with cloud storage.
    op: Operator,
    /// Maximum number of concurrent transfer tasks.
    concurrency: usize,
    /// Cache-Control header set on uploaded objects; empty means unset.
    cache_control: String,
}

impl Uploader {
    /// Creates a new `Uploader` instance.
    ///
    /// # Arguments
    ///
    /// * `op` - An `Operator` instance for cloud storage operations.
    pub fn new(op: Operator) -> Self {
        Self {
            op,
            concurrency: Self::default_concurrency(),
            cache_control: String::new(),
        }
    }

    /// Overrides the concurrency limit used for parallel transfers.
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Overrides the Cache-Control header set on uploaded objects.
    ///
    /// An empty value (the default) leaves the header unset. The CLI
    /// default stays moderate on purpose: images are replaced in place
    /// by cloud path, so an aggressive policy (immutable, huge max-age)
    /// would serve stale copies from CDN caches after a replacement.
    pub fn with_cache_control(mut self, cache_control: String) -> Self {
        self.cache_control = cache_control;
        self
    }

    /// Calculates a suitable concurrency limit for the current system.
    ///
    /// This method determines the concurrency limit based on the number of CPU
    /// cores, ensuring efficient utilization of system resources for
    /// I/O-bound tasks.
    fn default_concurrency() -> usize {
        crate::default_concurrency()
    }

    /// Lists files and directories in the cloud storage.
    ///
    /// # Arguments
    ///
    /// * `path` - A string slice representing the directory path to list.
    /// * `recur` - A boolean indicating whether to recursively list the directory.
    ///
    /// # Returns
    ///
    /// A `Result` containing a vector of `opendal::Entry` on success, or an error on failure.
    pub async fn list_cloud(&self, path: &str, recur: bool) -> Result<Vec<opendal::Entry>> {
        let list = self
            .op
            .list_with(path)
            .recursive(recur)
            .await
            .with_context(|| format!("Failed to list cloud path {}", path))?;
        Ok(list)
    }

    /// Uploads multiple files to the cloud storage.
    ///
    /// Unlike the previous behavior, this method fails the whole operation if
    /// any individual upload fails, so callers never mistake a partial
    /// transfer for a successful sync.
    ///
    /// # Arguments
    ///
    /// * `files` - A vector of `UpFile` representing the files to upload.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    pub async fn upload_files(&self, files: Vec<UpFile>) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        debug!(
            "Uploading {} files with concurrency {}",
            files.len(),
            self.concurrency
        );

        // The files are already owned; move each one into its job and keep a
        // single clone of the name for error reporting.
        let jobs = files.into_iter().map(|file| {
            let uploader = self.clone();
            let name = file.cloud_path.clone();
            (name, async move { uploader.upload_one(&file).await })
        });

        let failures = Self::run_limited(jobs, self.concurrency).await;
        Self::report_failures("upload", failures)
    }

    /// Uploads a single file to the cloud storage.
    ///
    /// The file is streamed in fixed-size chunks instead of being buffered
    /// whole, so peak memory stays bounded by the chunk size no matter how
    /// large the file is (or how many transfers run concurrently). Empty
    /// files skip the loop entirely and commit an empty object on close.
    ///
    /// # Arguments
    ///
    /// * `file` - The `UpFile` describing the local source and cloud destination.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    async fn upload_one(&self, file: &UpFile) -> Result<()> {
        debug!(
            "Uploading {} to cloud path {}",
            file.local_path.display(),
            file.cloud_path
        );

        let mut reader = fs::File::open(&file.local_path)
            .await
            .with_context(|| format!("Failed to read local file {}", file.local_path.display()))?;

        // Without an explicit Content-Type S3 stores
        // application/octet-stream, which makes browsers download images
        // instead of displaying them.
        let writer = self
            .op
            .writer_with(&file.cloud_path)
            .content_type(crate::guess_content_type(&file.local_path));
        let writer = if self.cache_control.is_empty() {
            writer
        } else {
            writer.cache_control(&self.cache_control)
        };
        let mut writer = writer
            .await
            .with_context(|| format!("Failed to write cloud path {}", file.cloud_path))?;

        // `read_buf` pulls straight into the chunk's spare capacity and the
        // chunk is moved into the writer, so each megabyte is neither zeroed
        // nor copied on its way to storage.
        loop {
            let mut chunk = Vec::with_capacity(UPLOAD_CHUNK_SIZE);
            let n = reader.read_buf(&mut chunk).await.with_context(|| {
                format!("Failed to read local file {}", file.local_path.display())
            })?;
            if n == 0 {
                break;
            }
            writer
                .write(chunk)
                .await
                .with_context(|| format!("Failed to write cloud path {}", file.cloud_path))?;
        }

        let _meta = writer
            .close()
            .await
            .with_context(|| format!("Failed to write cloud path {}", file.cloud_path))?;
        Ok(())
    }

    /// Deletes multiple files from the cloud storage.
    ///
    /// Fails the whole operation if any individual deletion fails.
    ///
    /// # Arguments
    ///
    /// * `paths` - A vector of cloud paths to delete.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    pub async fn delete_files(&self, paths: Vec<CloudPath>) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        debug!(
            "Deleting {} files with concurrency {}",
            paths.len(),
            self.concurrency
        );

        let jobs = paths.into_iter().map(|path| {
            let uploader = self.clone();
            let name = path.clone();
            (name, async move { uploader.delete_one(&path).await })
        });

        let failures = Self::run_limited(jobs, self.concurrency).await;
        Self::report_failures("delete", failures)
    }

    /// Deletes a single file from the cloud storage.
    ///
    /// # Arguments
    ///
    /// * `path` - The cloud path of the object to delete.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    async fn delete_one(&self, path: &CloudPath) -> Result<()> {
        debug!("Deleting file from cloud: {}", path);

        self.op
            .delete(path)
            .await
            .with_context(|| format!("Failed to delete cloud path {}", path))?;

        debug!("Successfully deleted file: {}", path);
        Ok(())
    }

    /// Runs a batch of named jobs with bounded concurrency and collects the
    /// failures. Shared by upload and delete so the limiting logic exists once.
    ///
    /// # Arguments
    ///
    /// * `jobs` - An iterator of `(name, future)` pairs; the name identifies
    ///   the job in error reports.
    /// * `limit` - Maximum number of jobs polled concurrently.
    ///
    /// # Returns
    ///
    /// A vector of `(name, error)` pairs for every job that failed.
    async fn run_limited<I, Fut>(jobs: I, limit: usize) -> Vec<(CloudPath, anyhow::Error)>
    where
        I: IntoIterator<Item = (CloudPath, Fut)>,
        Fut: Future<Output = Result<()>>,
    {
        futures::stream::iter(jobs)
            .map(|(name, fut)| async move {
                if let Err(e) = fut.await {
                    Some((name, e))
                } else {
                    None
                }
            })
            .buffer_unordered(limit.max(1))
            .filter_map(|result| async move { result })
            .collect()
            .await
    }

    /// Turns a list of per-file failures into a single aggregate error.
    fn report_failures(action: &str, failures: Vec<(CloudPath, anyhow::Error)>) -> Result<()> {
        if failures.is_empty() {
            return Ok(());
        }

        let mut message = format!("failed to {} {} file(s):", action, failures.len());
        for (name, e) in &failures {
            log::error!("Failed to {} {}: {}", action, name, e);
            message.push_str(&format!("\n  {}: {}", name, e));
        }
        bail!(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services;

    fn memory_op() -> Operator {
        Operator::new(services::Memory::default()).unwrap().finish()
    }

    #[tokio::test]
    async fn upload_files_writes_actual_content() {
        // Regression test: uploads must persist the real file bytes.
        // The old implementation created a writer and dropped it without
        // writing, leaving remote objects empty or missing entirely.
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("pic.png");
        std::fs::write(&img, b"png-bytes-123").unwrap();

        let op = memory_op();
        let uploader = Uploader::new(op.clone());
        uploader
            .upload_files(vec![UpFile::new(img, CloudPath::new("pic.png"))])
            .await
            .unwrap();

        let got = op.read("pic.png").await.unwrap().to_vec();
        assert_eq!(got, b"png-bytes-123");
    }

    #[tokio::test]
    async fn upload_files_sets_content_type_and_cache_control() {
        // The memory backend stores both headers in the object metadata
        // and exposes them through stat, so the round trip is observable.
        // The mixed-case local extension also checks MIME inference at the
        // real call site.
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("pic.PNG");
        std::fs::write(&img, b"png-bytes").unwrap();

        let op = memory_op();
        let uploader =
            Uploader::new(op.clone()).with_cache_control("public, max-age=86400".to_string());
        uploader
            .upload_files(vec![UpFile::new(img, CloudPath::new("pic.png"))])
            .await
            .unwrap();

        let meta = op.stat("pic.png").await.unwrap();
        assert_eq!(meta.content_type(), Some("image/png"));
        assert_eq!(meta.cache_control(), Some("public, max-age=86400"));
    }

    #[tokio::test]
    async fn upload_files_omits_cache_control_when_unset() {
        // Default configuration: content type still set, cache control
        // header left absent.
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("photo.jpeg");
        std::fs::write(&img, b"jpeg-bytes").unwrap();

        let op = memory_op();
        let uploader = Uploader::new(op.clone());
        uploader
            .upload_files(vec![UpFile::new(img, CloudPath::new("photo.jpeg"))])
            .await
            .unwrap();

        let meta = op.stat("photo.jpeg").await.unwrap();
        assert_eq!(meta.content_type(), Some("image/jpeg"));
        assert!(meta.cache_control().is_none());
    }

    #[tokio::test]
    async fn upload_files_streams_multi_chunk_file() {
        // A file larger than one upload chunk must survive the streaming
        // loop byte for byte. 3 MiB of pseudo-random data means three full
        // 1 MiB writes plus a final zero-byte read that ends the loop.
        let mut rng: u32 = 0x1234_5678;
        let data: Vec<u8> = (0..3 * UPLOAD_CHUNK_SIZE)
            .map(|_| {
                rng ^= rng << 13;
                rng ^= rng >> 17;
                rng ^= rng << 5;
                (rng >> 24) as u8
            })
            .collect();

        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("big.bin");
        std::fs::write(&img, &data).unwrap();

        let op = memory_op();
        let uploader = Uploader::new(op.clone());
        uploader
            .upload_files(vec![UpFile::new(img, CloudPath::new("big.bin"))])
            .await
            .unwrap();

        let got = op.read("big.bin").await.unwrap();
        assert_eq!(got.len(), data.len());
        assert_eq!(got.to_vec(), data);
    }

    #[tokio::test]
    async fn upload_files_handles_empty_file() {
        // Zero-byte files must commit an empty object instead of failing or
        // silently skipping the write.
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("empty.png");
        std::fs::write(&img, b"").unwrap();

        let op = memory_op();
        let uploader = Uploader::new(op.clone());
        uploader
            .upload_files(vec![UpFile::new(img, CloudPath::new("empty.png"))])
            .await
            .unwrap();

        let meta = op.stat("empty.png").await.unwrap();
        assert_eq!(meta.content_length(), 0);
    }

    #[tokio::test]
    async fn upload_files_reports_missing_local_file() {
        let uploader = Uploader::new(memory_op());
        let err = uploader
            .upload_files(vec![UpFile::new(
                PathBuf::from("/nonexistent/x.png"),
                CloudPath::new("x.png"),
            )])
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("x.png"),
            "error should mention the file: {}",
            msg
        );
    }

    #[tokio::test]
    async fn upload_files_reports_partial_failure_after_uploading_rest() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        let op = memory_op();
        let uploader = Uploader::new(op.clone());
        let err = uploader
            .upload_files(vec![
                UpFile::new(a, CloudPath::new("a.png")),
                UpFile::new(
                    PathBuf::from("/nonexistent/missing.png"),
                    CloudPath::new("missing.png"),
                ),
                UpFile::new(b, CloudPath::new("b.png")),
            ])
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("1 file(s)"), "unexpected message: {}", msg);
        assert!(msg.contains("missing.png"));

        // Valid files are still uploaded despite the failure.
        assert_eq!(op.read("a.png").await.unwrap().to_vec(), b"a");
        assert_eq!(op.read("b.png").await.unwrap().to_vec(), b"b");
    }

    #[tokio::test]
    async fn delete_files_removes_cloud_objects() {
        let op = memory_op();
        op.write("a.png", b"x".to_vec()).await.unwrap();
        op.write("b.png", b"y".to_vec()).await.unwrap();

        let uploader = Uploader::new(op.clone());
        uploader
            .delete_files(vec![CloudPath::new("a.png"), CloudPath::new("b.png")])
            .await
            .unwrap();

        let entries = uploader.list_cloud("/", true).await.unwrap();
        assert!(entries.is_empty(), "cloud should be empty: {:?}", entries);
    }

    #[tokio::test]
    async fn list_cloud_returns_written_objects() {
        let op = memory_op();
        op.write("sub/pic.png", b"x".to_vec()).await.unwrap();

        let uploader = Uploader::new(op.clone());
        let entries = uploader.list_cloud("/", true).await.unwrap();
        let paths: Vec<String> = entries
            .iter()
            .map(|e| e.path().trim_start_matches('/').to_string())
            .collect();
        assert_eq!(paths, vec!["sub/pic.png".to_string()]);
    }

    #[tokio::test]
    async fn with_concurrency_clamps_to_at_least_one() {
        let uploader = Uploader::new(memory_op()).with_concurrency(0);
        assert_eq!(uploader.concurrency, 1);
    }
}
