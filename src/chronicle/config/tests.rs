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
excluded_note_ids = []
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
candidate_limit = 15
distance_threshold = 0.8
near_duplicate_threshold = 0.85
max_chunks_per_document = 2
pagerank_weight = 0.15

[chronicle.synthesis]
retrieval_limit = 12
candidate_limit = 40
max_chunks_per_document = 3
batch_token_budget = 1800
max_batches = 6

[chronicle.access]
gm_user_ids = []

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
fn loads_complete_nested_settings_and_domain_ownership() -> Result<()> {
    let config = load(&CONFIG.replace("gm_user_ids = []", "gm_user_ids = [\"99\"]"))?;
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
fn requires_all_chronicle_settings() {
    for missing in [
        "excluded_note_ids = []\n",
        "context_limit = 4096\n",
        "candidate_limit = 15\n",
        "distance_threshold = 0.8\n",
        "near_duplicate_threshold = 0.85\n",
        "max_chunks_per_document = 2\n",
        "pagerank_weight = 0.15\n",
        "retrieval_limit = 12\n",
        "candidate_limit = 40\n",
        "max_chunks_per_document = 3\n",
        "batch_token_budget = 1800\n",
        "max_batches = 6\n",
        "gm_user_ids = []\n",
    ] {
        assert!(
            load(&CONFIG.replace(missing, "")).is_err(),
            "{missing} must be required"
        );
    }

    let synthesis = "[chronicle.synthesis]\nretrieval_limit = 12\ncandidate_limit = 40\nmax_chunks_per_document = 3\nbatch_token_budget = 1800\nmax_batches = 6\n\n";
    assert!(load(&CONFIG.replace(synthesis, "")).is_err());
}

#[test]
fn requires_explicit_discord_maps_and_collections() {
    let empty_discord = "[discord.alias_groups.party]\nname = \"Party\"\n\n[discord.alias_groups.party.aliases]\n\"10\" = \"Alice\"\n\"20\" = \"Bob\"\n\n[discord.guilds.\"30\"]\nalias_groups = [\"party\"]\n";
    assert!(load(&CONFIG.replace(empty_discord, "")).is_err());

    let empty_aliases =
        "[discord.alias_groups.party.aliases]\n\"10\" = \"Alice\"\n\"20\" = \"Bob\"\n\n";
    assert!(load(&CONFIG.replace(empty_aliases, "")).is_err());
    assert!(load(&CONFIG.replace("alias_groups = [\"party\"]", "")).is_err());
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
fn rejects_retired_indexing_keys_with_parse_context() {
    let legacy = CONFIG.replace("max_chunk_tokens = 480", "max_chunk_length = 480");
    let error = load(&legacy).expect_err("retired key must not be accepted");
    let message = format!("{error:#}");

    assert!(message.contains("Failed to parse config file"));
    assert!(message.contains("max_chunk_length"));
}

#[test]
fn validates_current_chronicle_numeric_boundaries_and_required_strings() {
    for invalid in [
        CONFIG.replace("max_chunk_tokens = 480", "max_chunk_tokens = 2"),
        CONFIG.replace("chunk_overlap_tokens = 48", "chunk_overlap_tokens = 480"),
        CONFIG.replace("limit = 5", "limit = 0"),
        CONFIG.replace("max_tokens = 512", "max_tokens = 32769"),
        CONFIG.replace(
            "system_prompt = \"Answer from the corpus.\"",
            "system_prompt = \"\"",
        ),
    ] {
        assert!(load(&invalid).is_err());
    }
}

#[test]
fn rejects_invalid_discord_ids_and_unknown_guild_alias_groups() {
    let invalid_user_id = CONFIG.replace("\"10\" = \"Alice\"", "\"0\" = \"Alice\"");
    assert!(load(&invalid_user_id).is_err());

    let unknown_group =
        CONFIG.replace("alias_groups = [\"party\"]", "alias_groups = [\"missing\"]");
    assert!(load(&unknown_group).is_err());
}

#[test]
fn participant_validation_reports_missing_aliases() -> Result<()> {
    let config = load(CONFIG)?;
    let error = config
        .validate_participants("party", [&UserId::new(10), &UserId::new(99)])
        .expect_err("unknown participant must require an alias");

    assert!(error.to_string().contains("99"));
    Ok(())
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
