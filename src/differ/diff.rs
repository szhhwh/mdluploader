use std::path::PathBuf;

use anyhow::{Ok, Result};
use log::{info, trace};
use mdluploader::FileInfo;

/// Compares the differences between local and remote files.
///
/// This function takes two vectors of `FileInfo` objects, representing local and remote files,
/// and determines which files need to be uploaded, deleted, or replaced based on their names and MD5 checksums.
///
/// # Arguments
///
/// * `local` - A vector of `FileInfo` objects representing the local files.
/// * `remote` - A vector of `FileInfo` objects representing the remote files.
///
/// # Returns
///
/// A `Result` containing a tuple of three vectors:
///
/// * `uploadlist` - A vector of `PathBuf` objects representing files that need to be uploaded.
/// * `deletelist` - A vector of `PathBuf` objects representing files that need to be deleted.
/// * `replacelist` - A vector of `PathBuf` objects representing files that need to be replaced.
///
/// # Errors
///
/// This function will return an error if any operation fails, such as sorting or accessing file information.
///
/// # Examples
///
/// ```
/// use mdluploader::FileInfo;
/// use std::path::PathBuf;
/// use mdluploader::differ::diff;
///
/// let local_files = vec![FileInfo::new(PathBuf::from("file1.txt"), "md5hash1")];
/// let remote_files = vec![FileInfo::new(PathBuf::from("file2.txt"), "md5hash2")];
///
/// let (uploadlist, deletelist, replacelist) = diff(local_files, remote_files).unwrap();
/// assert_eq!(uploadlist.len(), 1);
/// assert_eq!(deletelist.len(), 1);
/// assert_eq!(replacelist.len(), 0);
/// ```
pub fn diff(
    local: Vec<FileInfo>,
    remote: Vec<FileInfo>,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>)> {
    // Sorting
    trace!("Start sorting local and remote files");
    let mut local_sorted = local;
    local_sorted.sort_unstable();
    let mut remote_sorted = remote;
    remote_sorted.sort_unstable();

    trace!("Finished sorting local and remote files");

    // Index
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
                info!("File {:?} does not exist in the cloud, waiting for upload", local_name);
                uploadlist.push(local_file_info.get_path().to_path_buf());
                local_idx += 1; // Only increase local index
            }
            std::cmp::Ordering::Greater => {
                info!("File {:?} does not exist locally, waiting for deletion", remote_name);
                deletelist.push(remote_file_info.get_path().to_path_buf());
                remote_idx += 1; // Only increase remote index
            }
            std::cmp::Ordering::Equal => {
                if local_md5 != remote_md5 {
                    info!("File {:?} content has changed, waiting for update", local_name);
                    replacelist.push(local_file_info.get_path().to_path_buf());
                }
                local_idx += 1; // Increase both indices
                remote_idx += 1;
            }
        }
    }

    // Process remaining local files
    while local_idx < local_sorted.len() {
        let local_file_info = &local_sorted[local_idx];
        let local_name = local_file_info.get_path().file_name().unwrap();
        info!("File {:?} does not exist in the cloud, waiting for upload", local_name);
        uploadlist.push(local_file_info.get_path().to_path_buf());
        local_idx += 1;
    }

    // Process remaining remote files
    while remote_idx < remote_sorted.len() {
        let remote_file_info = &remote_sorted[remote_idx];
        let remote_name = remote_file_info.get_path().file_name().unwrap();
        info!("File {:?} does not exist locally, waiting for deletion", remote_name);
        deletelist.push(remote_file_info.get_path().to_path_buf());
        remote_idx += 1;
    }

    Ok((uploadlist, deletelist, replacelist))
}
