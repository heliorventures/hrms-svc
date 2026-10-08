//! Protect a new output directory before writing packages, credentials or backups.
use anyhow::{bail, Result};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

pub fn create(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        bail!("OUTPUT_DIRECTORY_MUST_BE_NEW");
    }
    fs::create_dir(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let identity = Command::new("whoami.exe")
            .args(["/user", "/fo", "csv", "/nh"])
            .creation_flags(0x08000000)
            .output()?;
        if !identity.status.success() {
            bail!("PRIVATE_OUTPUT_IDENTITY_UNRESOLVED");
        }
        let text = String::from_utf8(identity.stdout)?;
        let sid = text
            .split(['"', ',', '\r', '\n'])
            .find(|value| {
                value.starts_with("S-1-")
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
            })
            .ok_or_else(|| anyhow::anyhow!("PRIVATE_OUTPUT_IDENTITY_UNRESOLVED"))?;
        let acl = Command::new("icacls.exe")
            .arg(path)
            .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
            .creation_flags(0x08000000)
            .output()?;
        if !acl.status.success() {
            bail!("PRIVATE_OUTPUT_ACL_FAILED");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(fs::canonicalize(path)?)
}
pub fn write_json<T: serde::Serialize>(directory: &Path, name: &str, value: &T) -> Result<()> {
    if Path::new(name).file_name().and_then(|v| v.to_str()) != Some(name)
        || !name.ends_with(".json")
    {
        bail!("OUTPUT_NAME_INVALID");
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(directory.join(name))?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
