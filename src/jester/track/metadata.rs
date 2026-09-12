use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};

pub fn metadata_sidecar_path(audio_dir: &Path, file_id: &str) -> PathBuf {
    audio_dir.join(format!("{file_id}.metadata.json"))
}

pub fn read_metadata_sidecar(audio_dir: &Path, file_id: &str) -> Result<Value> {
    let path = metadata_sidecar_path(audio_dir, file_id);
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read metadata sidecar {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse metadata sidecar {}", path.display()))
}

pub fn process_ytdlp_json_at(
    audio_dir: &std::path::Path,
    file_id: &str,
) -> Result<serde_json::Value> {
    let path = audio_dir.join(format!("{file_id}.info.json"));
    let content =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;

    // Parse the full JSON
    let v: Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse JSON from {}", path.display()))?;

    // Extract only the fields we want
    let slim = json!({
        "id": v.get("id").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'id' field in yt-dlp JSON"))?,
        "upload_date": v.get("upload_date").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'upload_date' field in yt-dlp JSON"))?,
        "title": v.get("title").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'title' field in yt-dlp JSON"))?,
        "channel": v.get("channel").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'channel' field in yt-dlp JSON"))?,
    });

    fs::remove_file(&path).ok();

    Ok(slim)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::process_ytdlp_json_at;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn extracts_supported_metadata_and_removes_source_file() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("track.info.json");
        fs::write(
            &path,
            r#"{"id":"track","upload_date":"20240102","title":"Title","channel":"Artist","ignored":true}"#,
        )?;

        let value = process_ytdlp_json_at(directory.path(), "track")?;

        assert_eq!(value["id"], "track");
        assert_eq!(value["upload_date"], "20240102");
        assert_eq!(value["title"], "Title");
        assert_eq!(value["channel"], "Artist");
        assert!(value.get("ignored").is_none());
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn reports_missing_required_fields_without_removing_source() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("track.info.json");
        fs::write(&path, r#"{"id":"track"}"#)?;

        let error = process_ytdlp_json_at(directory.path(), "track").unwrap_err();

        assert!(error.to_string().contains("upload_date"));
        assert!(path.exists());
        Ok(())
    }

    #[test]
    fn reports_invalid_json_with_file_context() -> anyhow::Result<()> {
        let directory = tempdir()?;
        fs::write(directory.path().join("bad.info.json"), "{")?;

        let error = process_ytdlp_json_at(directory.path(), "bad").unwrap_err();

        assert!(format!("{error:#}").contains("Failed to parse JSON"));
        Ok(())
    }

    #[test]
    fn reports_missing_metadata_file() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let error = process_ytdlp_json_at(directory.path(), "missing").unwrap_err();
        assert!(format!("{error:#}").contains("Failed to read"));
        Ok(())
    }
}
