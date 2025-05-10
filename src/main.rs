use anyhow::Result;
use clap::Parser;
use differ::diff;
use log::{debug, info, trace};
use mdluploader::{cli::Args, *};
use opendal::Operator;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::path::PathBuf;
use uploader::{s3::AwsS3, uploader::Uploader};

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
            remote_root,
        } => {
            let op = AwsS3::new(
                bucket,
                ak,
                sk,
                region,
                endpoint,
                remote_root.unwrap_or("/".to_string()),
            )
            .build()?;
            upload(path, depth, op).await?;
        }
    }

    Ok(())
}

async fn upload(md_src_path: PathBuf, depth: usize, op: Operator) -> Result<()> {
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
                if p.extension().unwrap() == "md" {
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
        .filter_map(|img| {
            match get_file_md5(img) {
                Ok(md5) => Some(FileInfo::new(img.clone(), md5)),
                Err(e) => {
                    log::error!("Failed to get MD5 for {:?}: {}", img, e);
                    None
                }
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
