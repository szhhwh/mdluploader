use anyhow::{Ok, Result};
use futures::TryStreamExt;
use opendal::Operator;

/// # 获取云端文件列表（包括文件夹）
/// - `op` 是一个 `Operator` 对象，用于与云存储进行交互。
/// - 'path` 是一个字符串，表示要列出的目录的路径。
/// - `recur` 是一个布尔值，指示是否递归列出目录。
pub async fn list_cloud(op: Operator, path: &str, recur: bool) -> Result<Vec<opendal::Entry>> {
    let mut list = op.lister_with(path).recursive(recur).await?;

    Ok(list.try_collect::<Vec<_>>().await?)
}
