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
use uploader::{s3::AwsS3, uploader::Uploader};
use url::Url;

// Modules
mod differ;
mod mdparser;
mod uploader;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    std::env::set_var("RUST_LOG", "trace");
    env_logger::init();

    let args = Args::parse();

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

    info!("Starting to upload files...");
    debug!("Source path: {:?}", md_src_path);
    // 读取给定路径下所有文件以及文件夹
    let files = read_file_list(&md_src_path, &depth)?;
    // 过滤出有效的文件
    // 1. 只保留文件，排除文件夹
    // 2. 只保留存在的文件
    // 3. 只保留扩展名为 .md 的文件
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

    // 从每个有效的 Markdown 文件中提取本地图片链接
    let image_path_list: Vec<PathBuf> = vaild_files
        .par_iter()
        .filter_map(|current_mdfile_path| extract_image_paths_from_file(current_mdfile_path))
        .flatten()
        .collect();

    // 输出所有搜寻到的图像
    info!(
        "{} img links detected in markdown files.",
        image_path_list.len()
    );
    for item in &image_path_list {
        trace!("Image detected: {:?}", item);
    }

    // 计算本地图片MD5值
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

    // 从云端拉取文件列表
    trace!("Starting to list cloud files...");
    let list = Uploader::new(op.clone()).list_cloud("/", true).await?;
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

    // 比较云端文件和本地文件，找出需要上传到云端的文件
    let (uploadlist, deletelist, replacelist) = diff::diff(local_img_list, remote_img_list)?;

    // 输出差异列表
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

    // 创建上传器实例
    let uploader = Uploader::new(op.clone());

    // 处理需要上传的文件
    if !uploadlist.is_empty() {
        info!("开始上传文件...");
        uploader.upload_files(uploadlist).await?;
    }

    // 处理需要删除的文件
    if !deletelist.is_empty() {
        info!("开始删除文件...");
        uploader.delete_files(deletelist).await?;
    }

    // 处理需要替换的文件
    if !replacelist.is_empty() {
        info!("开始替换文件...");
        uploader.upload_files(replacelist).await?;
    }

    // 处理 Markdown 文件中的链接替换
    if !image_path_list.is_empty() {
        info!("开始替换 Markdown 文件中的图片链接...");

        // 创建一个映射表，将本地图片路径映射到 S3 URL
        let mut path_map: HashMap<String, Url> = HashMap::new();
        let mut affected_md_files: HashSet<PathBuf> = HashSet::new();

        // 从云端获取文件列表，用于验证文件是否存在于S3
        debug!("获取S3云端文件列表以验证文件存在...");
        let cloud_files = match Uploader::new(op.clone()).list_cloud("/", true).await {
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
                warn!("获取云端文件列表失败: {}, 将跳过链接替换", e);
                HashSet::new()
            }
        };

        // 遍历所有已上传和替换的图片，构建映射表
        for img_path in &image_path_list {
            // 获取文件名
            if let Some(filename) = img_path.file_name() {
                let file_name_str = filename.to_string_lossy().to_string();

                // 检查文件是否存在于S3
                if cloud_files.contains(&file_name_str) {
                    // 构建 S3 URL，使用自定义域名
                    let s3_url = Url::from_str(&format!(
                        "{}/{}",
                        domain.join(&remote_root)?.to_string(),
                        file_name_str
                    ))?;
                    debug!("构建的 S3 URL: {}", s3_url);
                    path_map.insert(file_name_str, s3_url);

                    // 遍历所有 Markdown 文件，找出包含此图片的文件
                    for md_file in &vaild_files {
                        if let Some(img_paths) = extract_image_paths_from_file(md_file) {
                            if img_paths.contains(img_path) {
                                affected_md_files.insert(md_file.clone());
                            }
                        }
                    }
                } else {
                    debug!("S3中不存在文件: {}, 跳过链接替换", file_name_str);
                }
            }
        }

        // 遍历受影响的 Markdown 文件，替换链接
        for md_file in affected_md_files {
            info!("Replacing links in file: {:?}", md_file);

            // 读取 Markdown 文件内容
            if let Ok(content) = std::fs::read_to_string(&md_file) {
                // 执行链接替换
                let new_content = mdparser::mdparser::link_replacer(&content, &path_map);

                // 写回文件
                if let Err(e) = std::fs::write(&md_file, new_content) {
                    warn!("Failed to write back to file {:?}: {}", md_file, e);
                } else {
                    info!("Successfully updated links in {:?}", md_file);
                }
            } else {
                warn!("Failed to read file for link replacement: {:?}", md_file);
            }
        }

        info!("完成 Markdown 图片链接替换.");
    }

    Ok(())
}

/// 从单个 Markdown 文件中提取所有有效的图片路径
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

/// 解析图片路径
fn resolve_image_path(img_path: PathBuf, md_file_path: &PathBuf) -> Option<PathBuf> {
    // 处理绝对路径
    if img_path.is_absolute() {
        return Some(img_path);
    }

    // 处理相对路径
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
