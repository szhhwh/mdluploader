use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version)]
#[command(about = "Mdparser")]
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

        /// Access key for the cloud storage
        #[arg(long, short)]
        ak: String,

        /// Secret key for the cloud storage
        #[arg(long, short)]
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
        #[arg(long, short = 'r')]
        remote_root: Option<String>,

        /// Set the maximum depth of the directory tree to traverse
        #[arg(long, default_value_t = 10)]
        depth: usize,
    },
}
