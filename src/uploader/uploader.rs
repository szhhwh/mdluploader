use std::path::PathBuf;

use anyhow::{Context, Ok, Result};
use futures::TryStreamExt;
use log::{debug, error, info, trace};
use opendal::Operator;
use tokio::{fs::File, task::JoinSet};

#[derive(Clone)]
pub struct Uploader {
    op: Operator,
}

impl Uploader {
    pub fn new(op: Operator) -> Self {
        Self { op }
    }

    /// # 获取适合当前系统的并发限制
    /// 
    /// 根据系统CPU核心数和其他因素计算合理的并发数
    fn get_concurrency_limit() -> usize {
        // 获取系统CPU核心数
        let cpu_count = num_cpus::get();
        
        // 基础并发数: CPU核心数 * 2 (考虑到I/O绑定任务)
        let base_limit = cpu_count * 2;
        
        // 设置一个最小值和最大值，防止极端情况
        let min_limit = 4;
        let max_limit = 32;
        
        base_limit.clamp(min_limit, max_limit)
    }

    /// # 获取云端文件列表（包括文件夹）
    /// - `path` 是一个字符串，表示要列出的目录的路径。
    /// - `recur` 是一个布尔值，指示是否递归列出目录。
    pub async fn list_cloud(&self, path: &str, recur: bool) -> Result<Vec<opendal::Entry>> {
        let list = self.op.lister_with(path).recursive(recur).await?;
        Ok(list.try_collect::<Vec<_>>().await?)
    }

    pub async fn upload_files(&self, files: Vec<PathBuf>) -> Result<()> {
        let mut set = JoinSet::new();
        
        // 计算并发限制
        let concurrency_limit = Self::get_concurrency_limit();
        debug!("Using concurrency limit of {} for upload", concurrency_limit);

        for local_p_string in files {
            // uploader_clone 用于在异步任务中调用方法
            let uploader_clone = self.clone();

            set.spawn(async move {
                trace!("Uploading file: {:?}", local_p_string);
                // Use the cloned PathBuf in the async task
                if let Err(e) = uploader_clone
                    .upload_to_cloud(
                        local_p_string.to_string_lossy().as_ref(),
                        local_p_string
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .as_ref(),
                    )
                    .await
                {
                    error!("Error uploading file {}: {}", local_p_string.display(), e);
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

    async fn upload_to_cloud(&self, local_path: &str, cloud_path: &str) -> Result<()> {
        use futures::AsyncWriteExt;

        // 读取本地文件
        let mut f = File::open(local_path)
            .await
            .with_context(|| format!("Failed to open local file {}", local_path))?;

        // 创建云端文件的写入器，写入器默认以3线程并行上传
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
        // 读取本地文件并写入云端
        // 使用循环读取文件，直到 EOF
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
        // 关闭写入器，确保所有数据都被上传
        writer
            .close()
            .await
            .with_context(|| format!("Failed to finalize upload to {}", cloud_path))?;

        debug!("Total file upload of {} bytes.", uploaded);
        Ok(())
    }

    /// # 删除云端文件
    /// - `paths` 是要删除的文件路径列表
    pub async fn delete_files(&self, paths: Vec<PathBuf>) -> Result<()> {
        let mut set = JoinSet::new();
        let concurrency_limit = Self::get_concurrency_limit(); // 动态计算并发限制
        info!("Using concurrency limit of {} for deletion", concurrency_limit);

        for path in paths {
            // 为每个文件创建一个独立的任务进行删除
            let uploader_clone = self.clone();
            let path_clone = path.clone(); // Clone PathBuf for the async task
            set.spawn(async move {
                if let Err(e) = uploader_clone.delete_from_cloud(&path_clone.to_string_lossy().as_ref()).await {
                    error!("Error deleting file {}: {}", path_clone.display(), e);
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

    /// # 从云端删除单个文件
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
