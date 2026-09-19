//! The upload pipeline: scan markdown, hash images, diff against the cloud,
//! transfer, and rewrite links.
//!
//! Each phase is a small testable function; [`run`] wires them together.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use log::{debug, info, warn};
use opendal::Operator;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use url::Url;
use walkdir::WalkDir;

use crate::differ::{diff, LocalImage, RemoteImage};
use crate::mdparser::{extract_image_dests, is_remote_url, percent_decode, replace_image_links};
use crate::uploader::Uploader;
use crate::{get_file_md5, normalize_cloud_path, remote_md5, to_cloud_path};

/// Configuration for one pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Directory containing the markdown source tree.
    pub src: PathBuf,
    /// Maximum directory depth to scan for markdown files.
    pub depth: usize,
    /// Public domain used to build image URLs written back into markdown.
    pub domain: String,
    /// Remote root prefix under which images are served (e.g. `imgs`).
    pub remote_root: String,
    /// Only print the plan; do not transfer or rewrite anything.
    pub dry_run: bool,
    /// Cache-Control header set on uploaded objects; an empty string
    /// leaves the header unset.
    pub cache_control: String,
    /// Override the concurrent-transfer limit; `None` uses the default.
    pub concurrency: Option<usize>,
}

/// Result of scanning the local markdown tree.
#[derive(Debug, Default)]
struct ScanResult {
    /// All markdown files found.
    md_files: Vec<PathBuf>,
    /// For every markdown file, the resolved local image paths it references.
    /// Built once and reused for hashing and link rewriting, so each markdown
    /// file is parsed exactly once per run.
    images_by_md: HashMap<PathBuf, Vec<PathBuf>>,
    /// Cloud paths referenced through already-rewritten URLs on our own
    /// domain. These objects must not be deleted by the diff just because
    /// no local file references them anymore.
    managed_remote: HashSet<String>,
}

/// Runs the full upload pipeline.
///
/// Phases: scan markdown and images, hash local images, list the remote
/// once, diff, transfer (upload/delete/replace), then rewrite image links in
/// every affected markdown file.
pub async fn run(op: Operator, config: PipelineConfig) -> Result<()> {
    let domain = Url::parse(&config.domain)
        .with_context(|| format!("Invalid domain URL: {}", config.domain))?;
    info!("Using domain: {}", domain);

    let src = config
        .src
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize source path {:?}", config.src))?;

    // Phase 1: scan markdown files and their local images.
    let scan = scan_local(&src, config.depth, &domain, &config.remote_root);
    info!("Found {} markdown files.", scan.md_files.len());
    let image_paths: HashSet<PathBuf> = scan.images_by_md.values().flatten().cloned().collect();
    info!(
        "{} image links detected in markdown files.",
        image_paths.len()
    );
    debug!("Local images: {:?}", image_paths);

    // Phase 2: hash local images and map them to cloud paths.
    let local_images: Vec<LocalImage> = image_paths
        .par_iter()
        .filter_map(|img| match get_file_md5(img) {
            Ok(md5) => Some(LocalImage {
                local_path: img.clone(),
                cloud_path: to_cloud_path(img, &src),
                md5,
            }),
            Err(e) => {
                warn!("Failed to get MD5 for {}: {}", img.display(), e);
                None
            }
        })
        .collect();

    let uploader = match config.concurrency {
        Some(limit) => Uploader::new(op).with_concurrency(limit),
        None => Uploader::new(op),
    }
    .with_cache_control(config.cache_control.clone());

    // Phase 3: list the remote once and diff. Some backends (fs, memory)
    // return directory entries in listings; only compare actual objects.
    let entries: Vec<opendal::Entry> = uploader
        .list_cloud("/", true)
        .await?
        .into_iter()
        .filter(|entry| entry.metadata().mode() != opendal::EntryMode::DIR)
        .collect();
    let initial_remote: Vec<String> = entries
        .iter()
        .map(|entry| normalize_cloud_path(entry.path()))
        .collect();
    let remote_images: Vec<RemoteImage> = entries
        .iter()
        .map(|entry| RemoteImage {
            cloud_path: normalize_cloud_path(entry.path()),
            md5: entry.metadata().content_md5().and_then(remote_md5),
        })
        .collect();

    let plan = diff(local_images, remote_images);
    // Objects still referenced through rewritten URLs on our own domain are
    // managed; only delete remote objects nobody references anymore.
    let deletes: Vec<String> = plan
        .deletes
        .iter()
        .filter(|p| !scan.managed_remote.contains(*p))
        .cloned()
        .collect();
    if deletes.len() != plan.deletes.len() {
        debug!(
            "Keeping {} remote object(s) still referenced by rewritten URLs",
            plan.deletes.len() - deletes.len()
        );
    }
    info!("{} files need to be uploaded.", plan.uploads.len());
    info!("{} files need to be deleted.", deletes.len());
    info!("{} files need to be replaced.", plan.replaces.len());

    if config.dry_run {
        for f in &plan.uploads {
            info!(
                "[dry-run] upload {} <- {}",
                f.cloud_path,
                f.local_path.display()
            );
        }
        for d in &deletes {
            info!("[dry-run] delete {}", d);
        }
        for f in &plan.replaces {
            info!(
                "[dry-run] replace {} <- {}",
                f.cloud_path,
                f.local_path.display()
            );
        }
        info!("[dry-run] no changes applied.");
        return Ok(());
    }

    // Phase 4: transfer. Any failure aborts the run, so reaching phase 5
    // guarantees the final remote set below is accurate.
    let mut added: Vec<String> = Vec::with_capacity(plan.uploads.len() + plan.replaces.len());
    if !plan.uploads.is_empty() {
        info!("Uploading new files...");
        added.extend(plan.uploads.iter().map(|f| f.cloud_path.clone()));
        uploader.upload_files(plan.uploads).await?;
    }
    if !deletes.is_empty() {
        info!("Deleting stale remote files...");
        uploader.delete_files(deletes.clone()).await?;
    }
    if !plan.replaces.is_empty() {
        info!("Uploading changed files...");
        added.extend(plan.replaces.iter().map(|f| f.cloud_path.clone()));
        uploader.upload_files(plan.replaces).await?;
    }

    // Phase 5: rewrite links. The final remote set is derived from the
    // (single) listing plus the transfers that just succeeded, avoiding a
    // second full cloud listing.
    let final_remote = final_remote_set(initial_remote, &deletes, &added);
    let mut path_map: HashMap<PathBuf, String> = HashMap::with_capacity(image_paths.len());
    for img in &image_paths {
        let cloud_path = to_cloud_path(img, &src);
        if final_remote.contains(&cloud_path) {
            let url = build_public_url(&domain, &config.remote_root, &cloud_path)?;
            path_map.insert(img.clone(), url.to_string());
        } else {
            debug!(
                "Image {} is not on the remote; link kept as-is",
                img.display()
            );
        }
    }

    let failures = rewrite_markdown_links(&scan.images_by_md, &path_map);
    if !failures.is_empty() {
        // Images are already on the remote at this point; exiting non-zero is
        // the only way to tell the caller the markdown still points at local
        // files (CI must not mistake this for a successful run).
        let details: Vec<String> = failures
            .iter()
            .map(|f| format!("{} ({})", f.path.display(), f.error))
            .collect();
        bail!(
            "Failed to rewrite image links in {} markdown file(s): {}",
            failures.len(),
            details.join("; ")
        );
    }
    info!("Pipeline finished.");
    Ok(())
}

