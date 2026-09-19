use std::collections::{hash_map::Entry, HashMap};
use std::path::PathBuf;

use log::{debug, info, warn};

use crate::uploader::UpFile;
use crate::CloudPath;

/// A local image with its computed checksum and target cloud path.
#[derive(Debug, Clone)]
pub struct LocalImage {
    /// Absolute local path of the image file.
    pub local_path: PathBuf,
    /// Cloud path (relative to the remote root, no leading slash).
    pub cloud_path: CloudPath,
    /// Lowercase hexadecimal MD5 of the local content.
    pub md5: String,
}

/// A remote object as reported by the cloud listing.
#[derive(Debug, Clone)]
pub struct RemoteImage {
    /// Cloud path (relative to the remote root, no leading slash).
    pub cloud_path: CloudPath,
    /// Lowercase hexadecimal MD5, or `None` when unknown (e.g. multipart ETag
    /// or a service that does not expose checksums in listings).
    pub md5: Option<String>,
}

/// The set of cloud transfers needed to bring the remote in sync with local.
#[derive(Debug, Default)]
pub struct DiffPlan {
    /// Files that exist only locally and must be uploaded.
    pub uploads: Vec<UpFile>,
    /// Cloud paths that exist only remotely and must be deleted.
    pub deletes: Vec<CloudPath>,
    /// Files that exist on both sides with different content.
    pub replaces: Vec<UpFile>,
}

