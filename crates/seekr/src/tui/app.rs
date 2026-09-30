//! TUI state and input handling; rendering lives in `ui.rs`.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use seekr_net::{Client, DownloadState, Event};

use super::results::Results;
use super::settings::{Settings, SettingsAction};
use super::transfers::Transfers;
use super::uploads::Uploads;
use crate::config::{self, Config};
use crate::persist::{self, SavedDownload, SavedStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Search,
    Transfers,
    Uploads,
    Settings,
}

impl Tab {
    const ORDER: [Tab; 4] = [Tab::Search, Tab::Transfers, Tab::Uploads, Tab::Settings];

    pub fn index(self) -> usize {
        Self::ORDER.iter().position(|t| *t == self).unwrap()
    }

    fn cycle(self, delta: isize) -> Tab {
        let n = Self::ORDER.len() as isize;
        Self::ORDER[(self.index() as isize + delta).rem_euclid(n) as usize]
    }
}

/// What we currently share, as last reported by the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharesStatus {
    Scanning,
    Ready { folders: usize, files: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    List,
}

pub struct ActiveSearch {
    pub token: u32,
    pub query: String,
    pub started: Instant,
}

pub struct App {
    pub client: Client,
    pub username: String,
    pub tab: Tab,
    pub focus: Focus,
    pub input: String,
    pub search: Option<ActiveSearch>,
    pub results: Results,
    /// First visible result row; kept by the renderer.
    pub results_offset: usize,
    pub transfers: Transfers,
    pub transfers_offset: usize,
    pub uploads: Uploads,
    pub uploads_offset: usize,
    pub shares: SharesStatus,
    pub settings: Settings,
    pub status: String,
    pub connected: bool,
    /// Rows visible in the current list, for page up/down.
    pub page_size: usize,
    pub quit: bool,
    /// Where the download list is saved; `None` in tests.
    downloads_path: Option<PathBuf>,
    confirm_quit: bool,
}

impl App {
    pub fn new(
        client: Client,
        username: String,
        cfg: &Config,
        saved: Vec<SavedDownload>,
        downloads_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let mut app = Self {
            client,
            username,
            tab: Tab::Search,
            focus: Focus::Input,
            input: String::new(),
            search: None,
            results: Results::default(),
            results_offset: 0,
            transfers: Transfers::default(),
            transfers_offset: 0,
            uploads: Uploads::default(),
            uploads_offset: 0,
            shares: SharesStatus::Scanning,
            settings: Settings::new(cfg.download_dir()?, cfg.listen_port, cfg.shared_dirs()?),
            status: String::new(),
            connected: true,
            page_size: 10,
            quit: false,
            confirm_quit: false,
            downloads_path,
        };
        app.restore(saved);
        Ok(app)
    }

    /// Puts the saved list back: finished entries as they were, unfinished
    /// ones queued again (they resume from their `.part` files).
    fn restore(&mut self, saved: Vec<SavedDownload>) {
        // Finished entries are only displayed; give them ids the client
        // never hands out.
        let mut restored_id = u64::MAX;
        let mut next_restored_id = || {
            restored_id -= 1;
            restored_id
        };
        let mut requeued = 0;
        for d in saved {
            let (id, state) = match d.status {
                SavedStatus::Pending => match self.client.download(&d.username, &d.filename) {
                    Ok(id) => {
                        requeued += 1;
                        (id, DownloadState::Queued { place: None })
                    }
                    Err(e) => (
                        next_restored_id(),
                        DownloadState::Failed {
                            reason: format!("could not queue again: {e}"),
                        },
                    ),
                },
                SavedStatus::Completed { path } => {
                    (next_restored_id(), DownloadState::Completed { path })
                }
                SavedStatus::Failed { reason } => {
                    (next_restored_id(), DownloadState::Failed { reason })
                }
            };
            self.transfers.update(id, d.username, d.filename, state);
        }
        self.transfers.dirty = false;
        if requeued > 0 {
            self.status = format!("resuming {requeued} unfinished downloads from last time");
        }
    }

