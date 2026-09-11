use std::{fs, io::Write, path::Path};

/// Atomically replace `path` with `contents`.
///
/// The temporary file is created in the destination directory with a unique
/// name, synced before the rename, and the containing directory is synced
/// afterwards. This makes a successful return mean that both the file data and
/// the directory entry have been handed to the filesystem for persistence.
pub(crate) fn write_atomic(path: impl AsRef<Path>, contents: &[u8]) -> anyhow::Result<()> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Atomic-write target has no parent directory"))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".atomic-")
        .tempfile_in(parent)?;

    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;

    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
