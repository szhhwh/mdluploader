pub mod cli;

use std::path::Path;
use walkdir::WalkDir;

pub fn read_file_list<P: AsRef<Path>>(path: &P, depth: &usize) -> Option<Vec<walkdir::DirEntry>> {
    let mut file_list: Vec<walkdir::DirEntry>  = Vec::new();
    for item in WalkDir::new(path).max_depth(*depth) {
        if let Ok(v) = item {
            file_list.push(v);
        }
    }
    Some(file_list)
}
