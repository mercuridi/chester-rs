use std::{fs, path::Path};

use anyhow::Result;
use serenity::all::{GuildId, UserId};
use tempfile::tempdir;

use super::{
    app::Config,
    paths::{resolve_path, resolve_sqlite_url},
};

const CONFIG: &str = r#"
[database]
jester = "sqlite://data/jester.sqlite3"
chronicle = "sqlite://data/chronicle.sqlite3?mode=rwc"

[chronicle.indexing]
corpus_dir = "corpus"
max_chunk_tokens = 480
chunk_overlap_tokens = 48

[chronicle.llm.model]
repo = "owner/model"
revision = "main"
file = "model.gguf"

[chronicle.llm.tokenizer]
repo = "owner/tokenizer"
file = "tokenizer.json"

[chronicle.llm.generation]
max_tokens = 512
context_limit = 4096
temperature = 0.7
seed = 42
system_prompt = "Answer from the corpus."
max_reply_length = 1900

[chronicle.retrieval]
limit = 5

[discord.alias_groups.party]
name = "Party"

[discord.alias_groups.party.aliases]
"10" = "Alice"
"20" = "Bob"

[discord.guilds."30"]
alias_groups = ["party"]
"#;

fn load(contents: &str) -> Result<Config> {
    let directory = tempdir()?;
    let config_dir = directory.path().join(".chronicle");
    fs::create_dir(&config_dir)?;
    let path = config_dir.join("config.toml");
    fs::write(&path, contents)?;
    Config::load(&path)
}

#[test]
fn checked_in_example_uses_the_only_supported_schema() -> Result<()> {
    let directory = tempdir()?;
    let config_dir = directory.path().join(".chronicle");
    fs::create_dir(&config_dir)?;
    let path = config_dir.join("config.toml");
    fs::write(
        &path,
        include_str!("../../../chronicle.config.example.toml"),
    )?;
    Config::load(path)?;
    Ok(())
}

#[test]
fn loads_nested_settings_with_defaults_and_domain_ownership() -> Result<()> {
    let config = load(&CONFIG.replace(
        "[chronicle.retrieval]",
        "[chronicle.access]\ngm_user_ids = [\"99\"]\n\n[chronicle.retrieval]",
    ))?;
    assert_eq!(config.chronicle.llm.model.repo, "owner/model");
    assert_eq!(config.chronicle.llm.generation.context_limit, 4096);
    assert_eq!(config.chronicle.retrieval.candidate_limit, 15);
    assert_eq!(config.chronicle.synthesis.max_batches, 6);
    assert!(config.chronicle.access.is_gm(UserId::new(99)));
    assert!(config.guild_has_alias_group(GuildId::new(30), "party"));
    assert!(
        config
            .validate_participants("party", [&UserId::new(10), &UserId::new(20)])
            .is_ok()
    );
    Ok(())
}

#[test]
fn applies_nested_setting_defaults() -> Result<()> {
    let config = load(&CONFIG.replace("context_limit = 4096\n", ""))?;
    assert_eq!(config.chronicle.llm.generation.context_limit, 8_192);
    assert_eq!(config.chronicle.retrieval.candidate_limit, 15);
    assert_eq!(config.chronicle.synthesis.batch_token_budget, 1_800);
    Ok(())
}

#[test]
fn rejects_legacy_flat_chronicle_and_top_level_discord_keys() {
    let legacy_chronicle = CONFIG.replace(
        "[chronicle.indexing]",
        "[chronicle]\nllm_repo = \"owner/model\"",
    );
    assert!(load(&legacy_chronicle).is_err());
    let legacy_discord = CONFIG.replace("[discord.alias_groups.party]", "[alias_groups.party]");
    assert!(load(&legacy_discord).is_err());
    let legacy_guilds = CONFIG.replace("[discord.guilds.\"30\"]", "[guilds.\"30\"]");
    assert!(load(&legacy_guilds).is_err());
}

#[test]
fn rejects_cross_setting_synthesis_budget_overflow() {
    let invalid = format!("{CONFIG}\n[chronicle.synthesis]\nbatch_token_budget = 3585\n");
    assert!(load(&invalid).is_err());
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
