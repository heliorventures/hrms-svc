//! Snapshot-consistent schema backup. The caller retains write-blocking locks until commit.
use crate::options::ImportOptions;
use anyhow::{bail, Result};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub async fn backup<C: ConnectionTrait>(
    db: &C,
    options: &ImportOptions,
    connection_host: &str,
    directory: &Path,
    bin: &Path,
) -> Result<PathBuf> {
    let snapshot = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_export_snapshot() AS id".to_owned(),
        ))
        .await?
        .ok_or_else(|| anyhow::anyhow!("BACKUP_SNAPSHOT_UNRESOLVED"))?
        .try_get::<String>("", "id")?;
    if !snapshot.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
        bail!("BACKUP_SNAPSHOT_INVALID");
    }
    let output = directory.join("tenant-before-import.dump");
    if output.exists() {
        bail!("BACKUP_ALREADY_EXISTS");
    }
    let mut dump = Command::new(bin.join(if cfg!(windows) {
        "pg_dump.exe"
    } else {
        "pg_dump"
    }));
    dump.args([
        "--format=custom",
        "--no-owner",
        "--no-acl",
        "--schema",
        &options.schema_name,
        "--snapshot",
        &snapshot,
        "--file",
    ])
    .arg(&output)
    .env("PGHOST", connection_host)
    .env("PGDATABASE", &options.db_name);
    configure_connection(&mut dump)?;
    let result = dump.output()?;
    if !result.status.success() {
        bail!("BACKUP_DUMP_FAILED");
    }
    let mut restore = Command::new(bin.join(if cfg!(windows) {
        "pg_restore.exe"
    } else {
        "pg_restore"
    }));
    restore.arg("--list").arg(&output);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        restore.creation_flags(0x08000000);
    }
    let listing = restore.output()?;
    if !listing.status.success()
        || !String::from_utf8_lossy(&listing.stdout).contains(&options.schema_name)
    {
        bail!("BACKUP_VERIFICATION_FAILED");
    }
    Ok(output)
}
fn configure_connection(command: &mut Command) -> Result<()> {
    for (source, target) in [
        ("POSTGRES_USER", "PGUSER"),
        ("POSTGRES_PASSWORD", "PGPASSWORD"),
        ("POSTGRES_PORT", "PGPORT"),
        ("POSTGRES_SSLMODE", "PGSSLMODE"),
    ] {
        if let Ok(value) = std::env::var(source) {
            command.env(target, value);
        } else if source != "POSTGRES_SSLMODE" {
            bail!("BACKUP_CONNECTION_CONFIGURATION_REQUIRED");
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Ok(())
}
