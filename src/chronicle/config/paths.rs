use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub runtime_root: PathBuf,
    pub config_path: PathBuf,
    pub env_path: PathBuf,
    pub log_dir: PathBuf,
    pub recordings_dir: PathBuf,
    pub audio_dir: PathBuf,
    pub ytdlp_path: PathBuf,
    pub cookies_path: PathBuf,
}

impl AppPaths {
    /// Resolves every runtime-owned path from one deployment root.
    ///
    /// A relative config path is interpreted relative to `runtime_root`; this
    /// keeps a relocated binary independent of its process working directory.
    pub fn from_runtime_root(runtime_root: &Path, config_path: Option<&Path>) -> Result<Self> {
        let runtime_root = absolute_path(runtime_root)?;
        let config_path = config_path.map_or_else(
            || runtime_root.join(".chronicle/config.toml"),
            |path| resolve_path(&runtime_root, path),
        );
        Ok(Self {
            env_path: runtime_root.join(".env"),
            log_dir: runtime_root.join("logs/application"),
            recordings_dir: runtime_root.join(".chronicle/recordings"),
            audio_dir: runtime_root.join("audio"),
            ytdlp_path: runtime_root.join("yt-dlp"),
            cookies_path: runtime_root.join("cookies.txt"),
            runtime_root,
            config_path,
        })
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .context("Failed to determine current directory")
            .map(|directory| directory.join(path))
    }
}

pub(crate) fn resolve_path(root: &Path, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

pub(crate) fn resolve_sqlite_url(project_root: &Path, url: &str) -> String {
    let Some(path_and_query) = url.strip_prefix("sqlite://") else {
        return url.to_owned();
    };
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    if path == ":memory:" || path.starts_with('/') {
        return url.to_owned();
    }
    let resolved = resolve_path(project_root, path)
        .to_string_lossy()
        .into_owned();
    match query {
        Some(query) => format!("sqlite://{resolved}?{query}"),
        None => format!("sqlite://{resolved}"),
    }
}
