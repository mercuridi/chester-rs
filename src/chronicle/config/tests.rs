use std::{fs, path::Path};

use anyhow::Result;
use serenity::all::{GuildId, UserId};
use tempfile::tempdir;

use super::{
    app::Config,
    paths::{resolve_path, resolve_sqlite_url},
    raw::RawConfig,
};

const CHRONICLE: &str = r#"
llm_repo = "owner/model"
llm_revision = "main"
llm_model_file = "model.gguf"
llm_tokenizer_repo = "owner/tokenizer"
llm_tokenizer_file = "tokenizer.json"
corpus_dir = "corpus"
llm_max_tokens = 512
llm_temperature = 0.7
llm_seed = 42
llm_system_prompt = "Answer from the corpus."
llm_max_reply_length = 1900
retrieval_limit = 5
max_chunk_tokens = 480
chunk_overlap_tokens = 48
"#;

fn full_config() -> String {
    format!(
        r#"
[chronicle]
{CHRONICLE}

[database]
jester = "sqlite://data/jester.sqlite3"
chronicle = "sqlite://data/chronicle.sqlite3?mode=rwc"

[alias_groups.party]
name = "Party"

[alias_groups.party.aliases]
"10" = "Alice"
"20" = "Bob"

[guilds."30"]
alias_groups = ["party"]
"#
    )
}

#[test]
fn example_uses_the_strict_current_schema() -> Result<()> {
    let _: RawConfig = toml::from_str(include_str!("../../../chronicle.config.example.toml"))?;
    Ok(())
}

#[test]
fn resolves_project_relative_paths_and_sqlite_urls() {
    let root = Path::new("/project");
    assert_eq!(resolve_path(root, "corpus"), Path::new("/project/corpus"));
    assert_eq!(
        resolve_path(root, "/data/corpus"),
        Path::new("/data/corpus")
    );
    assert_eq!(
        resolve_sqlite_url(root, "sqlite://data/db.sqlite?mode=rwc"),
        "sqlite:///project/data/db.sqlite?mode=rwc"
    );
    assert_eq!(
        resolve_sqlite_url(root, "sqlite://:memory:"),
        "sqlite://:memory:"
    );
}

#[test]
fn loads_nested_runtime_settings_and_discord_policy() -> Result<()> {
    let directory = tempdir()?;
    let config_dir = directory.path().join(".chronicle");
    fs::create_dir(&config_dir)?;
    let path = config_dir.join("config.toml");
    fs::write(
        &path,
        full_config().replace(
            "chunk_overlap_tokens = 48",
            "chunk_overlap_tokens = 48\ngm_user_ids = [\"99\"]",
        ),
    )?;

    let config = Config::load(&path)?;
    assert_eq!(config.chronicle.llm.model.repo, "owner/model");
    assert_eq!(
        config.chronicle.indexing.corpus_dir,
        directory.path().join("corpus")
    );
    assert_eq!(config.chronicle.retrieval.candidate_limit, 15);
    assert!(config.guild_has_alias_group(GuildId::new(30), "party"));
    assert!(
        config
            .validate_participants("party", [&UserId::new(10), &UserId::new(20)])
            .is_ok()
    );
    assert!(config.is_chronicle_gm(UserId::new(99)));
    Ok(())
}

#[test]
fn rejects_unknown_alias_group_and_duplicate_gm_ids() -> Result<()> {
    let unknown_group =
        full_config().replace("alias_groups = [\"party\"]", "alias_groups = [\"missing\"]");
    assert!(toml::from_str::<RawConfig>(&unknown_group).is_ok());
    let directory = tempdir()?;
    let config_dir = directory.path().join(".chronicle");
    fs::create_dir(&config_dir)?;
    let path = config_dir.join("config.toml");
    fs::write(&path, unknown_group)?;
    assert!(Config::load(&path).is_err());
    fs::write(
        &path,
        full_config().replace(
            "chunk_overlap_tokens = 48",
            "chunk_overlap_tokens = 48\ngm_user_ids = [\"10\", \"10\"]",
        ),
    )?;
    assert!(Config::load(&path).is_err());
    Ok(())
}