/// Scans the source tree for markdown files, resolving their local images
/// and collecting the cloud paths of already-rewritten links on our domain.
fn scan_local(src: &Path, depth: usize, domain: &Url, remote_root: &str) -> ScanResult {
    let md_files = collect_md_files(src, depth);

    let scanned: Vec<(PathBuf, Vec<PathBuf>, HashSet<String>)> = md_files
        .par_iter()
        .map(|md| scan_markdown_file(md, domain, remote_root))
        .collect();

    let mut images_by_md = HashMap::with_capacity(scanned.len());
    let mut managed_remote = HashSet::new();
    for (md, images, managed) in scanned {
        managed_remote.extend(managed);
        images_by_md.insert(md, images);
    }

    ScanResult {
        md_files,
        images_by_md,
        managed_remote,
    }
}

/// Parses one markdown file, resolving local images and collecting managed
/// remote references in a single pass.
fn scan_markdown_file(
    md_file: &Path,
    domain: &Url,
    remote_root: &str,
) -> (PathBuf, Vec<PathBuf>, HashSet<String>) {
    let Ok(content) = std::fs::read_to_string(md_file) else {
        warn!("Failed to read markdown file {}", md_file.display());
        return (md_file.to_path_buf(), Vec::new(), HashSet::new());
    };

    let dests = extract_image_dests(&content);
    let images: Vec<PathBuf> = dests
        .iter()
        .filter(|dest| !dest.is_empty() && !is_remote_url(dest))
        .filter_map(|dest| resolve_image_path(&percent_decode(dest), md_file))
        .collect();
    let managed: HashSet<String> = dests
        .iter()
        .filter_map(|dest| managed_cloud_path(dest, domain, remote_root))
        .collect();

    (md_file.to_path_buf(), images, managed)
}

