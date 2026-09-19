use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version)]
#[command(about = "A command line tool to upload markdown files to cloud storage.")]
pub struct Args {
    #[command(subcommand)]
    pub command: Commands,
}

/// Arguments for the `upload` subcommand.
#[derive(clap::Args, Debug)]
pub struct UploadArgs {
    /// Path to the folder contains markdown files
    #[arg(required = true)]
    pub path: PathBuf,

    /// Path to the folder contains markdown files
    #[arg(short, long)]
    pub bucket: String,

    /// Access key for the cloud storage (falls back to $MDLUPLOADER_AK)
    #[arg(short, long, env = "MDLUPLOADER_AK")]
    pub ak: String,

    /// Secret key for the cloud storage (falls back to $MDLUPLOADER_SK,
    /// preferred over passing it on the command line)
    #[arg(short, long, env = "MDLUPLOADER_SK")]
    pub sk: String,

    /// Region for the cloud storage
    #[arg(long, short = 'g')]
    pub region: String,

    /// Endpoint for the cloud storage
    #[arg(long, short)]
    pub endpoint: String,

    /// Domain for accessing the S3 bucket (used for link replacement)
    #[arg(long, short = 'd')]
    pub domain: String,

    /// Remote root path to upload
    #[arg(long, short)]
    pub remote_root: Option<String>,

    /// Set the maximum depth of the directory tree to traverse
    #[arg(long, default_value_t = 10)]
    pub depth: usize,

    /// Show what would be uploaded, deleted and replaced without
    /// transferring anything or rewriting links
    #[arg(long)]
    pub dry_run: bool,

    /// Cache-Control header set on uploaded objects
    /// (empty string leaves the header unset)
    #[arg(long, default_value = "public, max-age=86400")]
    pub cache_control: String,

    /// Maximum number of concurrent cloud transfers
    /// (default: CPU-based heuristic, clamped to 4..=32)
    #[arg(long)]
    pub concurrency: Option<usize>,
}

#[derive(Subcommand, Debug)]
// Flattening shrinks the variant to one `UploadArgs` field, but the struct is
// still held by value, so the size gap with `Completions` remains. The enum
// is built once per run and never hot-pathed; boxing would needlessly
// complicate every match site.
#[allow(clippy::large_enum_variant)]
pub enum Commands {
    /// Upload to cloud
    Upload(#[command(flatten)] UploadArgs),

    /// Generate a completion script for the given shell
    Completions {
        /// Shell to generate the completion script for
        #[arg(required = true)]
        shell: Shell,
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
    fn upload_args_parse_standalone() {
        // `UploadArgs` is flattened into the subcommand; augmenting a bare
        // command verifies it stays a self-contained `clap::Args` impl.
        use clap::{Args as _, FromArgMatches as _};

        let cmd = UploadArgs::augment_args(clap::Command::new("upload"));
        let matches = cmd
            .try_get_matches_from([
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
                "--depth",
                "2",
            ])
            .expect("valid upload arguments");
        let parsed = UploadArgs::from_arg_matches(&matches).expect("matches fit UploadArgs");

        assert_eq!(parsed.path, PathBuf::from("/md"));
        assert_eq!(parsed.bucket, "b");
        assert_eq!(parsed.ak, "a");
        assert_eq!(parsed.sk, "s");
        assert_eq!(parsed.region, "r");
        assert_eq!(parsed.endpoint, "https://e");
        assert_eq!(parsed.domain, "https://d");
        assert_eq!(parsed.remote_root, None);
        assert_eq!(parsed.depth, 2);
        assert!(!parsed.dry_run);
        assert_eq!(parsed.cache_control, "public, max-age=86400");
        assert_eq!(parsed.concurrency, None);
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
            "--cache-control",
            "public, max-age=3600",
        ]);

        let Commands::Upload(UploadArgs {
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
        }) = args.command
        else {
            panic!("expected upload command");
        };

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
        assert_eq!(cache_control, "public, max-age=3600");
        assert_eq!(concurrency, None);
    }

    #[test]
    fn cache_control_defaults_overrides_and_empties() {
        let base = [
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
        ];

        // Default keeps the moderate one-day policy (images are replaced
        // in place by cloud path, so the default must not be immutable).
        let args = Args::parse_from(base.to_vec());
        let Commands::Upload(UploadArgs { cache_control, .. }) = args.command else {
            panic!("expected upload command");
        };
        assert_eq!(cache_control, "public, max-age=86400");

        // Explicit override wins.
        let args = Args::parse_from({
            let mut v = base.to_vec();
            v.extend(["--cache-control", "public, max-age=604800"]);
            v
        });
        let Commands::Upload(UploadArgs { cache_control, .. }) = args.command else {
            panic!("expected upload command");
        };
        assert_eq!(cache_control, "public, max-age=604800");

        // An empty string is accepted and means "leave the header unset".
        let args = Args::parse_from({
            let mut v = base.to_vec();
            v.extend(["--cache-control", ""]);
            v
        });
        let Commands::Upload(UploadArgs { cache_control, .. }) = args.command else {
            panic!("expected upload command");
        };
        assert_eq!(cache_control, "");
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

        let Commands::Upload(UploadArgs {
            dry_run,
            concurrency,
            ..
        }) = args.command
        else {
            panic!("expected upload command");
        };
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

        let Commands::Upload(UploadArgs { ak, sk, .. }) = args.command else {
            panic!("expected upload command");
        };
        assert_eq!(ak, "env-ak");
        assert_eq!(sk, "env-sk");

        std::env::remove_var("MDLUPLOADER_AK");
        std::env::remove_var("MDLUPLOADER_SK");
    }

    #[test]
    fn parses_completions_command() {
        let args = Args::parse_from(["mdluploader", "completions", "bash"]);
        let Commands::Completions { shell } = args.command else {
            panic!("expected completions command");
        };
        assert_eq!(shell, Shell::Bash);
    }

    #[test]
    fn completions_script_is_generated_without_panic() {
        let mut buf: Vec<u8> = Vec::new();
        clap_complete::generate(Shell::Bash, &mut Args::command(), "mdluploader", &mut buf);
        assert!(!buf.is_empty());
    }
}
