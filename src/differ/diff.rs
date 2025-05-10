use std::path::PathBuf;

use anyhow::{Ok, Result};
use log::{info, trace};
use mdluploader::FileInfo;

/// # 文件差异比较
/// # Return
/// - (uploadlist, deletelist, replacelist)
pub fn diff(
    local: Vec<FileInfo>,
    remote: Vec<FileInfo>,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>)> {
    // 排序
    trace!("Start sorting local and remote files");
    let mut local_sorted = local;
    local_sorted.sort_unstable();
    let mut remote_sorted = remote;
    remote_sorted.sort_unstable();

    trace!("Finished sorting local and remote files");
    trace!(
        "Local files: {:?}",
        local_sorted.iter().map(|f| f.get_path()).collect::<Vec<_>>()
    );
    trace!(
        "Remote files: {:?}",
        remote_sorted.iter().map(|f| f.get_path()).collect::<Vec<_>>()
    );

    // 索引
    let mut local_idx = 0;
    let mut remote_idx = 0;

    let mut uploadlist: Vec<PathBuf> = Vec::new();
    let mut deletelist: Vec<PathBuf> = Vec::new();
    let mut replacelist: Vec<PathBuf> = Vec::new();

    while local_idx < local_sorted.len() && remote_idx < remote_sorted.len() {
        trace!(
            "Comparing local file {} with remote file {}",
            local_sorted[local_idx].get_path().display(),
            remote_sorted[remote_idx].get_path().display()
        );
        let local_file_info = &local_sorted[local_idx];
        let local_name = local_file_info.get_path().file_name().unwrap();
        let local_md5 = local_file_info.get_md5();

        let remote_file_info = &remote_sorted[remote_idx];
        let remote_name = remote_file_info.get_path().file_name().unwrap();
        let remote_md5 = remote_file_info.get_md5();

        match local_name.cmp(remote_name) {
            std::cmp::Ordering::Less => {
                info!("文件 {:?} 在云端不存在，等待上传", local_name);
                uploadlist.push(local_file_info.get_path().to_path_buf());
                local_idx += 1; // 只增加本地索引
            }
            std::cmp::Ordering::Greater => {
                info!("文件 {:?} 在本地不存在，等待删除", remote_name);
                deletelist.push(remote_file_info.get_path().to_path_buf());
                remote_idx += 1; // 只增加远程索引
            }
            std::cmp::Ordering::Equal => {
                if local_md5 != remote_md5 {
                    info!("文件 {:?} 内容已变更，等待更新", local_name);
                    replacelist.push(local_file_info.get_path().to_path_buf());
                }
                local_idx += 1; // 两个索引都增加
                remote_idx += 1;
            }
        }
    }

    // 处理剩余的本地文件
    while local_idx < local_sorted.len() {
        let local_file_info = &local_sorted[local_idx];
        let local_name = local_file_info.get_path().file_name().unwrap();
        info!("文件 {:?} 在云端不存在，等待上传", local_name);
        uploadlist.push(local_file_info.get_path().to_path_buf());
        local_idx += 1;
    }

    // 处理剩余的远程文件
    while remote_idx < remote_sorted.len() {
        let remote_file_info = &remote_sorted[remote_idx];
        let remote_name = remote_file_info.get_path().file_name().unwrap();
        info!("文件 {:?} 在本地不存在，等待删除", remote_name);
        deletelist.push(remote_file_info.get_path().to_path_buf());
        remote_idx += 1;
    }

    Ok((uploadlist, deletelist, replacelist))
}