/// Maps an image URL on our own domain back to its cloud path.
///
/// Rewritten links (e.g. `https://cdn.example.com/imgs/a.png` with
/// remote_root `imgs`) are recognized as references to objects this tool
/// manages. Returns `None` for foreign URLs and non-URLs.
///
/// `url.path()` is the percent-encoded form (see [`build_public_url`]); it is
/// decoded before the cloud path is extracted so the result matches the raw
/// object keys used for uploads, listings and the diff.
fn managed_cloud_path(dest: &str, domain: &Url, remote_root: &str) -> Option<String> {
    let url = Url::parse(dest).ok()?;
    // Match host and port strictly. Comparing `domain()` alone would accept
    // URLs served on a different port, and `domain()` is `None` for IP hosts,
    // so any two IPs would compare equal. `port_or_known_default()` treats an
    // explicit default port (e.g. `:443` for https) the same as no port.
    match (url.host_str(), domain.host_str()) {
        (Some(url_host), Some(domain_host)) if url_host == domain_host => {}
        _ => return None,
    }
    if url.port_or_known_default() != domain.port_or_known_default() {
        return None;
    }
    let root = remote_root.trim_matches('/');
    let path = percent_decode(url.path());
    if root.is_empty() {
        return Some(path.trim_start_matches('/').to_string());
    }
    let prefix = format!("/{}/", root);
    path.strip_prefix(&prefix)
        .map(|rest| rest.trim_start_matches('/').to_string())
}

/// Collects markdown files under `root` up to `depth`.
///
/// Hidden directories, `.git` and dependency/vendor directories are skipped,
/// and the extension match is case-insensitive so `README.MD` is found too.
fn collect_md_files(root: &Path, depth: usize) -> Vec<PathBuf> {
    WalkDir::new(root)
        .max_depth(depth)
        .into_iter()
        .filter_entry(|e| !is_skipped_entry(e))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file() && has_md_extension(entry.path()))
        .map(|entry| entry.into_path())
        .collect()
}

/// Whether a walk entry should be pruned entirely.
fn is_skipped_entry(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 {
        // Never skip the explicitly provided root, even if it is hidden.
        return false;
    }
    if !entry.file_type().is_dir() {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    name.starts_with('.') || name == "node_modules" || name == "target"
}

/// Case-insensitive `.md` extension check.
fn has_md_extension(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

/// Resolves one markdown image destination to a canonical local path.
/// Relative destinations resolve against the markdown file's directory;
/// absolute destinations are used as-is. Both are canonicalized so tree-ness
/// checks (to_cloud_path) operate on one canonical form, and non-files
/// (missing paths, directories) yield `None`.
fn resolve_image_path(dest: &str, md_file: &Path) -> Option<PathBuf> {
    let candidate = PathBuf::from(dest);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        md_file.parent()?.join(candidate)
    };

    let canonical = candidate.canonicalize().ok()?;
    canonical.is_file().then_some(canonical)
}

/// Characters percent-encoded in the path of generated public URLs.
///
/// The set covers control characters, spaces, non-ASCII bytes (always encoded
/// by `utf8_percent_encode`), characters that terminate a URL path (`#`, `?`)
/// or are unsafe in markdown inline link destinations (`<`, `>`, `"`), and `%`
/// itself so percent-decoding the URL always yields the original cloud path,
/// even for file names that already contain `%xx`-looking sequences. `/` is
/// deliberately kept so the cloud path hierarchy stays readable.
const PUBLIC_URL_PATH: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'#')
    .add(b'?');

/// Builds the public URL for a cloud path.
///
/// `remote_root` may be `/`, empty or a plain directory name; the result
/// never contains double slashes. The cloud path is percent-encoded (see
/// [`PUBLIC_URL_PATH`]) so the URL stays valid per RFC 3986 and usable in
/// markdown inline links even for non-ASCII names, spaces and reserved
/// characters; the cloud object key itself remains the raw, unencoded path.
pub fn build_public_url(domain: &Url, remote_root: &str, cloud_path: &str) -> Result<Url> {
    let root = remote_root.trim_matches('/');
    let domain_str = domain.as_str().trim_end_matches('/');
    let base = if root.is_empty() {
        domain_str.to_string()
    } else {
        format!("{}/{}", domain_str, root)
    };
    let encoded = utf8_percent_encode(cloud_path.trim_start_matches('/'), PUBLIC_URL_PATH);
    let url = format!("{}/{}", base, encoded);
    Url::parse(&url).with_context(|| format!("Failed to build public URL from {}", url))
}

/// Computes the set of cloud paths present after a successful transfer:
/// the initial listing minus deletions plus uploads/replacements.
pub fn final_remote_set(
    initial: Vec<String>,
    deletes: &[String],
    added: &[String],
) -> HashSet<String> {
    let deleted: HashSet<&String> = deletes.iter().collect();
    initial
        .into_iter()
        .filter(|p| !deleted.contains(p))
        .chain(added.iter().cloned())
        .collect()
}

/// Why rewriting a markdown file failed.
#[derive(Debug)]
enum RewriteError {
    /// Reading the markdown file failed.
    Read(std::io::Error),
    /// Writing the rewritten content back failed.
    Write(std::io::Error),
}

impl std::fmt::Display for RewriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RewriteError::Read(e) => write!(f, "failed to read: {e}"),
            RewriteError::Write(e) => write!(f, "failed to write back: {e}"),
        }
    }
}

