//! TUI state and input handling; rendering lives in `ui.rs`.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use seekr_net::{Client, DownloadState, Event};

use super::results::Results;
use super::transfers::Transfers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Search,
    Transfers,
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
    pub status: String,
    pub connected: bool,
    /// Rows visible in the current list, for page up/down.
    pub page_size: usize,
    pub quit: bool,
    confirm_quit: bool,
}

impl App {
    pub fn new(client: Client, username: String) -> Self {
        Self {
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
            status: String::new(),
            connected: true,
            page_size: 10,
            quit: false,
            confirm_quit: false,
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

        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Tab | KeyCode::BackTab => {
                self.tab = match self.tab {
                    Tab::Search => Tab::Transfers,
                    Tab::Transfers => Tab::Search,
                }
            }
            KeyCode::Char('1') => self.tab = Tab::Search,
            KeyCode::Char('2') => self.tab = Tab::Transfers,
            KeyCode::Char('/') => {
                self.tab = Tab::Search;
                self.focus = Focus::Input;
            }
            _ => match self.tab {
                Tab::Search => self.on_results_key(key),
                Tab::Transfers => self.on_transfers_key(key),
            },
        }
    }

    fn request_quit(&mut self) {
        let active = self.transfers.active();
        if active == 0 || self.confirm_quit {
            self.quit = true;
        } else {
            self.confirm_quit = true;
            self.status = format!("{active} downloads still active – press q again to quit");
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
