pub mod cli;

use anyhow::{Ok, Result};
use log::{debug, trace};
use md5::Digest;
use std::{fmt::Debug, path::Path};
use walkdir::WalkDir;

pub struct FileInfo {
    path: std::path::PathBuf,
    md5: String,
}

impl FileInfo {
    pub fn new(path: std::path::PathBuf, md5: String) -> Self {
        FileInfo { path, md5 }
    }

    /// 获取文件路径
    /// # Return
    /// - &String
    pub fn get_path(&self) -> &std::path::PathBuf {
        &self.path
    }

    /// 获取文件 MD5
    /// # Return
    /// - &String
    /// # Note
    /// - MD5 值是一个 32 位的十六进制字符串
    /// - 例如：`"d41d8cd98f00b204e9800998ecf8427e"`
    pub fn get_md5(&self) -> &String {
        &self.md5
    }
}

impl std::cmp::Eq for FileInfo {
    fn assert_receiver_is_total_eq(&self) {}
}

impl std::cmp::Ord for FileInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.md5.cmp(&other.md5)
    }
}

impl std::cmp::PartialOrd for FileInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.md5.partial_cmp(&other.md5)
    }
}

impl std::cmp::PartialEq for FileInfo {
    fn eq(&self, other: &Self) -> bool {
        self.md5 == other.md5
    }
}

/// 读取指定路径下的文件列表
/// # Arguments
/// - `path` - 要读取的路径
/// - `depth` - 递归深度
/// # Return
/// - `Result<Vec<walkdir::DirEntry>, std::io::Error>` - 返回文件列表或错误
pub fn read_file_list<P: AsRef<Path> + Debug>(
    path: &P,
    depth: &usize,
) -> Result<Vec<walkdir::DirEntry>> {
    // 创建文件列表
    let mut file_list: Vec<walkdir::DirEntry> = Vec::new();
    // 遍历目录
    debug!("traversing directory: {:?}", path);
    for item in WalkDir::new(path).max_depth(*depth) {
        file_list.push(item?);
    }
    Ok(file_list)
}

pub fn get_file_md5<P: AsRef<Path>>(img: &P) -> Result<String> {
    let mut md = md5::Md5::new();
    let mut img_content = vec![];
    let _ = std::io::Read::read_to_end(&mut std::fs::File::open(img)?, &mut img_content);
    md.update(&img_content);
    let md = md.finalize();
    trace!("MD5: {:x}", md);
    Ok(format!("{:x}", md))
}
