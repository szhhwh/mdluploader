use anyhow::Result;
use clap::Parser;
use differ::diff;
use log::{debug, info, Level};
use md5::Digest;
use mdluploader::{cli::Args, *};
use opendal::Operator;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use std::{io::Read, path::PathBuf};
use uploader::{s3::AwsS3, uploader::list_cloud};

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
        } => {
            // info!("Uploading files...");
            let op = AwsS3::new(bucket, ak, sk, region, endpoint).build()?;
            upload(path, depth, op).await?;
        }
    }

    Ok(())
}

async fn upload(md_src_path: PathBuf, depth: usize, op: Operator) -> Result<()> {
    let files = read_file_list(&md_src_path, &depth).unwrap();
    let vaild_files: Vec<PathBuf> = files
        .par_iter()
        .filter(|x| x.file_type().is_file())
        .filter_map(|x| {
            let p = PathBuf::from(x.path());
            if p.try_exists().unwrap_or(false) {
                if p.extension().unwrap() == "md" {
                    Some(p)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    let image_path_list: Vec<PathBuf> = vaild_files
        .par_iter()
        .filter_map(|current_mdfile_path| extract_image_paths_from_file(current_mdfile_path))
        .flatten()
        .collect();

    // 输出所有搜寻到的图像
    println!("{} files found", image_path_list.len());
    for item in &image_path_list {
        debug!("{:?}", item);
    }
    // 计算本地图片MD5值
    let local_img_list: Vec<(String, String)> = image_path_list
        .par_iter()
        .map(|img| {
            // MD5计算逻辑
            let mut md = md5::Md5::new();
            let mut img_content = vec![];
            let _ = std::fs::File::open(img)
                .unwrap()
                .read_to_end(&mut img_content);
            md.update(img_content);
            let md = md.finalize();
            (
                img.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap_or_default()
                    .to_string(),
                format!("{:x}", md),
            )
        })
        .collect();

    // 从云端拉取文件列表
    let list = list_cloud(op, "/", true).await?;
    let remote_img_list: Vec<(String, String)> = list
        .par_iter()
        .map(|entry| {
            // 打印云端文件列表
            info!(
                "filename: {} md5: {}",
                entry.name(),
                entry.metadata().content_md5().unwrap()
            );
            (
                entry.name().to_string(),
                entry.metadata().content_md5().unwrap().to_string(),
            )
        })
        .collect();

    // 比较云端文件和本地文件，找出需要上传到云端的文件
    let (uploadlist, deletelist, replacelist) = diff::diff(local_img_list, remote_img_list)?;

    Ok(())
}

/// 从单个Markdown文件中提取所有有效的图片路径
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
    let full_path = PathBuf::from(parent_dir).join(img_path);

    if full_path.try_exists().unwrap_or(false) {
        Some(full_path)
    } else {
        None
    }
}