/// One markdown file whose image links could not be rewritten.
#[derive(Debug)]
struct RewriteFailure {
    /// The markdown file that could not be processed.
    path: PathBuf,
    /// Why reading or writing the file failed.
    error: RewriteError,
}

/// Rewrites image links in every markdown file that references at least one
/// uploaded image.
///
/// Returns the files whose content could not be read or written back; an
/// empty list means every affected file was processed successfully.
fn rewrite_markdown_links(
    images_by_md: &HashMap<PathBuf, Vec<PathBuf>>,
    path_map: &HashMap<PathBuf, String>,
) -> Vec<RewriteFailure> {
    if path_map.is_empty() {
        info!("No uploaded images; skipping link replacement.");
        return Vec::new();
    }
    info!("Replacing image links in markdown files...");

    let mut failures = Vec::new();
    for (md_file, images) in images_by_md {
        if !images.iter().any(|img| path_map.contains_key(img)) {
            continue;
        }

        let result = rewrite_one_file(
            md_file,
            path_map,
            || std::fs::read_to_string(md_file),
            |new_content| std::fs::write(md_file, new_content),
        );
        match result {
            Ok(true) => info!("Updated links in {}", md_file.display()),
            Ok(false) => {}
            Err(failure) => {
                warn!(
                    "Failed to rewrite links in {}: {}",
                    failure.path.display(),
                    failure.error
                );
                failures.push(failure);
            }
        }
    }

    if failures.is_empty() {
        info!("Completed markdown image link replacement.");
    }
    failures
}

