use anyhow::{Context, Ok, Result};
use futures::TryStreamExt;
use log::{debug, error, info, trace};
use opendal::Operator;
use std::path::PathBuf;
use tokio::{fs::File, task::JoinSet};

/// Represents a file to be uploaded to the cloud storage.
#[derive(Debug, Clone)]
pub struct UpFile {
    /// The local file path to be uploaded.
    pub local_path: PathBuf,
    /// The destination path in the cloud storage.
    pub cloud_path: String,
}

impl UpFile {
    pub fn from_pathbuf(local_path: &PathBuf, src: &PathBuf) -> Result<Self> {
        let relative_path = local_path.strip_prefix(src).with_context(|| {
            format!(
                "Path {} is not under base path {}",
                local_path.display(),
                src.display()
            )
        })?;

        let cloud_path = relative_path.to_string_lossy().to_string();

        Ok(Self {
            local_path: local_path.clone(),
            cloud_path,
        })
    }
}

/// Represents an uploader that interacts with a cloud storage system.
///
/// This struct provides methods to upload, list, and delete files in the cloud.
#[derive(Clone)]
pub struct Uploader {
    /// The operator instance used to interact with the cloud storage.
    op: Operator,
}

impl Uploader {
    /// Creates a new `Uploader` instance.
    ///
    /// # Arguments
    ///
    /// * `op` - An `Operator` instance for cloud storage operations.
    pub fn new(op: Operator) -> Self {
        Self { op }
    }

    /// Calculates a suitable concurrency limit for the current system.
    ///
    /// This method determines the concurrency limit based on the number of CPU cores,
    /// ensuring efficient utilization of system resources for I/O-bound tasks.
    ///
    /// # Returns
    ///
    /// A `usize` value representing the concurrency limit.
    fn get_concurrency_limit() -> usize {
        // Get the number of CPU cores
        let cpu_count = num_cpus::get();

        // Base concurrency limit: CPU cores * 2 (considering I/O-bound tasks)
        let base_limit = cpu_count * 2;

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
        let list = self.op.lister_with(path).recursive(recur).await?;
        Ok(list.try_collect::<Vec<_>>().await?)
    }

    /// Uploads multiple files to the cloud storage.
    ///
    /// # Arguments
    ///
    /// * `files` - A vector of `PathBuf` representing the local file paths to upload.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    pub async fn upload_files(&self, files: Vec<UpFile>) -> Result<()> {
        let mut set = JoinSet::new();

        // Calculate concurrency limit
        let concurrency_limit = Self::get_concurrency_limit();
        debug!(
            "Using concurrency limit of {} for upload",
            concurrency_limit
        );

        for local_p_string in files {
            // uploader_clone is used to call methods in the async task
            let uploader_clone = self.clone();

            set.spawn(async move {
                trace!("Uploading file: {:?}", local_p_string);
                // Use the cloned PathBuf in the async task
                if let Err(e) = uploader_clone
                    .upload_to_cloud(
                        local_p_string.local_path.to_string_lossy().as_ref(),
                        local_p_string.cloud_path.as_str(),
                    )
                    .await
                {
                    error!(
                        "Error uploading file {}: {}",
                        local_p_string.local_path.display(),
                        e
                    );
                }
            });

            // Limit the number of concurrent tasks
            if set.len() >= concurrency_limit {
                set.join_next().await;
            }
        }

        // Wait for all remaining tasks to complete
        while let Some(_) = set.join_next().await {}

        Ok(())
    }

    /// Uploads a single file to the cloud storage.
    ///
    /// # Arguments
    ///
    /// * `local_path` - A string slice representing the local file path.
    /// * `cloud_path` - A string slice representing the destination path in the cloud.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    async fn upload_to_cloud(&self, local_path: &str, cloud_path: &str) -> Result<()> {
        use futures::AsyncWriteExt;

        // Read local file
        let mut f = File::open(local_path)
            .await
            .with_context(|| format!("Failed to open local file {}", local_path))?;

        // Create a writer for the cloud file, defaulting to 3-thread parallel upload
        let mut writer = self
            .op
            .writer_with(cloud_path)
            .concurrent(3)
            .await
            .with_context(|| format!("Failed to create writer for cloud path {}", cloud_path))?
            .into_futures_async_write();
        let mut buf = [0_u8; 8192];
        let mut uploaded = 0;

        info!("Uploading file {} to cloud path {}", local_path, cloud_path);
        // Read local file and write to the cloud
        // Use a loop to read the file until EOF
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut f, &mut buf[..])
                .await
                .with_context(|| format!("Failed to read from local file {}", local_path))?;
            if n == 0 {
                break;
            }
            writer
                .write_all(&buf[..n])
                .await
                .with_context(|| format!("Failed to write to cloud path {}", cloud_path))?;
            uploaded += n;
        }
        // Close the writer to ensure all data is uploaded
        writer
            .close()
            .await
            .with_context(|| format!("Failed to finalize upload to {}", cloud_path))?;

        debug!("Total file upload of {} bytes.", uploaded);
        Ok(())
    }

    /// Deletes multiple files from the cloud storage.
    ///
    /// # Arguments
    ///
    /// * `paths` - A vector of `PathBuf` representing the file paths to delete.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    pub async fn delete_files(&self, paths: Vec<UpFile>) -> Result<()> {
        let mut set = JoinSet::new();
        let concurrency_limit = Self::get_concurrency_limit(); // Dynamically calculate concurrency limit
        info!(
            "Using concurrency limit of {} for deletion",
            concurrency_limit
        );

        for path in paths {
            // Create a separate task for each file to delete
            let uploader_clone = self.clone();
            set.spawn(async move {
                if let Err(e) = uploader_clone.delete_from_cloud(&path.cloud_path).await {
                    error!("Error deleting file {}: {}", path.cloud_path, e);
                }
            });

            // Limit the number of concurrent tasks
            if set.len() >= concurrency_limit {
                set.join_next().await;
            }
        }

        // Wait for all remaining tasks to complete
        while let Some(_) = set.join_next().await {}

        Ok(())
    }

    /// Deletes a single file from the cloud storage.
    ///
    /// # Arguments
    ///
    /// * `path` - A string slice representing the file path to delete.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or failure.
    async fn delete_from_cloud(&self, path: &str) -> Result<()> {
        info!("Deleting file from cloud: {}", path);

        self.op
            .delete(path)
            .await
            .with_context(|| format!("Failed to delete file {}", path))?;

        info!("Successfully deleted file: {}", path);
        Ok(())
    }
}
