use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub recordings_dir: PathBuf,
    pub audio_dir: PathBuf,
}

impl AppPaths {
    pub(crate) fn from_project_root(project_root: &Path) -> Self {
        Self {
            recordings_dir: project_root.join(".chronicle/recordings"),
            audio_dir: project_root.join("audio"),
        }
    }
}

pub(crate) fn project_root(config_path: &Path) -> &Path {
    config_path
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) fn resolve_path(project_root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
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