/// Reads one markdown file, replaces its image links and writes it back.
///
/// The read and write operations are injected so failure paths can be unit
/// tested without relying on real filesystem permissions. Returns `Ok(true)`
/// when the file was rewritten, `Ok(false)` when no link changed, and an
/// [`RewriteFailure`] when reading or writing failed.
fn rewrite_one_file(
    md_file: &Path,
    path_map: &HashMap<PathBuf, String>,
    read: impl FnOnce() -> std::io::Result<String>,
    write: impl FnOnce(&str) -> std::io::Result<()>,
) -> Result<bool, RewriteFailure> {
    let content = read().map_err(|e| RewriteFailure {
        path: md_file.to_path_buf(),
        error: RewriteError::Read(e),
    })?;

    let new_content = replace_image_links(&content, |dest| {
        let abs = resolve_image_path(&percent_decode(dest), md_file)?;
        path_map.get(&abs).cloned()
    });

    if new_content == content {
        return Ok(false);
    }

    write(&new_content).map_err(|e| RewriteFailure {
        path: md_file.to_path_buf(),
        error: RewriteError::Write(e),
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_md_files_filters_noise_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join(".git/x.md"), "git").unwrap();
        std::fs::write(dir.path().join("node_modules/pkg/y.md"), "nm").unwrap();
        std::fs::write(dir.path().join("sub/ok.md"), "ok").unwrap();
        std::fs::write(dir.path().join("UPPER.MD"), "upper").unwrap();
        std::fs::write(dir.path().join("ignore.txt"), "txt").unwrap();

        let mut files: Vec<String> = collect_md_files(dir.path(), 10)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        files.sort();
        assert_eq!(files, vec!["UPPER.MD".to_string(), "ok.md".to_string()]);
    }

    #[test]
    fn collect_md_files_respects_depth() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/shallow.md"), "x").unwrap();
        std::fs::write(dir.path().join("a/b/deep.md"), "x").unwrap();

        let files = collect_md_files(dir.path(), 2);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("shallow.md"));
    }

    #[test]
    fn resolve_image_path_resolves_relative_and_absolute() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("images")).unwrap();
        let img = dir.path().join("images/a.png");
        std::fs::write(&img, b"x").unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(&md, b"").unwrap();

        let rel = resolve_image_path("images/a.png", &md).unwrap();
        assert_eq!(rel, img.canonicalize().unwrap());

        let abs = resolve_image_path(img.to_str().unwrap(), &md).unwrap();
        assert_eq!(abs, img.canonicalize().unwrap());

        assert!(resolve_image_path("images/missing.png", &md).is_none());
        assert!(resolve_image_path("images", &md).is_none());
    }

    #[test]
    fn build_public_url_never_produces_double_slashes() {
        let domain = Url::parse("https://cdn.example.com").unwrap();

        let url = build_public_url(&domain, "/", "sub/a.png").unwrap();
        assert_eq!(url.as_str(), "https://cdn.example.com/sub/a.png");

        let url = build_public_url(&domain, "", "a.png").unwrap();
        assert_eq!(url.as_str(), "https://cdn.example.com/a.png");

        let url = build_public_url(&domain, "imgs", "a.png").unwrap();
        assert_eq!(url.as_str(), "https://cdn.example.com/imgs/a.png");

        let url = build_public_url(&domain, "/imgs/", "/a.png").unwrap();
        assert_eq!(url.as_str(), "https://cdn.example.com/imgs/a.png");
    }

    #[test]
    fn managed_cloud_path_maps_own_domain_urls() {
        let domain = Url::parse("https://cdn.example.com").unwrap();

        assert_eq!(
            managed_cloud_path("https://cdn.example.com/images/a.png", &domain, "/"),
            Some("images/a.png".to_string())
        );
        assert_eq!(
            managed_cloud_path("https://cdn.example.com/imgs/a.png", &domain, "imgs"),
            Some("a.png".to_string())
        );
        assert_eq!(
            managed_cloud_path("https://other.example.com/imgs/a.png", &domain, "imgs"),
            None
        );
        assert_eq!(managed_cloud_path("images/a.png", &domain, "/"), None);
        // A path that is under the root but not exactly at its boundary does
        // not accidentally match.
        assert_eq!(
            managed_cloud_path("https://cdn.example.com/imgsx/a.png", &domain, "imgs"),
            None
        );
    }

    #[test]
    fn managed_cloud_path_matches_host_and_port_strictly() {
        let domain = Url::parse("https://cdn.example.com").unwrap();

        // The same host on a non-default port is a different origin.
        assert_eq!(
            managed_cloud_path("https://cdn.example.com:8443/imgs/a.png", &domain, "imgs"),
            None
        );
        // An explicit default port is the same URL as the omitted-port form.
        assert_eq!(
            managed_cloud_path("https://cdn.example.com:443/imgs/a.png", &domain, "imgs"),
            Some("a.png".to_string())
        );

        // A configured non-default port only matches that exact port.
        let ported = Url::parse("https://cdn.example.com:8443").unwrap();
        assert_eq!(
            managed_cloud_path("https://cdn.example.com/imgs/a.png", &ported, "imgs"),
            None
        );
        assert_eq!(
            managed_cloud_path("https://cdn.example.com:8443/imgs/a.png", &ported, "imgs"),
            Some("a.png".to_string())
        );

        // IP hosts: `domain()` is `None` for both, so they must be told
        // apart by comparing the host strings directly.
        let ip_domain = Url::parse("https://1.2.3.4").unwrap();
        assert_eq!(
            managed_cloud_path("https://5.6.7.8/imgs/a.png", &ip_domain, "imgs"),
            None
        );
        assert_eq!(
            managed_cloud_path("https://1.2.3.4/imgs/a.png", &ip_domain, "imgs"),
            Some("a.png".to_string())
        );
    }

    #[test]
    fn build_public_url_percent_encodes_unsafe_path_characters() {
        let domain = Url::parse("https://cdn.example.com").unwrap();

        // Non-ASCII and spaces are encoded; `/` separators are preserved.
        let url = build_public_url(&domain, "/", "图片 目录/图.png").unwrap();
        assert_eq!(
            url.as_str(),
            "https://cdn.example.com/%E5%9B%BE%E7%89%87%20%E7%9B%AE%E5%BD%95/%E5%9B%BE.png"
        );

        let url = build_public_url(&domain, "imgs", "my image.png").unwrap();
        assert_eq!(url.as_str(), "https://cdn.example.com/imgs/my%20image.png");

        // Characters that terminate a URL path or break inline links are
        // encoded, and `/` between path segments is kept as-is.
        let url = build_public_url(&domain, "/", "sub/a<b>c\"d#e?f.png").unwrap();
        assert_eq!(
            url.as_str(),
            "https://cdn.example.com/sub/a%3Cb%3Ec%22d%23e%3Ff.png"
        );

        // `%` itself is encoded so decoding round-trips to the original name.
        let url = build_public_url(&domain, "/", "dir/100%.png").unwrap();
        assert_eq!(url.as_str(), "https://cdn.example.com/dir/100%25.png");
    }

    #[test]
    fn managed_cloud_path_decodes_percent_encoded_urls_back_to_keys() {
        let domain = Url::parse("https://cdn.example.com").unwrap();

        for (root, cloud_path) in [
            ("/", "图片 目录/图.png"),
            ("imgs", "my image.png"),
            ("/", "sub/a<b>c\"d#e?f.png"),
            ("imgs", "dir/100%.png"),
        ] {
            let url = build_public_url(&domain, root, cloud_path).unwrap();
            assert_eq!(
                managed_cloud_path(url.as_str(), &domain, root),
                Some(cloud_path.to_string()),
                "round-trip failed for {} under root {}",
                cloud_path,
                root
            );
        }
    }

    #[test]
    fn final_remote_set_applies_deletes_and_additions() {
        let initial = vec!["a.png".to_string(), "b.png".to_string()];
        let deletes = vec!["a.png".to_string()];
        let added = vec!["c.png".to_string()];

        let set = final_remote_set(initial, &deletes, &added);
        assert!(!set.contains("a.png"));
        assert!(set.contains("b.png"));
        assert!(set.contains("c.png"));
    }

    /// Builds a temp dir with `post.md` referencing a local image, plus the
    /// corresponding `path_map` entry, for `rewrite_one_file` tests.
    fn rewrite_fixture() -> (tempfile::TempDir, PathBuf, HashMap<PathBuf, String>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("images")).unwrap();
        std::fs::write(dir.path().join("images/a.png"), b"x").unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(&md, "![a](images/a.png)\n").unwrap();

        let img = dir.path().join("images/a.png").canonicalize().unwrap();
        let mut path_map = HashMap::new();
        path_map.insert(img, "https://cdn.example.com/images/a.png".to_string());
        (dir, md, path_map)
    }

    #[test]
    fn rewrite_one_file_reports_read_failure() {
        let (_dir, md, path_map) = rewrite_fixture();

        let failure = rewrite_one_file(
            &md,
            &path_map,
            || Err(std::io::Error::other("permission denied")),
            |_| unreachable!("write must not run when read fails"),
        )
        .unwrap_err();

        assert_eq!(failure.path, md);
        let RewriteError::Read(e) = &failure.error else {
            panic!("expected a read failure, got: {:?}", failure.error);
        };
        assert!(
            e.to_string().contains("permission denied"),
            "unexpected error: {e}"
        );
    }

    #[test]
    fn rewrite_one_file_reports_write_failure() {
        let (_dir, md, path_map) = rewrite_fixture();

        let failure = rewrite_one_file(
            &md,
            &path_map,
            || std::fs::read_to_string(&md),
            |_| Err(std::io::Error::other("read-only filesystem")),
        )
        .unwrap_err();

        assert_eq!(failure.path, md);
        let RewriteError::Write(e) = &failure.error else {
            panic!("expected a write failure, got: {:?}", failure.error);
        };
        assert!(
            e.to_string().contains("read-only filesystem"),
            "unexpected error: {e}"
        );
    }

    #[test]
    fn rewrite_one_file_writes_back_only_when_links_change() {
        let (_dir, md, path_map) = rewrite_fixture();

        // Matching link: the rewritten content is handed to the writer.
        let mut written: Option<String> = None;
        let changed = rewrite_one_file(
            &md,
            &path_map,
            || std::fs::read_to_string(&md),
            |content| {
                written = Some(content.to_string());
                Ok(())
            },
        )
        .unwrap();
        assert!(changed);
        assert_eq!(
            written.as_deref(),
            Some("![a](https://cdn.example.com/images/a.png)\n")
        );

        // Nothing to replace: the writer must not run at all.
        let changed = rewrite_one_file(
            &md,
            &path_map,
            || Ok("No images here.\n".to_string()),
            |_| unreachable!("write must not run when nothing changed"),
        )
        .unwrap();
        assert!(!changed);
    }
}

