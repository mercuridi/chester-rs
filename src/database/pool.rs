use anyhow::{Context, Result};
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::{path::Path, str::FromStr};

pub async fn open_sqlite_pool(database_url: &str, database_name: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("Failed to parse {database_name} database URL: {database_url}"))?;

    if let Some(parent) = options.get_filename().parent()
        && !parent.as_os_str().is_empty()
        && parent != Path::new(".")
    {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create {database_name} database directory: {}",
                parent.display()
            )
        })?;
    }

    // SQLite applies foreign-key enforcement per connection, not per database.
    // Put the pragma in the connection options so SQLx enables it whenever the
    // pool creates a connection, including connections opened after startup.
    let pool = SqlitePool::connect_with(options.create_if_missing(true).foreign_keys(true))
        .await
        .with_context(|| format!("Failed to open {database_name} database: {database_url}"))?;

    let rows = sqlx::query("PRAGMA database_list")
        .fetch_all(&pool)
        .await
        .with_context(|| format!("Failed to inspect {database_name} database"))?;

    for row in rows {
        let name: String = row.get("name");
        let file: String = row.get("file");
        tracing::info!(database = %name, file = %file, database_name, "SQLite database opened");
    }

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enables_foreign_keys_on_every_pool_connection() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database_url = format!(
            "sqlite://{}",
            directory.path().join("test.sqlite3").display()
        );
        let pool = open_sqlite_pool(&database_url, "test").await?;

        // Holding the first connection forces the pool to create a second one.
        let mut first = pool.acquire().await?;
        let mut second = pool.acquire().await?;

        for connection in [&mut first, &mut second] {
            let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(&mut **connection)
                .await?;
            assert_eq!(enabled, 1, "foreign keys must be enabled per connection");
        }

        sqlx::raw_sql(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY);\
             CREATE TABLE children (parent_id INTEGER NOT NULL REFERENCES parents(id));",
        )
        .execute(&mut *first)
        .await?;

        let Err(error) = sqlx::query("INSERT INTO children (parent_id) VALUES (999)")
            .execute(&mut *second)
            .await
        else {
            anyhow::bail!("a second pooled connection accepted an orphaned row");
        };
        assert!(error.to_string().contains("FOREIGN KEY constraint failed"));

        Ok(())
    }
}
