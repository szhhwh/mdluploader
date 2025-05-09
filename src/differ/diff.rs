use anyhow::{Ok, Result};
use log::info;

/// # 文件差异比较
/// # Return
/// - (uploadlist, deletelist, replacelist)
pub fn diff(
    local: Vec<(String, String)>,
    remote: Vec<(String, String)>,
) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    // 排序
    let mut local_sorted = local;
    local_sorted.sort_unstable();
    let mut remote_sorted = remote;
    remote_sorted.sort_unstable();

    // 索引
    let mut local_idx = 0;
    let mut remote_idx = 0;

    // 计数器
    let mut uploads = 0;
    let mut deletes = 0;
    let mut replaces = 0;

    let mut uploadlist = Vec::new();
    let mut deletelist = Vec::new();
    let mut replacelist = Vec::new();

    while local_idx < local_sorted.len() && remote_idx < remote_sorted.len() {
        let (local_name, local_md5) = &local_sorted[local_idx];
        let (remote_name, remote_md5) = &remote_sorted[remote_idx];

        match local_name.cmp(remote_name) {
            std::cmp::Ordering::Less => {
                info!("文件 {} 在云端不存在，等待上传", local_name);
                uploadlist.push(local_name.clone());
                uploads += 1;
                local_idx += 1; // 只增加本地索引
            }
            std::cmp::Ordering::Greater => {
                info!("文件 {} 在本地不存在，等待删除", remote_name);
                deletelist.push(remote_name.clone());
                deletes += 1;
                remote_idx += 1; // 只增加远程索引
            }
            std::cmp::Ordering::Equal => {
                if local_md5 != remote_md5 {
                    info!("文件 {} 内容已变更，等待更新", local_name);
                    replacelist.push(local_name.clone());
                    replaces += 1;
                }
                local_idx += 1; // 两个索引都增加
                remote_idx += 1;
            }
        }
    }

    // 处理剩余的本地文件
    while local_idx < local_sorted.len() {
        let (local_name, _) = &local_sorted[local_idx];
        info!("文件 {} 在云端不存在，等待上传", local_name);
        uploadlist.push(local_name.clone());
        uploads += 1;
        local_idx += 1;
    }

    // 处理剩余的远程文件
    while remote_idx < remote_sorted.len() {
        let (remote_name, _) = &remote_sorted[remote_idx];
        info!("文件 {} 在本地不存在，等待删除", remote_name);
        deletelist.push(remote_name.clone());
        deletes += 1;
        remote_idx += 1;
    }

    info!(
        "总计：{} 文件需要上传到云端，{} 个文件需要从云端删除，{} 个文件需要被替换",
        uploads, deletes, replaces
    );

    Ok((uploadlist, deletelist, replacelist))
}