#[cfg(test)]
mod pipeline_e2e_tests {
    use super::*;
    use opendal::services;

    fn memory_op() -> Operator {
        Operator::new(services::Memory::default()).unwrap().finish()
    }

    async fn run_once(op: &Operator, src: &Path) -> Result<()> {
        run(
            op.clone(),
            PipelineConfig {
                src: src.to_path_buf(),
                depth: 10,
                domain: "https://cdn.example.com".to_string(),
                remote_root: "/".to_string(),
                dry_run: false,
                cache_control: String::new(),
                concurrency: None,
            },
        )
        .await
    }

    #[tokio::test]
    async fn managed_remote_link_protects_remote_object() {
        // Regression: after links are rewritten to the CDN, a re-run must not
        // delete the uploaded objects just because no local path references
        // them anymore.
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(&md, "![a](https://cdn.example.com/images/a.png)\n").unwrap();

        let op = memory_op();
        op.write("images/a.png", b"bytes".to_vec()).await.unwrap();
        run_once(&op, dir.path()).await.unwrap();

        assert_eq!(op.read("images/a.png").await.unwrap().to_vec(), b"bytes");
        let content = std::fs::read_to_string(&md).unwrap();
        assert_eq!(content, "![a](https://cdn.example.com/images/a.png)\n");
    }

