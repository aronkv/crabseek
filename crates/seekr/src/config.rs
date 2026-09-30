//! `~/.config/seekr/config.toml`: credentials and settings.
//!
//! The file is written with mode 600 inside a 700 directory, the same way
//! other Soulseek clients store the password (the protocol needs it in
//! plain text for the login hash).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use directories::{BaseDirs, ProjectDirs, UserDirs};
use seekr_net::ClientConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_server")]
    pub server: String,
    #[serde(default = "default_port")]
    pub listen_port: u16,
    /// Defaults to `~/Downloads/seekr`; a leading `~/` is expanded.
    download_dir: Option<PathBuf>,
    /// Folders offered to other users. Defaults to `~/Music`.
    shared_dirs: Option<Vec<PathBuf>>,
}

impl Default for Config {
    fn default() -> Self {
        toml::from_str("").expect("all fields have defaults")
    }
}

fn default_server() -> String {
    seekr_proto::server::DEFAULT_SERVER.to_owned()
}

fn default_port() -> u16 {
    2234
}

impl Config {
    pub fn has_credentials(&self) -> bool {
        !self.username.trim().is_empty() && !self.password.is_empty()
    }

    pub fn download_dir(&self) -> anyhow::Result<PathBuf> {
        match &self.download_dir {
            Some(dir) => expand_home(dir),
            None => {
                let dirs = UserDirs::new().context("could not determine home directory")?;
                let downloads = dirs
                    .download_dir()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| dirs.home_dir().join("Downloads"));
                Ok(downloads.join("seekr"))
            }
        }
    }

    pub fn shared_dirs(&self) -> anyhow::Result<Vec<PathBuf>> {
        match &self.shared_dirs {
            Some(dirs) => dirs.iter().map(|d| expand_home(d)).collect(),
            None => {
                let dirs = UserDirs::new().context("could not determine home directory")?;
                Ok(vec![
                    dirs.audio_dir()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| dirs.home_dir().join("Music")),
                ])
            }
        }
    }

    pub fn client_config(&self) -> anyhow::Result<ClientConfig> {
        Ok(ClientConfig {
            server: self.server.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            listen_port: self.listen_port,
            download_dir: self.download_dir()?,
        })
    }
}

pub fn expand_home(path: &Path) -> anyhow::Result<PathBuf> {
    match path.strip_prefix("~") {
        Ok(rest) => Ok(BaseDirs::new()
            .context("could not determine home directory")?
            .home_dir()
            .join(rest)),
        Err(_) => Ok(path.to_owned()),
    }
}

fn project_dirs() -> anyhow::Result<ProjectDirs> {
    ProjectDirs::from("", "", "seekr").context("could not determine home directory")
}

pub fn path() -> anyhow::Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("config.toml"))
}

/// `path` with the home directory shown as `~`.
pub fn display_path(path: &Path) -> String {
    match BaseDirs::new().and_then(|d| path.strip_prefix(d.home_dir()).ok().map(PathBuf::from)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Log file used while the TUI owns the terminal.
pub fn log_path() -> anyhow::Result<PathBuf> {
    let dirs = project_dirs()?;
    let dir = dirs.state_dir().unwrap_or_else(|| dirs.cache_dir());
    Ok(dir.join("seekr.log"))
}

/// The config, or defaults when there is no file yet.
pub fn load_or_default() -> anyhow::Result<Config> {
    let path = path()?;
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The config for CLI subcommands, which need saved credentials.
pub fn load() -> anyhow::Result<Config> {
    let cfg = load_or_default()?;
    if !cfg.has_credentials() {
        bail!("not logged in – run `seekr` once to log in");
    }
    Ok(cfg)
}

/// Stores the credentials, keeping every other setting in the file.
pub fn save_credentials(username: &str, password: &str) -> anyhow::Result<()> {
    update(|table| {
        table.insert("username".into(), username.into());
        table.insert("password".into(), password.into());
    })
}

pub fn save_download_dir(dir: &Path) -> anyhow::Result<()> {
    update(|table| {
        table.insert("download_dir".into(), display_path(dir).into());
    })
}

pub fn save_shared_dirs(dirs: &[PathBuf]) -> anyhow::Result<()> {
    update(|table| {
        let list: Vec<toml::Value> = dirs.iter().map(|d| display_path(d).into()).collect();
        table.insert("shared_dirs".into(), list.into());
    })
}

/// Removes the stored credentials. Returns false if there were none.
pub fn clear_credentials() -> anyhow::Result<bool> {
    let mut removed = false;
    update(|table| {
        removed |= table.remove("username").is_some();
        removed |= table.remove("password").is_some();
    })?;
    Ok(removed)
}

fn update(change: impl FnOnce(&mut toml::Table)) -> anyhow::Result<()> {
    let path = path()?;
    let mut table: toml::Table = match fs::read_to_string(&path) {
        Ok(text) => text
            .parse()
            .with_context(|| format!("parsing {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    change(&mut table);
    write_private(&path, &toml::to_string(&table)?)
}

/// Writes `contents` readable only by the user, replacing `path`
/// atomically so a crash never leaves a half-written config.
fn write_private(path: &Path, contents: &str) -> anyhow::Result<()> {
    let dir = path.parent().context("config path has no parent")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("toml.tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.server, "server.slsknet.org:2242");
        assert_eq!(cfg.listen_port, 2234);
        assert!(!cfg.has_credentials());
        assert!(cfg.download_dir().unwrap().ends_with("seekr"));
        assert_eq!(cfg.shared_dirs().unwrap().len(), 1);
    }

    #[test]
    fn tilde_expands() {
        let cfg: Config = toml::from_str("download_dir = \"~/x/y\"").unwrap();
        let home = BaseDirs::new().unwrap().home_dir().to_owned();
        assert_eq!(cfg.download_dir().unwrap(), home.join("x/y"));
        let cfg: Config = toml::from_str("shared_dirs = [\"~/a\", \"/b\"]").unwrap();
        assert_eq!(
            cfg.shared_dirs().unwrap(),
            [home.join("a"), PathBuf::from("/b")]
        );
        assert_eq!(display_path(&home.join("a")), "~/a");
    }

    #[test]
    fn private_write_keeps_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("seekr-cfg-{}", std::process::id()));
        let path = dir.join("config.toml");
        write_private(&path, "a = 1").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        fs::remove_dir_all(dir).unwrap();
    }
}