    /// Saves the download list if it changed.
    pub fn persist(&mut self) {
        if !self.transfers.dirty {
            return;
        }
        self.transfers.dirty = false;
        if let Some(path) = &self.downloads_path
            && let Err(e) = persist::save(path, &self.transfers.snapshot())
        {
            self.status = format!("could not save the download list: {e:#}");
        }
    }

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::SearchResult(resp) => {
                if self.search.as_ref().is_some_and(|s| s.token == resp.token) {
                    self.results.add(resp);
                }
            }
            Event::Download {
                id,
                username,
                filename,
                state,
            } => {
                if let DownloadState::Completed { path } = &state {
                    self.status = format!("saved {}", path.display());
                }
                self.transfers.update(id, username, filename, state);
            }
            Event::Upload {
                id,
                username,
                filename,
                state,
            } => self.uploads.update(id, username, filename, state),
            Event::SharesScanning => self.shares = SharesStatus::Scanning,
            Event::SharesScanned {
                folders,
                files,
                errors,
            } => {
                self.shares = SharesStatus::Ready { folders, files };
                if let Some(first) = errors.first() {
                    self.status = format!("sharing problem: {first}");
                }
            }
            Event::ListenPort { port, result } => {
                self.status = match result
                    .map_err(anyhow::Error::msg)
                    .and_then(|()| config::save_listen_port(port))
                {
                    Ok(()) => {
                        self.settings.listen_port = port;
                        format!("listening on port {port} – forward it on your router")
                    }
                    Err(e) => format!("could not use port {port}: {e:#}"),
                };
            }
            Event::ServerClosed { reason } => {
                self.connected = false;
                self.status = format!("disconnected from server: {reason}");
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if key.code != KeyCode::Char('q') {
            self.confirm_quit = false;
        }

        if self.focus == Focus::Input && self.tab == Tab::Search {
            self.on_input_key(key);
            return;
        }
        // A path being edited takes every key.
        if self.tab == Tab::Settings && self.settings.is_editing() {
            self.on_settings_key(key);
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Tab => self.tab = self.tab.cycle(1),
            KeyCode::BackTab => self.tab = self.tab.cycle(-1),
            KeyCode::Char('1') => self.tab = Tab::Search,
            KeyCode::Char('2') => self.tab = Tab::Transfers,
            KeyCode::Char('3') => self.tab = Tab::Uploads,
            KeyCode::Char('4') => self.tab = Tab::Settings,
            KeyCode::Char('/') => {
                self.tab = Tab::Search;
                self.focus = Focus::Input;
            }
            _ => match self.tab {
                Tab::Search => self.on_results_key(key),
                Tab::Transfers => self.on_transfers_key(key),
                Tab::Uploads => self.on_uploads_key(key),
                Tab::Settings => self.on_settings_key(key),
            },
        }
    }

    fn on_settings_key(&mut self, key: KeyEvent) {
        let result = match self.settings.on_key(key) {
            SettingsAction::None => return,
            SettingsAction::SetDownloadDir(dir) => config::save_download_dir(&dir).map(|()| {
                let _ = self.client.set_download_dir(dir.clone());
                format!("downloads now go to {}", config::display_path(&dir))
            }),
            SettingsAction::SetListenPort(port) => {
                self.status = match self.client.set_listen_port(port) {
                    Ok(()) => format!("switching to port {port}..."),
                    Err(e) => e.to_string(),
                };
                return;
            }
            SettingsAction::SetSharedDirs(dirs) => config::save_shared_dirs(&dirs).map(|()| {
                let _ = self.client.rescan_shares(dirs);
                "shared folders saved, rescanning...".to_owned()
            }),
        };
        self.status = match result {
            Ok(msg) => msg,
            Err(e) => format!("could not save the config: {e:#}"),
        };
    }

