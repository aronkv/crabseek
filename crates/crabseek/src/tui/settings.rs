//! The Settings tab: download folder, listen port and shared folders,
//! edited in place (folders with Tab completion). Changes are saved to the
//! config right away.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{display_path, expand_home};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    DownloadDir,
    ListenPort,
    Upnp,
    Notifications,
    Shared(usize),
    AddShared,
}

pub struct Edit {
    pub item: Item,
    pub text: String,
}

/// What the app should do after a key press on this tab.
#[derive(Debug, PartialEq, Eq)]
pub enum SettingsAction {
    None,
    SetDownloadDir(PathBuf),
    /// Saved once the new port is actually bound.
    SetListenPort(u16),
    SetUpnp(bool),
    SetNotifications(bool),
    SetSharedDirs(Vec<PathBuf>),
}

pub struct Settings {
    pub download_dir: PathBuf,
    pub listen_port: u16,
    pub upnp: bool,
    /// Desktop notifications; set by the app from the config.
    pub notifications: bool,
    pub shared: Vec<PathBuf>,
    pub selected: usize,
    pub edit: Option<Edit>,
    pub error: Option<String>,
}

impl Settings {
    pub fn new(download_dir: PathBuf, listen_port: u16, upnp: bool, shared: Vec<PathBuf>) -> Self {
        Self {
            download_dir,
            listen_port,
            upnp,
            notifications: false,
            shared,
            selected: 0,
            edit: None,
            error: None,
        }
    }

    pub fn items(&self) -> Vec<Item> {
        let mut items = vec![
            Item::DownloadDir,
            Item::ListenPort,
            Item::Upnp,
            Item::Notifications,
        ];
        items.extend((0..self.shared.len()).map(Item::Shared));
        items.push(Item::AddShared);
        items
    }

    /// Row index of the shared folder `i`.
    fn shared_index(i: usize) -> usize {
        i + 4
    }

