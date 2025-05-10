pub mod cli;

use std::path::Path;
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

pub fn read_file_list<P: AsRef<Path>>(path: &P, depth: &usize) -> Option<Vec<walkdir::DirEntry>> {
    let mut file_list: Vec<walkdir::DirEntry> = Vec::new();
    for item in WalkDir::new(path).max_depth(*depth) {
        if let Ok(v) = item {
            file_list.push(v);
        }
    }
    Some(file_list)
}