    fn on_uploads_key(&mut self, key: KeyEvent) {
        let page = self.page_size.max(1) as isize;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.uploads.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.uploads.move_by(-1),
            KeyCode::PageDown => self.uploads.move_by(page),
            KeyCode::PageUp => self.uploads.move_by(-page),
            KeyCode::Home | KeyCode::Char('g') => self.uploads.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.uploads.move_by(isize::MAX),
            KeyCode::Char('c') => {
                if let Some(u) = self.uploads.selected()
                    && !u.is_finished()
                {
                    let _ = self.client.cancel_upload(u.id);
                }
            }
            KeyCode::Char('x') => {
                let n = self.uploads.clear_finished();
                let _ = self.client.clear_finished_uploads();
                self.status = format!("cleared {n} finished uploads");
            }
            _ => {}
        }
    }

    fn request_quit(&mut self) {
        let active = self.transfers.active() + self.uploads.active();
        if active == 0 || self.confirm_quit {
            self.quit = true;
        } else {
            self.confirm_quit = true;
            self.status = format!("{active} transfers still active – press q again to quit");
        }
    }

    fn on_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let query = self.input.trim().to_owned();
                if !query.is_empty() {
                    self.start_search(query);
                    self.focus = Focus::List;
                }
            }
            KeyCode::Esc => self.focus = Focus::List,
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear()
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn start_search(&mut self, query: String) {
        if let Some(old) = self.search.take() {
            let _ = self.client.stop_search(old.token);
        }
        match self.client.search(&query) {
            Ok(token) => {
                self.search = Some(ActiveSearch {
                    token,
                    query,
                    started: Instant::now(),
                });
                self.results = Results::default();
                self.results_offset = 0;
                self.status.clear();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn on_results_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.page_size.max(1) as isize;
        let r = &mut self.results;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => r.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => r.move_by(-1),
            KeyCode::PageDown => r.move_by(page),
            KeyCode::PageUp => r.move_by(-page),
            KeyCode::Char('d') if ctrl => r.move_by(page / 2),
            KeyCode::Char('u') if ctrl => r.move_by(-page / 2),
            KeyCode::Home | KeyCode::Char('g') => r.move_to_start(),
            KeyCode::End | KeyCode::Char('G') => r.move_to_end(),
            KeyCode::Enter | KeyCode::Char(' ') => r.toggle(),
            KeyCode::Right | KeyCode::Char('l') => r.expand(),
            KeyCode::Left | KeyCode::Char('h') => r.collapse(),
            KeyCode::Char('d') => self.download_selection(),
            KeyCode::Char('f') => r.set_filter(r.filter().next()),
            KeyCode::Char('F') => r.set_filter(r.filter().prev()),
            _ => {}
        }
    }

    fn download_selection(&mut self) {
        let files = self.results.selection_files();
        let Some((username, _)) = files.first() else {
            return;
        };
        let username = username.clone();
        let mut queued = 0;
        for (user, filename) in files {
            let duplicate = self.transfers.list.iter().any(|t| {
                t.username == user
                    && t.filename == filename
                    && !matches!(t.state, DownloadState::Failed { .. })
            });
            if duplicate {
                continue;
            }
            match self.client.download(user, filename) {
                Ok(_) => queued += 1,
                Err(e) => {
                    self.status = e.to_string();
                    return;
                }
            }
        }
        self.status = match queued {
            0 => "already downloading".to_owned(),
            1 => format!("queued 1 file from {username}"),
            n => format!("queued {n} files from {username}"),
        };
    }

    fn on_transfers_key(&mut self, key: KeyEvent) {
        let page = self.page_size.max(1) as isize;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.transfers.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.transfers.move_by(-1),
            KeyCode::PageDown => self.transfers.move_by(page),
            KeyCode::PageUp => self.transfers.move_by(-page),
            KeyCode::Home | KeyCode::Char('g') => self.transfers.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.transfers.move_by(isize::MAX),
            KeyCode::Char('c') => {
                if let Some(t) = self.transfers.selected()
                    && !t.is_finished()
                {
                    let _ = self.client.cancel_download(t.id);
                }
            }
            KeyCode::Char('r') => {
                if let Some(t) = self.transfers.selected()
                    && matches!(t.state, DownloadState::Failed { .. })
                {
                    let (id, user, file) = (t.id, t.username.clone(), t.filename.clone());
                    if self.client.download(user, file).is_ok() {
                        self.transfers.remove(id);
                        self.status = "retrying".to_owned();
                    }
                }
            }
            KeyCode::Char('x') => {
                let n = self.transfers.clear_finished();
                self.status = format!("cleared {n} finished transfers");
            }
            _ => {}
        }
    }
}