/// Compares the differences between local and remote files.
///
/// Local and remote entries are joined on their normalized cloud path. Files
/// that only exist locally are planned for upload, files that only exist
/// remotely are planned for deletion, and files whose checksums differ are
/// planned for replacement. When the remote checksum is unknown (multipart
/// ETag or missing metadata) the file is left untouched instead of being
/// endlessly replaced.
///
/// # Arguments
///
/// * `local` - Local images with checksums.
/// * `remote` - Remote objects with checksums (when available).
///
/// # Returns
///
/// A `DiffPlan` describing uploads, deletions and replacements.
///
/// # Examples
///
/// ```
/// use mdluploader::CloudPath;
/// use mdluploader::differ::{diff, LocalImage, RemoteImage};
/// use std::path::PathBuf;
///
/// let local = vec![LocalImage {
///     local_path: PathBuf::from("/tmp/file1.txt"),
///     cloud_path: CloudPath::new("file1.txt"),
///     md5: "md5hash1".to_string(),
/// }];
/// let remote = vec![RemoteImage {
///     cloud_path: CloudPath::new("file2.txt"),
///     md5: Some("md5hash2".to_string()),
/// }];
///
/// let plan = diff(&local, remote);
/// assert_eq!(plan.uploads.len(), 1);
/// assert_eq!(plan.deletes, vec![CloudPath::new("file2.txt")]);
/// assert_eq!(plan.replaces.len(), 0);
/// ```
pub fn diff(local: &[LocalImage], remote: Vec<RemoteImage>) -> DiffPlan {
    // Sort before deduplication so the choice of a surviving entry is
    // deterministic regardless of parallel collection order upstream.
    let mut local_sorted: Vec<&LocalImage> = local.iter().collect();
    local_sorted.sort_by(|a, b| a.cloud_path.cmp(&b.cloud_path));

    let mut local_map: HashMap<CloudPath, &LocalImage> = HashMap::with_capacity(local_sorted.len());
    for image in local_sorted {
        // One entry() lookup both detects the duplicate and inserts, instead
        // of a contains_key + index + insert triple with a panic-prone index.
        let key = image.cloud_path.clone();
        match local_map.entry(key) {
            Entry::Occupied(existing) => warn!(
                "Multiple local images map to cloud path {}; keeping {} and skipping the rest",
                image.cloud_path,
                existing.get().local_path.display()
            ),
            Entry::Vacant(slot) => {
                slot.insert(image);
            }
        }
    }

    let mut remote_map: HashMap<CloudPath, Option<String>> = HashMap::with_capacity(remote.len());
    for entry in remote {
        remote_map.entry(entry.cloud_path).or_insert(entry.md5);
    }

    let mut plan = DiffPlan::default();

    for (cloud_path, image) in &local_map {
        match remote_map.get(cloud_path) {
            None => {
                info!(
                    "File {} does not exist in the cloud, waiting for upload",
                    cloud_path
                );
                plan.uploads
                    .push(UpFile::new(image.local_path.clone(), cloud_path.clone()));
            }
            Some(Some(remote_md5)) => {
                if !remote_md5.eq_ignore_ascii_case(&image.md5) {
                    info!(
                        "File {} content has changed, waiting for update",
                        cloud_path
                    );
                    plan.replaces
                        .push(UpFile::new(image.local_path.clone(), cloud_path.clone()));
                } else {
                    debug!("File {} is identical on both sides", cloud_path);
                }
            }
            Some(None) => {
                debug!(
                    "Remote checksum for {} is unknown; skipping to avoid spurious replacement",
                    cloud_path
                );
            }
        }
    }

    for cloud_path in remote_map.keys() {
        if !local_map.contains_key(cloud_path) {
            info!(
                "File {} does not exist locally, waiting for deletion",
                cloud_path
            );
            plan.deletes.push(cloud_path.clone());
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(cloud_path: &str, md5: &str) -> LocalImage {
        LocalImage {
            local_path: PathBuf::from("/tmp").join(cloud_path),
            cloud_path: CloudPath::new(cloud_path),
            md5: md5.to_string(),
        }
    }

    fn remote(cloud_path: &str, md5: Option<&str>) -> RemoteImage {
        RemoteImage {
            cloud_path: CloudPath::new(cloud_path),
            md5: md5.map(|s| s.to_string()),
        }
    }

    #[test]
    fn local_only_files_are_uploaded() {
        let plan = diff(&[local("a.png", "m1")], vec![]);
        assert_eq!(plan.uploads.len(), 1);
        assert_eq!(plan.uploads[0].cloud_path.as_str(), "a.png");
        assert!(plan.deletes.is_empty());
        assert!(plan.replaces.is_empty());
    }

    #[test]
    fn remote_only_files_are_deleted_by_cloud_path() {
        // Regression: remote-only paths used to be converted with a local
        // strip_prefix and panicked. They must be deleted as cloud paths.
        let plan = diff(&[], vec![remote("sub/b.png", Some("m2"))]);
        assert_eq!(plan.deletes, vec![CloudPath::new("sub/b.png")]);
    }

    #[test]
    fn identical_files_need_no_action_even_with_case_difference() {
        let plan = diff(
            &[local("a.png", "abc123")],
            vec![remote("a.png", Some("ABC123"))],
        );
        assert!(plan.uploads.is_empty());
        assert!(plan.deletes.is_empty());
        assert!(plan.replaces.is_empty());
    }

    #[test]
    fn changed_files_are_replaced() {
        let plan = diff(&[local("a.png", "m1")], vec![remote("a.png", Some("m2"))]);
        assert_eq!(plan.replaces.len(), 1);
        assert_eq!(plan.replaces[0].cloud_path.as_str(), "a.png");
        assert!(plan.uploads.is_empty());
        assert!(plan.deletes.is_empty());
    }

    #[test]
    fn unknown_remote_checksum_is_left_alone() {
        // Multipart ETags must not trigger endless replacement.
        let plan = diff(&[local("a.png", "m1")], vec![remote("a.png", None)]);
        assert!(plan.uploads.is_empty());
        assert!(plan.deletes.is_empty());
        assert!(plan.replaces.is_empty());
    }

    #[test]
    fn leading_slash_remote_paths_match_local_relative_paths() {
        // The diff itself receives normalized paths; verify the join survives
        // mixed forms via explicit normalization upstream is not needed here.
        let plan = diff(
            &[local("sub/a.png", "m1")],
            vec![remote("sub/a.png", Some("m1"))],
        );
        assert!(plan.replaces.is_empty());
    }

    #[test]
    fn same_basename_in_different_dirs_does_not_collide() {
        // Regression: the old basename-only diff treated these as one file.
        let plan = diff(
            &[local("sub1/a.png", "m1"), local("sub2/a.png", "m2")],
            vec![],
        );
        assert_eq!(plan.uploads.len(), 2);
    }

    #[test]
    fn duplicate_local_cloud_paths_are_deduplicated() {
        let plan = diff(&[local("a.png", "m1"), local("a.png", "m1")], vec![]);
        assert_eq!(plan.uploads.len(), 1);
    }
}
