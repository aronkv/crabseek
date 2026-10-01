//! TUI state and input handling; rendering lives in `ui.rs`.

use std::path::PathBuf;
use std::time::Instant;

use crabseek_net::{Client, DistribStatus, DownloadState, Event, PortMapStatus};
use crabseek_proto::search::{SearchFile, SearchResponse};
use crabseek_proto::server::ServerResponse;
use crabseek_proto::shares::SharedFileList;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::buddies::Buddies;
use super::chat::{self, ChatInput, Chats};
use super::notify::{self, DownloadBatch};
use super::results::Results;
use super::settings::{Settings, SettingsAction};
use super::transfers::Transfers;
use super::uploads::Uploads;
use super::wishlist::Wishlist;
use crate::config::{self, Config};
use crate::persist::{self, SavedDownload, SavedStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Search,
    Transfers,
    Uploads,
    Settings,
    Browse,
    Buddies,
    Wishlist,
    Chat,
}

impl Tab {
    /// Tab-bar order; the numbers (`Alt-1`…`Alt-6`) follow it.
    const ORDER: [Tab; 8] = [
        Tab::Search,
        Tab::Transfers,
        Tab::Uploads,
        Tab::Settings,
        Tab::Browse,
        Tab::Buddies,
        Tab::Wishlist,
        Tab::Chat,
    ];

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

/// Which result tree a key acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Search,
    Browse,
}

