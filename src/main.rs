use anyhow::{Context, Result};
use clap::Parser;
use differ::diff;
use log::{debug, info, trace, warn};
use mdluploader::{cli::Args, *};
use opendal::Operator;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use uploader::{s3::AwsS3, uploader::UpFile, uploader::Uploader};
use url::Url;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    if let Err(_) = std::env::var("RUST_LOG") {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    let args = Args::parse();

    info!("Welcome to Markdown Image Uploader!");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!(
        "Build time: {}",
        if let Some(timestamp) = option_env!("VERGEN_BUILD_TIMESTAMP") {
            timestamp
        } else {
            "Unknown Build Time"
        }
    );

    match args.command {
        cli::Commands::Upload {
            path,
            depth,
            bucket,
            ak,
            sk,
            region,
            endpoint,
            domain,
            remote_root,
        } => {
            let remote_root = remote_root.unwrap_or("/".to_string());
            let op = AwsS3::new(
                bucket,
                ak,
                sk,
                region,
                endpoint.clone(),
                remote_root.clone(),
            )
            .build()?;
            upload(path, depth, op, domain, remote_root).await?;
        }
    }

    Ok(())
}

async fn upload(
    md_src_path: PathBuf,
    depth: usize,
    op: Operator,
    domain: String,
    remote_root: String,
) -> Result<()> {
    let domain = Url::parse(&domain).with_context(|| format!("Invalid domain URL: {}", domain))?;
    info!("Get vaild domain: {}", domain);

    debug!("Source path: {:?}", md_src_path);
    // Read all files and folders from the given path
    let files = read_file_list(&md_src_path, &depth)?;
    // Filter valid files
    // 1. Keep only files, exclude folders
    // 2. Keep only existing files
    // 3. Keep only files with .md extension
    let vaild_files: Vec<PathBuf> = files
        .par_iter()
        .filter(|x| x.file_type().is_file())
        .filter_map(|x| {
            let p = PathBuf::from(x.path());
            if p.try_exists().unwrap_or(false) {
                if p.extension().unwrap_or_default() == "md" {
                    trace!("Valid file detected: {:?}", p);
                    Some(p)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // Extract local image links from each valid Markdown file
    let image_path_list: HashSet<PathBuf> = vaild_files
        .par_iter()
        .filter_map(|current_mdfile_path| extract_image_paths_from_file(current_mdfile_path))
        .flatten()
        .collect();

    // Output all detected images
    for item in &image_path_list {
        trace!("Image detected: {:?}", item);
    }
    info!(
        "{} img links detected in markdown files.",
        image_path_list.len()
    );

    // Calculate MD5 values for local images
    let local_img_list: Vec<FileInfo> = image_path_list
        .par_iter()
        .filter_map(|img| match get_file_md5(img) {
            Ok(md5) => Some(FileInfo::new(img.clone(), md5)),
            Err(e) => {
                log::error!("Failed to get MD5 for {:?}: {}", img, e);
                None
            }
        })
        .collect();

    // Create uploader instance
    let uploader = Uploader::new(op);

    // Fetch file list from cloud
    trace!("Starting to list cloud files...");
    let list = uploader.list_cloud("/", true).await?;
    let remote_img_list: Vec<FileInfo> = list
        .par_iter()
        .map(|entry| {
            FileInfo::new(
                PathBuf::from(entry.path().to_string()),
                entry.metadata().content_md5().unwrap().to_string(),
            )
        })
        .collect();
    trace!("Finished list cloud files.");

    // Compare cloud files and local files to find files that need to be uploaded
    let (uploadlist, deletelist, replacelist) = diff::diff(local_img_list, remote_img_list)?;

    // Output difference lists
    info!("{} files need to be uploaded.", uploadlist.len());
    for item in &uploadlist {
        debug!("File to upload: {:?}", item);
    }
    info!("{} files need to be deleted.", deletelist.len());
    for item in &deletelist {
        debug!("File to delete: {:?}", item);
    }
    info!("{} files need to be replaced.", replacelist.len());
    for item in &replacelist {
        debug!("File to replace: {:?}", item);
    }

    // Convert all PathBuf to UpFile, which contains both local path and cloud path
    let md_src_path = md_src_path // Ensure the source path is canonicalized
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize source path: {:?}", md_src_path))?;

    let uploadlist: Vec<UpFile> = uploadlist
        .par_iter()
        .map(|file| UpFile::from_pathbuf(file, &md_src_path).unwrap())
        .collect();

    let deletelist: Vec<String> = deletelist
        .par_iter()
        .map(|file| UpFile::from_pathbuf(file, &md_src_path).unwrap().cloud_path)
        .collect();

    let replacelist: Vec<UpFile> = replacelist
        .par_iter()
        .map(|file| UpFile::from_pathbuf(file, &md_src_path).unwrap())
        .collect();

    info!("Starting to upload files...");
    // Process files that need to be uploaded
    if !uploadlist.is_empty() {
        info!("Starting to upload files...");
        uploader.upload_files(uploadlist).await?;
    }

    // Process files that need to be deleted
    if !deletelist.is_empty() {
        info!("Starting to delete files...");
        uploader.delete_files(deletelist).await?;
    }

    // Process files that need to be replaced
    if !replacelist.is_empty() {
        info!("Starting to replace files...");
        uploader.upload_files(replacelist).await?;
    }

    // Process link replacement in Markdown files
    if !image_path_list.is_empty() {
        info!("Starting to replace image links in Markdown files...");

        // Create a mapping table that maps local image paths to S3 URLs
        let mut path_map: HashMap<String, Url> = HashMap::new();
        let mut affected_md_files: HashSet<PathBuf> = HashSet::new();

        // Get file list from cloud to verify file existence in S3
        debug!("Getting S3 cloud file list to verify file existence...");
        let cloud_files = match uploader.list_cloud("/", true).await {
            Ok(files) => {
                let file_names: HashSet<String> = files
                    .par_iter()
                    .filter_map(|entry| {
                        let path = entry.path().to_string();
                        path.split('/').last().map(|s| s.to_string())
                    })
                    .collect();
                file_names
            }
            Err(e) => {
                warn!(
                    "Failed to get cloud file list: {}, skipping link replacement",
                    e
                );
                HashSet::new()
            }
        };

        // Iterate through all uploaded and replaced images to build a mapping table
        for img_path in &image_path_list {
            // Get filename
            if let Some(filename) = img_path.file_name() {
                let file_name_str = filename.to_string_lossy().to_string();

                // Check if the file exists in S3
                if cloud_files.contains(&file_name_str) {
                    // Build S3 URL using custom domain
                    let s3_url = Url::from_str(&format!(
                        "{}/{}",
                        domain.join(&remote_root)?.to_string(),
                        file_name_str
                    ))?;
                    debug!("Constructed S3 URL: {}", s3_url);
                    path_map.insert(file_name_str, s3_url);

                    // Iterate through all Markdown files to find files containing this image
                    for md_file in &vaild_files {
                        if let Some(img_paths) = extract_image_paths_from_file(md_file) {
                            if img_paths.contains(img_path) {
                                affected_md_files.insert(md_file.clone());
                            }
                        }
                    }
                } else {
                    debug!(
                        "File does not exist in S3: {}, skipping link replacement",
                        file_name_str
                    );
                }
            }
        }

        // Iterate through affected Markdown files to replace links
        for md_file in affected_md_files {
            info!("Replacing links in file: {:?}", md_file);

            // Read Markdown file content
            if let Ok(content) = std::fs::read_to_string(&md_file) {
                // Execute link replacement
                let new_content = mdparser::mdparser::link_replacer(&content, &path_map);

                // Write back to file
                if let Err(e) = std::fs::write(&md_file, new_content) {
                    warn!("Failed to write back to file {:?}: {}", md_file, e);
                } else {
                    info!("Successfully updated links in {:?}", md_file);
                }
            } else {
                warn!("Failed to read file for link replacement: {:?}", md_file);
            }
        }

        info!("Completed Markdown image link replacement.");
    }

    Ok(())
}

/// Extract all valid image paths from a single Markdown file
fn extract_image_paths_from_file(current_mdfile_path: &PathBuf) -> Option<Vec<PathBuf>> {
    let buff = match std::fs::read_to_string(current_mdfile_path) {
        Ok(content) => content,
        Err(_) => return None,
    };

    mdparser::mdparser::extract_img_urls(&buff).map(|urls| {
        urls.into_iter()
            .filter_map(|img_path| resolve_image_path(img_path, current_mdfile_path))
            .collect()
    })
}

/// Resolve image path
fn resolve_image_path(img_path: PathBuf, md_file_path: &PathBuf) -> Option<PathBuf> {
    // Handle absolute path
    if img_path.is_absolute() {
        return Some(img_path);
    }

    // Handle relative path
    let parent_dir = md_file_path.parent()?;
    let full_path = PathBuf::from(parent_dir)
        .join(img_path)
        .canonicalize()
        .ok()?;

    if full_path.try_exists().unwrap_or(false) {
        Some(full_path)
    } else {
        None
    }
}
