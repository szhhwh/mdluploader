use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version)]
#[command(about = "A command line tool to upload markdown files to cloud storage.")]
pub struct Args {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Upload to cloud
    Upload {
        /// Path to the folder contains markdown files
        #[arg(required = true)]
        path: PathBuf,

        /// Path to the folder contains markdown files
        #[arg(short, long)]
        bucket: String,

        /// Access key for the cloud storage (falls back to $MDLUPLOADER_AK)
        #[arg(short, long, env = "MDLUPLOADER_AK")]
        ak: String,

        /// Secret key for the cloud storage (falls back to $MDLUPLOADER_SK,
        /// preferred over passing it on the command line)
        #[arg(short, long, env = "MDLUPLOADER_SK")]
        sk: String,

        /// Region for the cloud storage
        #[arg(long, short = 'g')]
        region: String,

        /// Endpoint for the cloud storage
        #[arg(long, short)]
        endpoint: String,

        /// Domain for accessing the S3 bucket (used for link replacement)
        #[arg(long, short = 'd')]
        domain: String,

        /// Remote root path to upload
        #[arg(long, short)]
        remote_root: Option<String>,

        /// Set the maximum depth of the directory tree to traverse
        #[arg(long, default_value_t = 10)]
        depth: usize,

        /// Show what would be uploaded, deleted and replaced without
        /// transferring anything or rewriting links
        #[arg(long)]
        dry_run: bool,

        /// Maximum number of concurrent cloud transfers
        /// (default: CPU-based heuristic, clamped to 4..=32)
        #[arg(long)]
        concurrency: Option<usize>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn parses_full_upload_command() {
        let args = Args::parse_from([
            "mdluploader",
            "upload",
            "/path/to/md",
            "--bucket",
            "my-bucket",
            "--ak",
            "AKIA",
            "--sk",
            "SECRET",
            "--region",
            "us-west-1",
            "--endpoint",
            "https://s3.us-west-1.amazonaws.com",
            "--domain",
            "https://cdn.example.com",
            "--remote-root",
            "imgs",
            "--depth",
            "3",
        ]);

        let Commands::Upload {
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
            concurrency,
        } = args.command;

        assert_eq!(path, PathBuf::from("/path/to/md"));
        assert_eq!(depth, 3);
        assert_eq!(bucket, "my-bucket");
        assert_eq!(ak, "AKIA");
        assert_eq!(sk, "SECRET");
        assert_eq!(region, "us-west-1");
        assert_eq!(endpoint, "https://s3.us-west-1.amazonaws.com");
        assert_eq!(domain, "https://cdn.example.com");
        assert_eq!(remote_root.as_deref(), Some("imgs"));
        assert!(!dry_run);
        assert_eq!(concurrency, None);
    }

    #[test]
    fn parses_dry_run_and_concurrency() {
        let args = Args::parse_from([
            "mdluploader",
            "upload",
            "/md",
            "-b",
            "b",
            "-a",
            "a",
            "-s",
            "s",
            "-g",
            "r",
            "-e",
            "https://e",
            "-d",
            "https://d",
            "--dry-run",
            "--concurrency",
            "7",
        ]);

        let Commands::Upload {
            dry_run,
            concurrency,
            ..
        } = args.command;
        assert!(dry_run);
        assert_eq!(concurrency, Some(7));
    }

    #[test]
    fn sk_and_ak_fall_back_to_env_vars() {
        // Credentials on the command line leak into shell history and process
        // listings; the env fallback must make the flags optional.
        std::env::set_var("MDLUPLOADER_AK", "env-ak");
        std::env::set_var("MDLUPLOADER_SK", "env-sk");

        let args = Args::parse_from([
            "mdluploader",
            "upload",
            "/md",
            "-b",
            "b",
            "-g",
            "r",
            "-e",
            "https://e",
            "-d",
            "https://d",
        ]);

        let Commands::Upload { ak, sk, .. } = args.command;
        assert_eq!(ak, "env-ak");
        assert_eq!(sk, "env-sk");

        std::env::remove_var("MDLUPLOADER_AK");
        std::env::remove_var("MDLUPLOADER_SK");
    }
}