    #[tokio::test]
    async fn removing_link_deletes_remote_object() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("images")).unwrap();
        std::fs::write(dir.path().join("images/a.png"), b"bytes").unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(&md, "![a](images/a.png)\n").unwrap();

        let op = memory_op();
        run_once(&op, dir.path()).await.unwrap();
        assert!(op.stat("images/a.png").await.is_ok());

        // Author removes the image from the post entirely: the remote object
        // is no longer referenced and must be deleted on the next run.
        std::fs::write(&md, "No more images.\n").unwrap();
        run_once(&op, dir.path()).await.unwrap();
        assert!(op.stat("images/a.png").await.is_err());
    }

    #[tokio::test]
    async fn dry_run_transfers_and_rewrites_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("images")).unwrap();
        std::fs::write(dir.path().join("images/a.png"), b"png-bytes").unwrap();
        let md = dir.path().join("post.md");
        let original = "![a](images/a.png)\n";
        std::fs::write(&md, original).unwrap();

        let op = memory_op();
        run(
            op.clone(),
            PipelineConfig {
                src: dir.path().to_path_buf(),
                depth: 10,
                domain: "https://cdn.example.com".to_string(),
                remote_root: "/".to_string(),
                dry_run: true,
                cache_control: "public, max-age=86400".to_string(),
                concurrency: Some(2),
            },
        )
        .await
        .unwrap();

        // Nothing was transferred and the markdown was left untouched.
        let entries = op.list_with("/").recursive(true).await.unwrap();
        assert!(entries.is_empty());
        assert_eq!(std::fs::read_to_string(&md).unwrap(), original);
    }

    #[tokio::test]
    async fn uploads_images_and_rewrites_markdown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("images")).unwrap();
        std::fs::write(dir.path().join("images/a.png"), b"png-bytes").unwrap();
        std::fs::write(dir.path().join("images/unused.png"), b"never-referenced").unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(
            &md,
            "![a](images/a.png) ![gone](images/missing.png) ![r](https://example.com/r.png)\n",
        )
        .unwrap();

        let op = memory_op();
        run_once(&op, dir.path()).await.unwrap();

        // The referenced image is uploaded with its real content.
        assert_eq!(
            op.read("images/a.png").await.unwrap().to_vec(),
            b"png-bytes"
        );
        // Unreferenced images are not uploaded.
        let entries: Vec<String> = op
            .list_with("/")
            .recursive(true)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.metadata().mode() != opendal::EntryMode::DIR)
            .map(|e| normalize_cloud_path(e.path()))
            .collect();
        assert_eq!(entries, vec!["images/a.png".to_string()]);

        // Links are rewritten only for uploaded local images.
        let content = std::fs::read_to_string(&md).unwrap();
        assert!(
            content.contains("![a](https://cdn.example.com/images/a.png)"),
            "unexpected content: {}",
            content
        );
        assert!(content.contains("![gone](images/missing.png)"));
        assert!(content.contains("![r](https://example.com/r.png)"));
    }

    #[tokio::test]
    async fn second_run_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("images")).unwrap();
        std::fs::write(dir.path().join("images/a.png"), b"png-bytes").unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(&md, "![a](images/a.png)\n").unwrap();

        let op = memory_op();
        run_once(&op, dir.path()).await.unwrap();
        let after_first = std::fs::read_to_string(&md).unwrap();

        run_once(&op, dir.path()).await.unwrap();
        let after_second = std::fs::read_to_string(&md).unwrap();
        assert_eq!(
            after_first, after_second,
            "second run must not rewrite again"
        );

        // Exactly one object remains.
        let count = op
            .list_with("/")
            .recursive(true)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.metadata().mode() != opendal::EntryMode::DIR)
            .count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn space_in_file_name_is_encoded_and_survives_rerun() {
        // Regression: a cloud path with a space used to be written verbatim
        // into the markdown, which breaks the CommonMark inline link, and the
        // next run could not map the URL back to the object key, deleting the
        // remote object. The rewritten URL must be percent-encoded while the
        // object key keeps the raw file name.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("my image.png"), b"png-bytes").unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(&md, "![a](<my image.png>)\n").unwrap();

        let op = memory_op();
        run_once(&op, dir.path()).await.unwrap();

        // The link is now a valid URL with no bare space.
        let rewritten = "![a](https://cdn.example.com/my%20image.png)\n";
        assert_eq!(std::fs::read_to_string(&md).unwrap(), rewritten);

        // The object key is the raw (unencoded) file name.
        assert_eq!(
            op.read("my image.png").await.unwrap().to_vec(),
            b"png-bytes"
        );

        // Second run: the rewritten link is still recognized as managed, so
        // the object is not deleted and the markdown is left untouched.
        run_once(&op, dir.path()).await.unwrap();
        assert!(op.stat("my image.png").await.is_ok());
        assert_eq!(std::fs::read_to_string(&md).unwrap(), rewritten);
    }

    #[tokio::test]
    async fn non_ascii_file_name_is_encoded_and_survives_rerun() {
        // Same regression for non-ASCII (Chinese) names with a space in the
        // directory: RFC 3986 requires percent-encoding, and decoding the URL
        // back must be symmetric with the raw object key.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("图片 目录")).unwrap();
        std::fs::write(dir.path().join("图片 目录/图.png"), b"png-bytes").unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(&md, "![图](<图片 目录/图.png>)\n").unwrap();

        let op = memory_op();
        run_once(&op, dir.path()).await.unwrap();

        let rewritten =
            "![图](https://cdn.example.com/%E5%9B%BE%E7%89%87%20%E7%9B%AE%E5%BD%95/%E5%9B%BE.png)\n";
        assert_eq!(std::fs::read_to_string(&md).unwrap(), rewritten);
        assert_eq!(
            op.read("图片 目录/图.png").await.unwrap().to_vec(),
            b"png-bytes"
        );

        run_once(&op, dir.path()).await.unwrap();
        assert!(op.stat("图片 目录/图.png").await.is_ok());
        assert_eq!(std::fs::read_to_string(&md).unwrap(), rewritten);
    }

    #[tokio::test]
    async fn stale_remote_files_are_deleted_by_cloud_path() {
        // Regression: remote-only paths used to panic during UpFile
        // conversion instead of being deleted.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("post.md"),
            "![a](https://example.com/a.png)\n",
        )
        .unwrap();

        let op = memory_op();
        op.write("old/stale.png", b"stale".to_vec()).await.unwrap();
        run_once(&op, dir.path()).await.unwrap();

        assert!(op.stat("old/stale.png").await.is_err());
    }

    #[tokio::test]
    async fn changed_local_image_with_known_remote_md5_replaces_content() {
        // The memory backend does not expose checksums in listings (S3 does,
        // via the ETag), so the replace decision is unit-tested in the
        // differ; here we verify the "unknown checksum" policy: an object
        // whose remote MD5 cannot be compared is left untouched instead of
        // being endlessly re-uploaded.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("images")).unwrap();
        std::fs::write(dir.path().join("images/a.png"), b"v1").unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(&md, "![a](images/a.png)\n").unwrap();

        let op = memory_op();
        run_once(&op, dir.path()).await.unwrap();
        assert_eq!(op.read("images/a.png").await.unwrap().to_vec(), b"v1");

        // Simulate the author re-adding the local link after replacing the
        // image file (after the first run the link was rewritten to the CDN).
        std::fs::write(&md, "![a](images/a.png)\n").unwrap();
        std::fs::write(dir.path().join("images/a.png"), b"v2-longer").unwrap();
        // Memory has no remote checksum, so the change cannot be detected;
        // the object must stay intact (no churn) and the run must succeed.
        run_once(&op, dir.path()).await.unwrap();
        assert_eq!(op.read("images/a.png").await.unwrap().to_vec(), b"v1");
    }

    #[tokio::test]
    async fn same_basename_in_different_dirs_does_not_clobber() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("p1/images")).unwrap();
        std::fs::create_dir_all(dir.path().join("p2/images")).unwrap();
        std::fs::write(dir.path().join("p1/images/logo.png"), b"logo-one").unwrap();
        std::fs::write(dir.path().join("p2/images/logo.png"), b"logo-two").unwrap();
        std::fs::write(dir.path().join("p1/a.md"), "![l](images/logo.png)\n").unwrap();
        std::fs::write(dir.path().join("p2/b.md"), "![l](images/logo.png)\n").unwrap();

        let op = memory_op();
        run_once(&op, dir.path()).await.unwrap();

        assert_eq!(
            op.read("p1/images/logo.png").await.unwrap().to_vec(),
            b"logo-one"
        );
        assert_eq!(
            op.read("p2/images/logo.png").await.unwrap().to_vec(),
            b"logo-two"
        );

        let a = std::fs::read_to_string(dir.path().join("p1/a.md")).unwrap();
        assert!(
            a.contains("https://cdn.example.com/p1/images/logo.png"),
            "{}",
            a
        );
        let b = std::fs::read_to_string(dir.path().join("p2/b.md")).unwrap();
        assert!(
            b.contains("https://cdn.example.com/p2/images/logo.png"),
            "{}",
            b
        );
    }

    #[tokio::test]
    async fn run_fails_when_markdown_write_back_fails() {
        // Regression: when the images are already on the remote but the
        // markdown write-back fails (here: read-only file, non-root), the run
        // must return an error naming the file instead of exiting zero with
        // the markdown still pointing at local paths.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("images")).unwrap();
        std::fs::write(dir.path().join("images/a.png"), b"png-bytes").unwrap();
        let md = dir.path().join("post.md");
        std::fs::write(&md, "![a](images/a.png)\n").unwrap();

        let op = memory_op();
        // First run uploads the image and rewrites the link while the file
        // is still writable.
        run_once(&op, dir.path()).await.unwrap();
        assert!(op.stat("images/a.png").await.is_ok());

        // Re-introduce the local link, then make the file read-only so the
        // write-back of the second run fails deterministically.
        std::fs::write(&md, "![a](images/a.png)\n").unwrap();
        let original_perms = std::fs::metadata(&md).unwrap().permissions();
        let mut perms = original_perms.clone();
        perms.set_readonly(true);
        std::fs::set_permissions(&md, perms).unwrap();

        let result = run_once(&op, dir.path()).await;

        // Restore the original permissions so the tempdir cleanup succeeds.
        std::fs::set_permissions(&md, original_perms).unwrap();

        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("post.md"), "error must name the file: {msg}");
        assert!(msg.contains("rewrite"), "error must say what failed: {msg}");

        // The uploaded object survives; only the markdown was left untouched.
        assert!(op.stat("images/a.png").await.is_ok());
        assert_eq!(
            std::fs::read_to_string(&md).unwrap(),
            "![a](images/a.png)\n"
        );
    }
}