    fn selected_item(&self) -> Item {
        let items = self.items();
        items[self.selected.min(items.len() - 1)]
    }

    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> SettingsAction {
        if self.edit.is_some() {
            return self.on_edit_key(key);
        }
        let count = self.items().len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count - 1)
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            // The UPnP switch toggles instead of opening an editor.
            KeyCode::Enter | KeyCode::Char('e' | ' ') if self.selected_item() == Item::Upnp => {
                self.upnp = !self.upnp;
                return SettingsAction::SetUpnp(self.upnp);
            }
            KeyCode::Enter | KeyCode::Char('e' | ' ')
                if self.selected_item() == Item::Notifications =>
            {
                self.notifications = !self.notifications;
                return SettingsAction::SetNotifications(self.notifications);
            }
            KeyCode::Enter | KeyCode::Char('e') => self.start_edit(self.selected_item()),
            KeyCode::Char('a') => {
                self.selected = count - 1;
                self.start_edit(Item::AddShared);
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Item::Shared(i) = self.selected_item() {
                    self.shared.remove(i);
                    self.error = None;
                    return SettingsAction::SetSharedDirs(self.shared.clone());
                }
            }
            _ => {}
        }
        SettingsAction::None
    }

    fn start_edit(&mut self, item: Item) {
        let text = match item {
            Item::DownloadDir => display_path(&self.download_dir),
            Item::ListenPort => self.listen_port.to_string(),
            Item::Upnp | Item::Notifications => return,
            Item::Shared(i) => display_path(&self.shared[i]),
            Item::AddShared => "~/".to_owned(),
        };
        self.error = None;
        self.edit = Some(Edit { item, text });
    }

    fn on_edit_key(&mut self, key: KeyEvent) -> SettingsAction {
        let edit = self.edit.as_mut().expect("editing");
        match key.code {
            KeyCode::Esc => self.edit = None,
            KeyCode::Tab if edit.item != Item::ListenPort => edit.text = complete(&edit.text),
            KeyCode::Char(c) if edit.item == Item::ListenPort && !c.is_ascii_digit() => {}
            KeyCode::Backspace => {
                edit.text.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                edit.text.clear()
            }
            KeyCode::Char(c) => edit.text.push(c),
            KeyCode::Enter => return self.commit(),
            _ => {}
        }
        SettingsAction::None
    }

    fn commit(&mut self) -> SettingsAction {
        let edit = self.edit.as_ref().expect("editing");
        let text = edit.text.trim();
        if edit.item == Item::ListenPort {
            return match text.parse::<u16>() {
                // Ports below 1024 need root.
                Ok(port) if port >= 1024 => {
                    self.edit = None;
                    self.error = None;
                    SettingsAction::SetListenPort(port)
                }
                _ => {
                    self.error = Some("Enter a port between 1024 and 65535.".to_owned());
                    SettingsAction::None
                }
            };
        }
        if text.is_empty() {
            self.error = Some("Enter a folder path.".to_owned());
            return SettingsAction::None;
        }
        let path = match expand_home(Path::new(text)) {
            Ok(p) if p.is_absolute() => normalize(&p),
            _ => {
                self.error = Some("Use an absolute path or one starting with ~/.".to_owned());
                return SettingsAction::None;
            }
        };

        let action = match edit.item {
            Item::DownloadDir => {
                if path.exists() && !path.is_dir() {
                    self.error = Some(format!("{} is not a folder.", display_path(&path)));
                    return SettingsAction::None;
                }
                self.download_dir = path.clone();
                SettingsAction::SetDownloadDir(path)
            }
            Item::ListenPort | Item::Upnp | Item::Notifications => {
                unreachable!("handled above")
            }
            item @ (Item::Shared(_) | Item::AddShared) => {
                if !path.is_dir() {
                    self.error = Some(format!(
                        "{} is not an existing folder.",
                        display_path(&path)
                    ));
                    return SettingsAction::None;
                }
                let editing = match item {
                    Item::Shared(i) => Some(i),
                    _ => None,
                };
                let duplicate = self
                    .shared
                    .iter()
                    .enumerate()
                    .any(|(i, p)| *p == path && Some(i) != editing);
                if duplicate {
                    self.error = Some("That folder is already shared.".to_owned());
                    return SettingsAction::None;
                }
                match editing {
                    Some(i) => self.shared[i] = path,
                    None => {
                        self.shared.push(path);
                        self.selected = Self::shared_index(self.shared.len() - 1);
                    }
                }
                SettingsAction::SetSharedDirs(self.shared.clone())
            }
        };
        self.edit = None;
        self.error = None;
        action
    }
}

/// Drops a trailing slash so the same folder is not saved twice.
fn normalize(path: &Path) -> PathBuf {
    path.components().collect()
}