pub struct ActiveBrowse {
    pub username: String,
    pub started: Instant,
    pub loaded: bool,
    pub error: Option<String>,
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
    pub browse: Option<ActiveBrowse>,
    pub browse_input: String,
    pub browse_focus: Focus,
    pub browse_results: Results,
    pub browse_offset: usize,
    pub transfers: Transfers,
    pub transfers_offset: usize,
    pub uploads: Uploads,
    pub uploads_offset: usize,
    pub buddies: Buddies,
    pub buddies_offset: usize,
    pub shares: SharesStatus,
    pub distrib: DistribStatus,
    pub portmap: PortMapStatus,
    /// Searches by other users that our shares answered this session.
    pub searches_answered: u64,
    pub settings: Settings,
    pub status: String,
    pub connected: bool,
    /// Rows visible in the current list, for page up/down.
    pub page_size: usize,
    pub quit: bool,
    /// Where the download list is saved; `None` in tests.
    downloads_path: Option<PathBuf>,
    /// Where the buddy list is saved; `None` in tests.
    buddies_path: Option<PathBuf>,
    pub wishlist: Wishlist,
    wishlist_path: Option<PathBuf>,
    pub chats: Chats,
    chats_path: Option<PathBuf>,
    confirm_quit: bool,
    /// Vim-style count typed before a motion (`10k`).
    pub count: Option<usize>,
    /// The `?` help window is open.
    pub help: bool,
    /// Finished downloads waiting to be announced together.
    download_batch: DownloadBatch,
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
            // Start in the list so every key works right away; `s`, `/`
            // or `Alt-s` open the search box.
            focus: Focus::List,
            input: String::new(),
            search: None,
            results: Results::default(),
            results_offset: 0,
            browse: None,
            browse_input: String::new(),
            browse_focus: Focus::List,
            browse_results: Results::default(),
            browse_offset: 0,
            transfers: Transfers::default(),
            transfers_offset: 0,
            uploads: Uploads::default(),
            uploads_offset: 0,
            buddies: Buddies::default(),
            buddies_offset: 0,
            shares: SharesStatus::Scanning,
            distrib: DistribStatus::Searching,
            portmap: if cfg.upnp {
                PortMapStatus::Trying
            } else {
                PortMapStatus::Disabled
            },
            searches_answered: 0,
            settings: Settings::new(
                cfg.download_dir()?,
                cfg.listen_port,
                cfg.upnp,
                cfg.shared_dirs()?,
            ),
            status: String::new(),
            connected: true,
            page_size: 10,
            quit: false,
            confirm_quit: false,
            count: None,
            help: false,
            download_batch: DownloadBatch::default(),
            downloads_path,
            buddies_path: None,
            wishlist: Wishlist::default(),
            wishlist_path: None,
            chats: Chats::default(),
            chats_path: None,
        };
        app.settings.notifications = cfg.notifications;
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

    /// Puts the saved conversations in place.
    pub fn with_chats(mut self, list: Vec<chat::Conversation>, path: Option<PathBuf>) -> Self {
        self.chats = Chats::new(list);
        self.chats_path = path;
        self
    }

    /// Opens the Chat tab on `username`'s conversation, ready to type.
    fn open_chat(&mut self, username: &str) {
        self.chats.open(username);
        self.chats.input = Some(ChatInput::Message(String::new()));
        self.tab = Tab::Chat;
    }

    /// Puts the saved wishlist queries in place.
    pub fn with_wishlist(mut self, queries: Vec<String>, path: Option<PathBuf>) -> Self {
        self.wishlist = Wishlist::new(queries);
        self.wishlist_path = path;
        self
    }

    fn save_wishlist(&self, done: String) -> String {
        let Some(path) = &self.wishlist_path else {
            return done;
        };
        match persist::save_wishlist(path, &self.wishlist.queries()) {
            Ok(()) => done,
            Err(e) => format!("could not save the wishlist: {e:#}"),
        }
    }

    fn add_wish(&mut self, query: &str) {
        self.status = if self.wishlist.add(query) {
            self.save_wishlist(format!("added {:?} to the wishlist", query.trim()))
        } else {
            format!("{:?} is already on the wishlist", query.trim())
        };
    }

    /// Runs wishlist query `index` now and restarts the interval.
    fn run_wish(&mut self, index: usize) {
        let query = self.wishlist.items[index].query.clone();
        match self.client.wishlist_search(&query) {
            Ok(token) => {
                if let Some(old) = self.wishlist.mark_run(index, token, Instant::now()) {
                    let _ = self.client.stop_search(old);
                }
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    /// Called on every tick: runs the wishlist query whose turn it is.
    pub fn tick(&mut self) {
        if let Some(index) = self.wishlist.due(Instant::now()) {
            self.run_wish(index);
        }
        if let Some((summary, body)) = self.download_batch.take_due(Instant::now()) {
            notify::send(summary, body);
        }
    }

    /// Opens a wishlist item's collected results on the Search tab.
    fn open_wish(&mut self) {
        let Some(item) = self.wishlist.items.get_mut(self.wishlist.selected) else {
            return;
        };
        item.new = 0;
        self.results = item.results.clone();
        self.results_offset = 0;
        self.search = Some(ActiveSearch {
            token: item.latest_token().unwrap_or(0),
            query: format!("wishlist: {}", item.query),
            started: item.last_run.unwrap_or_else(Instant::now),
        });
        self.input = item.query.clone();
        self.tab = Tab::Search;
        self.focus = Focus::List;
    }

    /// Puts the saved buddies on the list and starts watching them.
    pub fn with_buddies(mut self, names: Vec<String>, path: Option<PathBuf>) -> Self {
        for name in &names {
            let _ = self.client.watch_user(name);
        }
        self.buddies = Buddies::new(names);
        self.buddies_path = path;
        self
    }

    fn add_buddy(&mut self, username: &str) {
        let username = username.trim();
        self.status = if username.is_empty() {
            return;
        } else if username == self.username {
            "you cannot add yourself".to_owned()
        } else if !self.buddies.add(username.to_owned()) {
            format!("{username} is already a buddy")
        } else {
            let _ = self.client.watch_user(username);
            self.save_buddies(format!("added {username} to buddies"))
        };
    }

    fn remove_selected_buddy(&mut self) {
        if let Some(username) = self.buddies.remove_selected() {
            let _ = self.client.unwatch_user(&username);
            self.status = self.save_buddies(format!("removed {username} from buddies"));
        }
    }

    /// Returns `done`, or the error if saving failed.
    fn save_buddies(&self, done: String) -> String {
        let Some(path) = &self.buddies_path else {
            return done;
        };
        match persist::save_buddies(path, &self.buddies.names()) {
            Ok(()) => done,
            Err(e) => format!("could not save the buddy list: {e:#}"),
        }
    }

    /// The server does not push stat changes for watched users, so they
    /// are fetched again whenever the Buddies tab is opened.
    fn refresh_buddy_stats(&self) {
        for b in &self.buddies.list {
            let _ = self.client.user_stats(&b.username);
        }
    }

    /// The user of the selected row on the current tab, for `A`.
    fn selected_user(&mut self) -> Option<String> {
        match self.tab {
            Tab::Search => self
                .results
                .selection_files()
                .into_iter()
                .next()
                .map(|(user, _)| user),
            Tab::Browse => self.browse.as_ref().map(|b| b.username.clone()),
            Tab::Transfers => self.transfers.selected().map(|t| t.username.clone()),
            Tab::Uploads => self.uploads.selected().map(|u| u.username.clone()),
            Tab::Settings | Tab::Buddies | Tab::Wishlist | Tab::Chat => None,
        }
    }

    /// Saves the download list if it changed.
    pub fn persist(&mut self) {
        if self.chats.dirty {
            self.chats.dirty = false;
            if let Some(path) = &self.chats_path
                && let Err(e) = chat::save(path, &self.chats.list)
            {
                self.status = format!("could not save the chats: {e:#}");
            }
        }
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
                    self.results.add(resp.clone());
                }
                if let Some((query, new)) = self.wishlist.on_result(resp)
                    && new > 0
                {
                    self.status = format!("wishlist: {new} new files for {query:?}");
                    if self.settings.notifications {
                        notify::send(
                            format!("Wishlist: {query}"),
                            format!("{new} new files found"),
                        );
                    }
                }
            }
            Event::PrivateMessage {
                timestamp,
                username,
                message,
                ..
            } => {
                let seen = self.tab == Tab::Chat
                    && self
                        .chats
                        .selected()
                        .is_some_and(|c| c.username == username);
                let preview: String = message.chars().take(60).collect();
                self.status = format!("message from {username}: {preview}");
                if self.settings.notifications && !seen {
                    notify::send(format!("Message from {username}"), message.clone());
                }
                self.chats.receive(&username, message, timestamp, seen);
            }
            Event::ServerMessage(ServerResponse::WishlistInterval(secs)) => {
                self.wishlist.set_interval(secs)
            }
            Event::BrowseResult { username, list } => {
                if let Some(b) = &mut self.browse
                    && b.username == username
                    && !b.loaded
                {
                    b.loaded = true;
                    self.browse_results = Results::with_filter(self.browse_results.filter());
                    self.browse_results
                        .add(share_list_as_response(username, list));
                }
            }
            Event::PeerConnectFailed { username, reason } => {
                if let Some(b) = &mut self.browse
                    && b.username == username
                    && !b.loaded
                {
                    b.error = Some(format!("could not reach {username}: {reason}"));
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
                    if self.settings.notifications {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        self.download_batch.push(name, Instant::now());
                    }
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
            Event::Distrib(status) => self.distrib = status,
            Event::PortMap(status) => self.portmap = status,
            Event::SearchAnswered { .. } => self.searches_answered += 1,
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
            Event::ServerMessage(ServerResponse::WatchUser { username, user }) => {
                self.buddies.on_watch(&username, user)
            }
            Event::ServerMessage(ServerResponse::UserStatus {
                username, status, ..
            }) => self.buddies.on_status(&username, status),
            Event::ServerMessage(ServerResponse::UserStats { username, stats }) => {
                self.buddies.on_stats(&username, stats)
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
        if self.help {
            // The help window swallows keys until it is closed.
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                self.help = false;
            }
            return;
        }
        if key.code != KeyCode::Char('q') {
            self.confirm_quit = false;
        }
        // `Alt-s` types nothing, so it toggles the search box from
        // anywhere, even while typing in it.
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::ALT) {
            self.count = None;
            if self.tab == Tab::Search && self.focus == Focus::Input {
                self.focus = Focus::List;
            } else {
                self.open_search();
            }
            return;
        }

        if self.focus == Focus::Input && self.tab == Tab::Search {
            self.on_input_key(key);
            return;
        }
        if self.browse_focus == Focus::Input && self.tab == Tab::Browse {
            self.on_browse_input_key(key);
            return;
        }
        // A path being edited takes every key.
        if self.tab == Tab::Settings && self.settings.is_editing() {
            self.on_settings_key(key);
            return;
        }
        if self.tab == Tab::Buddies && self.buddies.adding.is_some() {
            self.on_buddy_input_key(key);
            return;
        }
        if self.tab == Tab::Wishlist && self.wishlist.adding.is_some() {
            self.on_wish_input_key(key);
            return;
        }
        if self.tab == Tab::Chat && self.chats.input.is_some() {
            self.on_chat_input_key(key);
            return;
        }

        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Digits build a count for the next motion, like in vim.
        if let KeyCode::Char(c @ '0'..='9') = key.code
            && !alt
            && (c != '0' || self.count.is_some())
        {
            let digit = c as usize - '0' as usize;
            self.count = Some((self.count.unwrap_or(0) * 10 + digit).min(99_999));
            return;
        }
        if key.code == KeyCode::Char('?') {
            self.count = None;
            self.help = true;
            return;
        }
        let count = self.count.take();
        let tab_before = self.tab;

        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Tab => self.tab = self.tab.cycle(1),
            KeyCode::BackTab => self.tab = self.tab.cycle(-1),
            KeyCode::Char('1') | KeyCode::F(1) => self.tab = Tab::Search,
            KeyCode::Char('2') | KeyCode::F(2) => self.tab = Tab::Transfers,
            KeyCode::Char('3') | KeyCode::F(3) => self.tab = Tab::Uploads,
            KeyCode::Char('4') | KeyCode::F(4) => self.tab = Tab::Settings,
            KeyCode::Char('5') | KeyCode::F(5) => self.tab = Tab::Browse,
            KeyCode::Char('6') | KeyCode::F(6) => self.tab = Tab::Buddies,
            KeyCode::Char('7') | KeyCode::F(7) => self.tab = Tab::Wishlist,
            KeyCode::Char('8') | KeyCode::F(8) => self.tab = Tab::Chat,
            // `m` writes to the user of the selected row.
            KeyCode::Char('m') if self.tab != Tab::Chat => {
                if let Some(user) = self.selected_user() {
                    self.open_chat(&user);
                }
            }
            // `A` adds the user of the selected row as a buddy.
            KeyCode::Char('A') => {
                if let Some(user) = self.selected_user() {
                    self.add_buddy(&user);
                }
            }
            // `/` edits the input of the current tab, or starts a search;
            // `s` always starts a search.
            KeyCode::Char('/') if self.tab == Tab::Browse => self.browse_focus = Focus::Input,
            KeyCode::Char('/' | 's') => self.open_search(),
            _ => match self.tab {
                Tab::Search => self.on_results_key(key, count, Which::Search),
                Tab::Browse => self.on_results_key(key, count, Which::Browse),
                Tab::Transfers => self.on_transfers_key(key, count),
                Tab::Uploads => self.on_uploads_key(key, count),
                Tab::Buddies => self.on_buddies_key(key, count),
                Tab::Wishlist => self.on_wishlist_key(key, count),
                Tab::Chat => self.on_chat_key(key, count),
                Tab::Settings => {
                    // Settings moves one row per key; repeat for a count.
                    let times = if matches!(
                        key.code,
                        KeyCode::Char('j' | 'k') | KeyCode::Up | KeyCode::Down
                    ) {
                        count.unwrap_or(1)
                    } else {
                        1
                    };
                    for _ in 0..times {
                        self.on_settings_key(key);
                    }
                }
            },
        }
        if self.tab == Tab::Buddies && tab_before != Tab::Buddies {
            self.refresh_buddy_stats();
        }
    }

    fn on_chat_key(&mut self, key: KeyEvent, count: Option<usize>) {
        let n = count.unwrap_or(1) as isize;
        let c = &mut self.chats;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => c.move_by(n),
            KeyCode::Up | KeyCode::Char('k') => c.move_by(-n),
            KeyCode::Home | KeyCode::Char('g') => c.selected = 0,
            KeyCode::End | KeyCode::Char('G') => c.move_by(isize::MAX),
            KeyCode::Enter | KeyCode::Char('i') if c.selected().is_some() => {
                c.input = Some(ChatInput::Message(String::new()))
            }
            KeyCode::Char('a') => c.input = Some(ChatInput::NewUser(String::new())),
            KeyCode::Char('b') => {
                if let Some(user) = c.selected().map(|c| c.username.clone()) {
                    self.start_browse(user);
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(user) = self.chats.remove_selected() {
                    self.status = format!("deleted the conversation with {user}");
                }
            }
            _ => {}
        }
        self.chats.mark_selected_read();
    }

    fn on_chat_input_key(&mut self, key: KeyEvent) {
        let Some(input) = &mut self.chats.input else {
            return;
        };
        let text = match input {
            ChatInput::Message(t) | ChatInput::NewUser(t) => t,
        };
        match key.code {
            KeyCode::Esc => self.chats.input = None,
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
            KeyCode::Char(ch) => text.push(ch),
            KeyCode::Enter => match self.chats.input.take() {
                Some(ChatInput::NewUser(user)) => {
                    let user = user.trim().to_owned();
                    if !user.is_empty() {
                        self.open_chat(&user);
                    }
                }
                Some(ChatInput::Message(message)) => {
                    // Stay in the box for the next line.
                    self.chats.input = Some(ChatInput::Message(String::new()));
                    let message = message.trim().to_owned();
                    let Some(user) = self.chats.selected().map(|c| c.username.clone()) else {
                        return;
                    };
                    if message.is_empty() {
                        return;
                    }
                    match self.client.message_user(&user, &message) {
                        Ok(()) => self.chats.sent(&user, message, chat::now()),
                        Err(e) => self.status = e.to_string(),
                    }
                }
                None => {}
            },
            _ => {}
        }
    }

    fn on_wishlist_key(&mut self, key: KeyEvent, count: Option<usize>) {
        let n = count.unwrap_or(1) as isize;
        let w = &mut self.wishlist;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => w.move_by(n),
            KeyCode::Up | KeyCode::Char('k') => w.move_by(-n),
            KeyCode::Home | KeyCode::Char('g') => w.selected = 0,
            KeyCode::End | KeyCode::Char('G') => match count {
                Some(line) => w.selected = (line - 1).min(w.items.len().saturating_sub(1)),
                None => w.move_by(isize::MAX),
            },
            KeyCode::Char('a') => w.adding = Some(String::new()),
            KeyCode::Enter => self.open_wish(),
            KeyCode::Char('r') => {
                if self.wishlist.selected < self.wishlist.items.len() {
                    self.run_wish(self.wishlist.selected);
                    self.status =
                        "searching now; the next scheduled run waits a full interval".into();
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some((query, tokens)) = self.wishlist.remove_selected() {
                    for token in tokens {
                        let _ = self.client.stop_search(token);
                    }
                    self.status =
                        self.save_wishlist(format!("removed {query:?} from the wishlist"));
                }
            }
            _ => {}
        }
    }

    fn on_wish_input_key(&mut self, key: KeyEvent) {
        let Some(text) = &mut self.wishlist.adding else {
            return;
        };
        match key.code {
            KeyCode::Enter => {
                let query = std::mem::take(text);
                self.wishlist.adding = None;
                self.add_wish(&query);
            }
            KeyCode::Esc => self.wishlist.adding = None,
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
            KeyCode::Char(c) => text.push(c),
            _ => {}
        }
    }

    fn open_search(&mut self) {
        self.tab = Tab::Search;
        self.focus = Focus::Input;
    }

    fn on_buddies_key(&mut self, key: KeyEvent, count: Option<usize>) {
        let n = count.unwrap_or(1) as isize;
        let page = self.page_size.max(1) as isize * n;
        let b = &mut self.buddies;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => b.move_by(n),
            KeyCode::Up | KeyCode::Char('k') => b.move_by(-n),
            KeyCode::PageDown => b.move_by(page),
            KeyCode::PageUp => b.move_by(-page),
            KeyCode::Home | KeyCode::Char('g') => b.selected = 0,
            KeyCode::End | KeyCode::Char('G') => match count {
                Some(line) => b.selected = (line - 1).min(b.list.len().saturating_sub(1)),
                None => b.move_by(isize::MAX),
            },
            KeyCode::Char('a') => b.adding = Some(String::new()),
            KeyCode::Char('x') => self.remove_selected_buddy(),
            KeyCode::Enter | KeyCode::Char('b') => {
                if let Some(user) = self.buddies.selected().map(|b| b.username.clone()) {
                    self.start_browse(user);
                }
            }
            _ => {}
        }
    }

    fn on_buddy_input_key(&mut self, key: KeyEvent) {
        let Some(input) = &mut self.buddies.adding else {
            return;
        };
        match key.code {
            KeyCode::Enter => {
                let name = std::mem::take(input);
                self.buddies.adding = None;
                self.add_buddy(&name);
            }
            KeyCode::Esc => self.buddies.adding = None,
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => input.clear(),
            KeyCode::Char(c) => input.push(c),
            _ => {}
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
            SettingsAction::SetNotifications(enabled) => {
                config::save_notifications(enabled).map(|()| {
                    if enabled {
                        notify::send(
                            "crabseek".to_owned(),
                            "Desktop notifications are on".to_owned(),
                        );
                        "desktop notifications on".to_owned()
                    } else {
                        "desktop notifications off".to_owned()
                    }
                })
            }
            SettingsAction::SetUpnp(enabled) => config::save_upnp(enabled).map(|()| {
                let _ = self.client.set_upnp(enabled);
                if enabled {
                    "opening the port on the router...".to_owned()
                } else {
                    "automatic port forwarding off".to_owned()
                }
            }),
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

    fn on_uploads_key(&mut self, key: KeyEvent, count: Option<usize>) {
        let n = count.unwrap_or(1) as isize;
        let page = self.page_size.max(1) as isize * n;
        let u = &mut self.uploads;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => u.move_by(n),
            KeyCode::Up | KeyCode::Char('k') => u.move_by(-n),
            KeyCode::PageDown => u.move_by(page),
            KeyCode::PageUp => u.move_by(-page),
            KeyCode::Home | KeyCode::Char('g') => u.selected = 0,
            KeyCode::End | KeyCode::Char('G') => match count {
                Some(line) => u.selected = (line - 1).min(u.list.len().saturating_sub(1)),
                None => u.move_by(isize::MAX),
            },
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

    fn on_browse_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                let user = self.browse_input.trim().to_owned();
                if !user.is_empty() {
                    self.start_browse(user);
                }
            }
            KeyCode::Esc => self.browse_focus = Focus::List,
            KeyCode::Backspace => {
                self.browse_input.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.browse_input.clear()
            }
            KeyCode::Char(c) => self.browse_input.push(c),
            _ => {}
        }
    }

    /// Opens `username`'s shares on the Browse tab.
    fn start_browse(&mut self, username: String) {
        self.tab = Tab::Browse;
        self.browse_focus = Focus::List;
        self.browse_input = username.clone();
        self.browse_results = Results::with_filter(self.browse_results.filter());
        self.browse_offset = 0;
        self.browse = Some(ActiveBrowse {
            username: username.clone(),
            started: Instant::now(),
            loaded: false,
            error: None,
        });
        if let Err(e) = self.client.browse(username) {
            self.status = e.to_string();
        }
    }

    fn results_mut(&mut self, which: Which) -> &mut Results {
        match which {
            Which::Search => &mut self.results,
            Which::Browse => &mut self.browse_results,
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
                self.results = Results::with_filter(self.results.filter());
                self.results_offset = 0;
                self.status.clear();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn on_results_key(&mut self, key: KeyEvent, count: Option<usize>, which: Which) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let n = count.unwrap_or(1) as isize;
        let page = self.page_size.max(1) as isize * n;
        if key.code == KeyCode::Char('b') {
            // Browse the user of the selected row.
            if let Some((user, _)) = self.results_mut(which).selection_files().first() {
                let user = user.clone();
                self.start_browse(user);
            }
            return;
        }
        if key.code == KeyCode::Char('w') && which == Which::Search {
            // Keep looking for the current search in the background.
            if let Some(search) = &self.search {
                let query = search
                    .query
                    .strip_prefix("wishlist: ")
                    .unwrap_or(&search.query)
                    .to_owned();
                self.add_wish(&query);
            }
            return;
        }
        let r = self.results_mut(which);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => r.move_by(n),
            KeyCode::Up | KeyCode::Char('k') => r.move_by(-n),
            KeyCode::PageDown => r.move_by(page),
            KeyCode::PageUp => r.move_by(-page),
            KeyCode::Char('d') if ctrl => r.move_by(page / 2),
            KeyCode::Char('u') if ctrl => r.move_by(-page / 2),
            KeyCode::Home | KeyCode::Char('g') => r.move_to_start(),
            KeyCode::End | KeyCode::Char('G') => match count {
                Some(line) => r.move_to_index(line - 1),
                None => r.move_to_end(),
            },
            KeyCode::Enter | KeyCode::Char(' ') => r.toggle(),
            KeyCode::Right | KeyCode::Char('l') => r.expand(),
            KeyCode::Left | KeyCode::Char('h') => r.collapse(),
            KeyCode::Char('d') => self.download_selection(which),
            KeyCode::Char('f') => r.set_filter(r.filter().next()),
            KeyCode::Char('F') => r.set_filter(r.filter().prev()),
            _ => {}
        }
    }

    fn download_selection(&mut self, which: Which) {
        let files = self.results_mut(which).selection_files();
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

    fn on_transfers_key(&mut self, key: KeyEvent, count: Option<usize>) {
        let n = count.unwrap_or(1) as isize;
        let page = self.page_size.max(1) as isize * n;
        let t = &mut self.transfers;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => t.move_by(n),
            KeyCode::Up | KeyCode::Char('k') => t.move_by(-n),
            KeyCode::PageDown => t.move_by(page),
            KeyCode::PageUp => t.move_by(-page),
            KeyCode::Home | KeyCode::Char('g') => t.selected = 0,
            KeyCode::End | KeyCode::Char('G') => match count {
                Some(line) => t.selected = (line - 1).min(t.list.len().saturating_sub(1)),
                None => t.move_by(isize::MAX),
            },
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

/// A share list in the shape of a search response, so the Browse tab can
/// reuse the result tree. File names get their folder path back.
fn share_list_as_response(username: String, list: SharedFileList) -> SearchResponse {
    let files = list
        .dirs
        .into_iter()
        .flat_map(|dir| {
            let path = dir.path;
            dir.files.into_iter().map(move |f| SearchFile {
                filename: format!("{path}\\{}", f.filename),
                ..f
            })
        })
        .collect();
    SearchResponse {
        username,
        token: 0,
        files,
        slot_free: true,
        avg_speed: 0,
        queue_length: 0,
        private_files: Vec::new(),
    }
}
