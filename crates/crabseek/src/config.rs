//! `~/.config/crabseek/config.toml`: credentials and settings.
//!
//! The file is written with mode 600 inside a 700 directory, the same way
//! other Soulseek clients store the password (the protocol needs it in
//! plain text for the login hash).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use crabseek_net::ClientConfig;
use directories::{BaseDirs, ProjectDirs, UserDirs};
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
    /// Defaults to `~/Downloads/crabseek`; a leading `~/` is expanded.
    download_dir: Option<PathBuf>,
    /// Folders offered to other users. Defaults to `~/Music`.
    shared_dirs: Option<Vec<PathBuf>>,
    /// Uploads that may run at the same time.
    #[serde(default = "default_upload_slots")]
    pub upload_slots: usize,
    /// Open the listen port on the router automatically (UPnP).
    #[serde(default = "default_upnp")]
    pub upnp: bool,
    /// Desktop notifications; off unless turned on.
    #[serde(default)]
    pub notifications: bool,
}

impl Default for Config {
    fn default() -> Self {
        toml::from_str("").expect("all fields have defaults")
    }
}

fn default_server() -> String {
    crabseek_proto::server::DEFAULT_SERVER.to_owned()
}

fn default_port() -> u16 {
    2234
}

fn default_upload_slots() -> usize {
    2
}

fn default_upnp() -> bool {
    true
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
                Ok(downloads.join("crabseek"))
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
            shared_dirs: self.shared_dirs()?,
            share_cache: Some(project_dirs()?.cache_dir().join("shares.json")),
            upload_slots: self.upload_slots,
            upnp: self.upnp,
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
    ProjectDirs::from("", "", "crabseek").context("could not determine home directory")
}

/// The project was called seekr before. On the first run under the new
/// name, its config, data, cache and state folders move over, so the
/// login, settings, download list, buddies, wishlist and chats survive.
/// Downloads stay where they are: a config without `download_dir` gets the
/// old default written in.
pub fn migrate_from_seekr() -> anyhow::Result<Vec<String>> {
    let (Some(old), Ok(new)) = (ProjectDirs::from("", "", "seekr"), project_dirs()) else {
        return Ok(Vec::new());
    };
    let pairs = [
        (old.config_dir(), new.config_dir()),
        (old.data_dir(), new.data_dir()),
        (old.cache_dir(), new.cache_dir()),
        (
            old.state_dir().unwrap_or(old.cache_dir()),
            new.state_dir().unwrap_or(new.cache_dir()),
        ),
    ];
    let pairs: Vec<(PathBuf, PathBuf)> = pairs
        .into_iter()
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect();
    let downloads = UserDirs::new().map(|u| {
        u.download_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| u.home_dir().join("Downloads"))
    });
    let state = new.state_dir().unwrap_or(new.cache_dir()).to_owned();
    let moved = migrate_dirs(&pairs, new.config_dir(), downloads.as_deref())?;
    // The log file inside the moved state folder kept its old name.
    let _ = fs::remove_file(state.join("seekr.log"));
    Ok(moved)
}

/// Moves each `(old, new)` folder that exists only under its old name.
/// `config_dir` is the new config folder; `downloads` the user's Downloads.
fn migrate_dirs(
    pairs: &[(PathBuf, PathBuf)],
    config_dir: &Path,
    downloads: Option<&Path>,
) -> anyhow::Result<Vec<String>> {
    let mut moved = Vec::new();
    let mut config_moved = false;
    for (from, to) in pairs {
        if from.is_dir() && !to.exists() {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(from, to)
                .with_context(|| format!("moving {} to {}", from.display(), to.display()))?;
            moved.push(format!("{} → {}", display_path(from), display_path(to)));
            config_moved |= to == config_dir;
        }
    }
    // Keep downloads in the old default folder rather than splitting them.
    let config = config_dir.join("config.toml");
    if config_moved
        && let Ok(text) = fs::read_to_string(&config)
        && let Ok(table) = text.parse::<toml::Table>()
        && !table.contains_key("download_dir")
        && let Some(downloads) = downloads
    {
        let old_default = downloads.join("seekr");
        if old_default.is_dir() {
            let mut table = table;
            table.insert("download_dir".into(), display_path(&old_default).into());
            write_private(&config, &toml::to_string(&table)?)?;
        }
    }
    Ok(moved)
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

/// The download list kept between runs.
pub fn downloads_path() -> anyhow::Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("downloads.json"))
}

/// Private message history.
pub fn chats_path() -> anyhow::Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("chats.json"))
}

/// Saved wishlist queries.
pub fn wishlist_path() -> anyhow::Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("wishlist.json"))
}

pub fn buddies_path() -> anyhow::Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("buddies.json"))
}

/// Log file used while the TUI owns the terminal.
pub fn log_path() -> anyhow::Result<PathBuf> {
    let dirs = project_dirs()?;
    let dir = dirs.state_dir().unwrap_or_else(|| dirs.cache_dir());
    Ok(dir.join("crabseek.log"))
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
        bail!("not logged in – run `crabseek` once to log in");
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

pub fn save_listen_port(port: u16) -> anyhow::Result<()> {
    update(|table| {
        table.insert("listen_port".into(), i64::from(port).into());
    })
}

pub fn save_notifications(enabled: bool) -> anyhow::Result<()> {
    update(|table| {
        table.insert("notifications".into(), enabled.into());
    })
}

pub fn save_upnp(enabled: bool) -> anyhow::Result<()> {
    update(|table| {
        table.insert("upnp".into(), enabled.into());
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
pub(crate) fn write_private(path: &Path, contents: &str) -> anyhow::Result<()> {
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
    fn migrates_old_folders_once() {
        let base = std::env::temp_dir().join(format!("crabseek-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let p = |s: &str| base.join(s);
        let pairs = vec![
            (p("config/seekr"), p("config/crabseek")),
            (p("share/seekr"), p("share/crabseek")),
            (p("cache/seekr"), p("cache/crabseek")),
        ];
        fs::create_dir_all(p("config/seekr")).unwrap();
        fs::write(p("config/seekr/config.toml"), "username = \"me\"\n").unwrap();
        fs::create_dir_all(p("share/seekr")).unwrap();
        fs::write(p("share/seekr/chats.json"), "[]").unwrap();
        // Existing downloads in the old default folder.
        fs::create_dir_all(p("Downloads/seekr")).unwrap();

        let moved = migrate_dirs(&pairs, &p("config/crabseek"), Some(&p("Downloads"))).unwrap();
        assert_eq!(moved.len(), 2, "{moved:?}");
        assert!(p("share/crabseek/chats.json").exists());
        assert!(!p("config/seekr").exists());
        let cfg: Config =
            toml::from_str(&fs::read_to_string(p("config/crabseek/config.toml")).unwrap()).unwrap();
        assert_eq!(cfg.username, "me");
        assert_eq!(cfg.download_dir().unwrap(), p("Downloads/seekr"));
        // A second run has nothing to do.
        assert!(
            migrate_dirs(&pairs, &p("config/crabseek"), Some(&p("Downloads")))
                .unwrap()
                .is_empty()
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.server, "server.slsknet.org:2242");
        assert_eq!(cfg.listen_port, 2234);
        assert!(!cfg.has_credentials());
        assert!(cfg.download_dir().unwrap().ends_with("crabseek"));
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
        let dir = std::env::temp_dir().join(format!("crabseek-cfg-{}", std::process::id()));
        let path = dir.join("config.toml");
        write_private(&path, "a = 1").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        fs::remove_dir_all(dir).unwrap();
    }
}
