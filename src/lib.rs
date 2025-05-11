pub mod cli;

use anyhow::{Ok, Result};
use log::debug;
use md5::Digest;
use std::{fmt::Debug, path::Path};
use walkdir::WalkDir;

#[derive(Eq)]
pub struct FileInfo {
    path: std::path::PathBuf,
    md5: String,
}

impl FileInfo {
    pub fn new(path: std::path::PathBuf, md5: String) -> Self {
        FileInfo { path, md5 }
    }

    /// Get File Path
    /// # Return
    /// - &String
    pub fn get_path(&self) -> &std::path::PathBuf {
        &self.path
    }

    /// Get File MD5
    /// # Return
    /// - &String
    /// # Note
    /// - The MD5 value is a 32-character hexadecimal string.
    /// - Example: `"d41d8cd98f00b204e9800998ecf8427e"`
    pub fn get_md5(&self) -> &String {
        &self.md5
    }
}

impl std::cmp::Ord for FileInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.path.file_name().cmp(&other.path.file_name()) {
            std::cmp::Ordering::Less => std::cmp::Ordering::Less,
            std::cmp::Ordering::Greater => std::cmp::Ordering::Greater,
            std::cmp::Ordering::Equal => {
                if self.md5 == other.md5 {
                    std::cmp::Ordering::Equal
                } else {
                    std::cmp::Ordering::Greater
                }
            }
        }
    }
}

impl std::cmp::PartialOrd for FileInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match self.path.file_name().partial_cmp(&other.path.file_name()) {
            Some(ordering) => Some(ordering),
            None => {
                if self.path.file_name().is_none() && other.path.file_name().is_none() {
                    match self.md5.partial_cmp(&other.md5) {
                        Some(ordering) => Some(ordering),
                        None => None,
                    }
                } else {
                    None
                }
            }
        }
    }
}

impl std::cmp::PartialEq for FileInfo {
    fn eq(&self, other: &Self) -> bool {
        self.md5 == other.md5 && self.path.file_name() == other.path.file_name()
    }
}

impl std::fmt::Debug for FileInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileInfo")
            .field("path", &self.path)
            .field("md5", &self.md5)
            .finish()
    }
}

impl std::fmt::Display for FileInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FileInfo {{ path: {:?}, md5: {} }}", self.path, self.md5)
    }
}

/// Read the file list under the specified path
/// # Arguments
/// - `path` - The path to read
/// - `depth` - Recursion depth
/// # Return
/// - `Result<Vec<walkdir::DirEntry>, std::io::Error>` - Returns the file list or an error
pub fn read_file_list<P: AsRef<Path> + Debug>(
    path: &P,
    depth: &usize,
) -> Result<Vec<walkdir::DirEntry>> {
    // Create file list
    let mut file_list: Vec<walkdir::DirEntry> = Vec::new();
    // Traverse directory
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
    Ok(format!("{:x}", md))
}
