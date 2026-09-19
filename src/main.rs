use anyhow::Result;
use clap::{CommandFactory, Parser};
use log::info;
use mdluploader::cli;
use mdluploader::pipeline::{self, PipelineConfig};
use mdluploader::uploader::s3::AwsS3;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    let args = cli::Args::parse();

    info!("Welcome to Markdown Image Uploader!");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!(
        "Build time: {}",
        option_env!("VERGEN_BUILD_TIMESTAMP").unwrap_or("Unknown Build Time")
    );

    match args.command {
        cli::Commands::Upload(cli::UploadArgs {
            path,
            depth,
            bucket,
            ak,
            sk,
            region,
            endpoint,
            domain,
            remote_root,
            dry_run,
            cache_control,
            concurrency,
        }) => {
            let remote_root = remote_root.unwrap_or_else(|| "/".to_string());
            let op = AwsS3::new(bucket, ak, sk, region, endpoint, remote_root.clone()).build()?;
            let config = PipelineConfig {
                src: path,
                depth,
                domain,
                remote_root,
                dry_run,
                cache_control,
                concurrency,
            };
            pipeline::run(op, config).await?;
        }
        cli::Commands::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut cli::Args::command(),
                "mdluploader",
                &mut std::io::stdout(),
            );
        }
    }

    Ok(())
}
