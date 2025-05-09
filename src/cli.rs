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

        #[arg(short, long)]
        bucket: String,

        #[arg(long)]
        ak: String,

        #[arg(long)]
        sk: String,

        #[arg(long)]
        region: String,

        #[arg(long)]
        endpoint: String,

        /// Set the maximum depth of the directory tree to traverse
        /// Default is 10
        #[arg(short, long, default_value_t = 10)]
        depth: usize,
    },
}
