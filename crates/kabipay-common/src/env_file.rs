//! Load `.env` from typical working directories so `cargo run` finds service config.
//!
//! `dotenvy::dotenv()` only reads `./.env` from the process cwd. Developers often run
//! `cargo run -p kabipay-auth` from the workspace root while `kabipay-svc/.env`
//! holds runtime `POSTGRES_*`.
//! Without this, services fall back to defaults (e.g. port 15432) and fail to connect
//! even though migrate works.

use std::path::{Path, PathBuf};

fn try_load(path: &Path) -> bool {
    path.is_file() && dotenvy::from_path(path).is_ok()
}

/// Load the first `.env` found by walking up from [`std::env::current_dir`].
///
/// At each directory, tries `./.env`, then `./kabipay-svc/.env`.
/// Falls back to `dotenvy::dotenv()` so a cwd-local `.env` still works.
pub fn load_dotenv() {
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir: Option<PathBuf> = Some(cwd);
        for _ in 0..8 {
            if let Some(ref d) = dir {
                if try_load(&d.join(".env")) {
                    return;
                }
                if try_load(&d.join("kabipay-svc").join(".env")) {
                    return;
                }
            }
            dir = dir.and_then(|d| d.parent().map(PathBuf::from));
        }
    }

    let _ = dotenvy::dotenv();
}
