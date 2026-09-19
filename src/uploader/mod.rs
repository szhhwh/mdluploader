pub mod s3;
use anyhow::{bail, Context, Result};
use futures::StreamExt;
use log::debug;
use opendal::Operator;
use std::future::Future;
use std::path::PathBuf;
use tokio::fs;

/// Represents a file to be uploaded to the cloud storage.
#[derive(Debug, Clone)]
pub struct UpFile {
    /// The local file path to be uploaded.
    pub local_path: PathBuf,
    /// The destination path in the cloud storage.
    pub cloud_path: String,
}

impl UpFile {
    /// Creates a new `UpFile` from an explicit local/cloud path pair.
    pub fn new(local_path: PathBuf, cloud_path: String) -> Self {
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
        }
    }

    /// Overrides the concurrency limit used for parallel transfers.
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Calculates a suitable concurrency limit for the current system.
    ///
    /// This method determines the concurrency limit based on the number of CPU
    /// cores, ensuring efficient utilization of system resources for
    /// I/O-bound tasks.
    fn default_concurrency() -> usize {
        // Base concurrency limit: CPU cores * 2 (considering I/O-bound tasks)
        let base_limit = num_cpus::get() * 2;

        // Set a minimum and maximum value to prevent extreme cases
        let min_limit = 4;
        let max_limit = 32;

        base_limit.clamp(min_limit, max_limit)
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

        let jobs = files
            .iter()
            .map(|file| {
                let uploader = self.clone();
                let file = file.clone();
                (file.cloud_path.clone(), async move {
                    uploader.upload_one(&file).await
                })
            })
            .collect();

        let failures = Self::run_limited(jobs, self.concurrency).await;
        Self::report_failures("upload", failures)
    }

    /// Uploads a single file to the cloud storage.
    ///
    /// # Arguments
    ///
    /// * `file` - The `UpFile` describing the local source and cloud destination.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    async fn upload_one(&self, file: &UpFile) -> Result<()> {
        let bytes = fs::read(&file.local_path)
            .await
            .with_context(|| format!("Failed to read local file {}", file.local_path.display()))?;

        debug!(
            "Uploading {} ({} bytes) to cloud path {}",
            file.local_path.display(),
            bytes.len(),
            file.cloud_path
        );

        self.op
            .write(&file.cloud_path, bytes)
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
    /// * `paths` - A vector of cloud paths (strings) to delete.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    pub async fn delete_files(&self, paths: Vec<String>) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        debug!(
            "Deleting {} files with concurrency {}",
            paths.len(),
            self.concurrency
        );

        let jobs = paths
            .iter()
            .map(|path| {
                let uploader = self.clone();
                let path = path.clone();
                (
                    path.clone(),
                    async move { uploader.delete_one(&path).await },
                )
            })
            .collect();

        let failures = Self::run_limited(jobs, self.concurrency).await;
        Self::report_failures("delete", failures)
    }

    /// Deletes a single file from the cloud storage.
    ///
    /// # Arguments
    ///
    /// * `path` - A string slice representing the cloud path.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    async fn delete_one(&self, path: &str) -> Result<()> {
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
    /// * `jobs` - A vector of `(name, future)` pairs; the name identifies the
    ///   job in error reports.
    /// * `limit` - Maximum number of jobs polled concurrently.
    ///
    /// # Returns
    ///
    /// A vector of `(name, error)` pairs for every job that failed.
    async fn run_limited<Fut>(
        jobs: Vec<(String, Fut)>,
        limit: usize,
    ) -> Vec<(String, anyhow::Error)>
    where
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
    fn report_failures(action: &str, failures: Vec<(String, anyhow::Error)>) -> Result<()> {
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
            .upload_files(vec![UpFile::new(img, "pic.png".to_string())])
            .await
            .unwrap();

        let got = op.read("pic.png").await.unwrap().to_vec();
        assert_eq!(got, b"png-bytes-123");
    }

    #[tokio::test]
    async fn upload_files_reports_missing_local_file() {
        let uploader = Uploader::new(memory_op());
        let err = uploader
            .upload_files(vec![UpFile::new(
                PathBuf::from("/nonexistent/x.png"),
                "x.png".to_string(),
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
                UpFile::new(a, "a.png".to_string()),
                UpFile::new(
                    PathBuf::from("/nonexistent/missing.png"),
                    "missing.png".to_string(),
                ),
                UpFile::new(b, "b.png".to_string()),
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
            .delete_files(vec!["a.png".to_string(), "b.png".to_string()])
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