/// Completes the last path component to a directory, shell style: a unique
/// match gets a trailing `/`, several matches extend to their common prefix.
/// Hidden folders only match when the typed part starts with a dot.
pub fn complete(text: &str) -> String {
    let (base, prefix) = match text.rfind('/') {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => return text.to_owned(),
    };
    let Ok(dir) = expand_home(Path::new(base)) else {
        return text.to_owned();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return text.to_owned();
    };
    let mut matches: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| {
            name.starts_with(prefix) && (prefix.starts_with('.') || !name.starts_with('.'))
        })
        .collect();
    matches.sort();
    match matches.as_slice() {
        [] => text.to_owned(),
        [only] => format!("{base}{only}/"),
        [first, rest @ ..] => {
            let common = rest.iter().fold(first.as_str(), |acc, m| {
                let len = acc
                    .char_indices()
                    .zip(m.chars())
                    .take_while(|((_, a), b)| a == b)
                    .last()
                    .map_or(0, |((i, a), _)| i + a.len_utf8());
                &acc[..len]
            });
            format!("{base}{common}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("crabseek-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["Music", "Musicals", "Movies", ".hidden"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("Mfile"), b"").unwrap();
        dir
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn type_text(s: &mut Settings, text: &str) {
        for c in text.chars() {
            s.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn completion() {
        let dir = temp_tree("complete");
        let base = format!("{}/", dir.display());
        assert_eq!(complete(&format!("{base}Mo")), format!("{base}Movies/"));
        assert_eq!(complete(&format!("{base}Mu")), format!("{base}Music"));
        assert_eq!(complete(&format!("{base}M")), format!("{base}M"));
        assert_eq!(complete(&format!("{base}.h")), format!("{base}.hidden/"));
        assert_eq!(complete(&format!("{base}zz")), format!("{base}zz"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn add_edit_remove_shared() {
        let dir = temp_tree("edit");
        let mut s = Settings::new(dir.join("dl"), 2234, true, vec![]);

        s.on_key(key(KeyCode::Char('a')));
        s.edit.as_mut().unwrap().text.clear();
        type_text(&mut s, &format!("{}/Music/", dir.display()));
        let action = s.on_key(key(KeyCode::Enter));
        assert_eq!(
            action,
            SettingsAction::SetSharedDirs(vec![dir.join("Music")])
        );

        // Same folder again is refused.
        s.on_key(key(KeyCode::Char('a')));
        s.edit.as_mut().unwrap().text = format!("{}/Music", dir.display());
        assert_eq!(s.on_key(key(KeyCode::Enter)), SettingsAction::None);
        assert!(s.error.as_deref().unwrap().contains("already shared"));
        s.on_key(key(KeyCode::Esc));

        // Missing folders are refused.
        s.on_key(key(KeyCode::Char('a')));
        s.edit.as_mut().unwrap().text = format!("{}/nope", dir.display());
        assert_eq!(s.on_key(key(KeyCode::Enter)), SettingsAction::None);
        s.on_key(key(KeyCode::Esc));

        s.selected = 4;
        assert_eq!(
            s.on_key(key(KeyCode::Char('x'))),
            SettingsAction::SetSharedDirs(vec![])
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn notifications_toggle() {
        let mut s = Settings::new(PathBuf::from("/dl"), 2234, true, vec![]);
        s.selected = 3;
        assert_eq!(
            s.on_key(key(KeyCode::Enter)),
            SettingsAction::SetNotifications(true)
        );
        assert_eq!(
            s.on_key(key(KeyCode::Enter)),
            SettingsAction::SetNotifications(false)
        );
    }

    #[test]
    fn upnp_toggles() {
        let mut s = Settings::new(PathBuf::from("/dl"), 2234, true, vec![]);
        s.selected = 2;
        assert_eq!(
            s.on_key(key(KeyCode::Enter)),
            SettingsAction::SetUpnp(false)
        );
        assert!(!s.is_editing());
        assert_eq!(
            s.on_key(key(KeyCode::Char(' '))),
            SettingsAction::SetUpnp(true)
        );
    }

    #[test]
    fn listen_port() {
        let mut s = Settings::new(PathBuf::from("/dl"), 2234, true, vec![]);
        s.selected = 1;
        s.on_key(key(KeyCode::Enter));
        assert_eq!(s.edit.as_ref().unwrap().text, "2234");
        s.on_key(key(KeyCode::Char('u')));
        for _ in 0..4 {
            s.on_key(key(KeyCode::Backspace));
        }
        type_text(&mut s, "80x");
        assert_eq!(s.edit.as_ref().unwrap().text, "80");
        assert_eq!(s.on_key(key(KeyCode::Enter)), SettingsAction::None);
        type_text(&mut s, "00");
        assert_eq!(
            s.on_key(key(KeyCode::Enter)),
            SettingsAction::SetListenPort(8000)
        );
        // Only confirmed ports are shown; the app updates it on success.
        assert_eq!(s.listen_port, 2234);
    }

    #[test]
    fn download_dir_may_not_exist_yet() {
        let dir = temp_tree("dl");
        let mut s = Settings::new(dir.join("dl"), 2234, true, vec![]);
        s.on_key(key(KeyCode::Enter));
        s.edit.as_mut().unwrap().text = format!("{}/new/place", dir.display());
        assert_eq!(
            s.on_key(key(KeyCode::Enter)),
            SettingsAction::SetDownloadDir(dir.join("new/place"))
        );
        s.on_key(key(KeyCode::Enter));
        s.edit.as_mut().unwrap().text = format!("{}/Mfile", dir.display());
        assert_eq!(s.on_key(key(KeyCode::Enter)), SettingsAction::None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
