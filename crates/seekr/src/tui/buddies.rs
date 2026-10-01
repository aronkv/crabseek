//! The buddy list on the Buddies tab: users we watch on the server, with
//! their status and share stats. Online buddies come first.

use seekr_proto::server::{OnlineStatus, UserStats, WatchedUser};

pub struct Buddy {
    pub username: String,
    /// `None` until the server answered our `WatchUser`.
    pub known: Option<Known>,
}

pub enum Known {
    Exists {
        status: OnlineStatus,
        stats: UserStats,
        country: Option<String>,
    },
    /// The server has no account with this name.
    Missing,
}

impl Buddy {
    pub fn status(&self) -> OnlineStatus {
        match &self.known {
            Some(Known::Exists { status, .. }) => *status,
            _ => OnlineStatus::Offline,
        }
    }
}

#[derive(Default)]
pub struct Buddies {
    pub list: Vec<Buddy>,
    pub selected: usize,
    /// The username being typed after `a`.
    pub adding: Option<String>,
}

impl Buddies {
    pub fn new(names: Vec<String>) -> Self {
        let mut buddies = Self::default();
        for name in names {
            buddies.add(name);
        }
        buddies
    }

    /// Adds `username` unless it is already on the list.
    pub fn add(&mut self, username: String) -> bool {
        if self.contains(&username) {
            return false;
        }
        self.list.push(Buddy {
            username,
            known: None,
        });
        self.sort();
        true
    }

    pub fn contains(&self, username: &str) -> bool {
        self.list.iter().any(|b| b.username == username)
    }

    pub fn remove_selected(&mut self) -> Option<String> {
        if self.selected >= self.list.len() {
            return None;
        }
        let buddy = self.list.remove(self.selected);
        self.selected = self.selected.min(self.list.len().saturating_sub(1));
        Some(buddy.username)
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.list.iter().map(|b| b.username.clone()).collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    pub fn online(&self) -> usize {
        self.list
            .iter()
            .filter(|b| b.status() != OnlineStatus::Offline)
            .count()
    }

    pub fn on_watch(&mut self, username: &str, user: Option<WatchedUser>) {
        self.update(username, |b| {
            b.known = Some(match user {
                Some(u) => Known::Exists {
                    status: u.status,
                    stats: u.stats,
                    country: u.country,
                },
                None => Known::Missing,
            });
        });
    }

    pub fn on_status(&mut self, username: &str, new: OnlineStatus) {
        self.update(username, |b| match &mut b.known {
            Some(Known::Exists {
                status, country, ..
            }) => {
                *status = new;
                if new == OnlineStatus::Offline {
                    *country = None;
                }
            }
            known => {
                *known = Some(Known::Exists {
                    status: new,
                    stats: UserStats::default(),
                    country: None,
                })
            }
        });
    }

    pub fn on_stats(&mut self, username: &str, new: UserStats) {
        self.update(username, |b| {
            if let Some(Known::Exists { stats, .. }) = &mut b.known {
                *stats = new;
            }
        });
    }

    /// Changes one buddy and re-sorts, keeping the cursor on the same row.
    fn update(&mut self, username: &str, change: impl FnOnce(&mut Buddy)) {
        if let Some(b) = self.list.iter_mut().find(|b| b.username == username) {
            change(b);
            self.sort();
        }
    }

    fn sort(&mut self) {
        let selected = self.selected().map(|b| b.username.clone());
        self.list.sort_by(|a, b| {
            b.status()
                .cmp(&a.status())
                .then_with(|| a.username.to_lowercase().cmp(&b.username.to_lowercase()))
        });
        if let Some(name) = selected {
            self.selected = self
                .list
                .iter()
                .position(|b| b.username == name)
                .unwrap_or(0);
        }
    }

    pub fn selected(&self) -> Option<&Buddy> {
        self.list.get(self.selected)
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.list.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.list.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watched(status: OnlineStatus) -> Option<WatchedUser> {
        Some(WatchedUser {
            status,
            stats: UserStats::default(),
            country: None,
        })
    }

    #[test]
    fn online_first_and_cursor_follows() {
        let mut b = Buddies::new(vec!["carol".into(), "alice".into(), "Bob".into()]);
        assert_eq!(b.names(), ["alice", "Bob", "carol"]);
        assert!(!b.add("alice".into()));

        b.selected = 0; // alice
        b.on_watch("carol", watched(OnlineStatus::Online));
        b.on_watch("Bob", watched(OnlineStatus::Away));
        let order: Vec<&str> = b.list.iter().map(|b| b.username.as_str()).collect();
        assert_eq!(order, ["carol", "Bob", "alice"]);
        assert_eq!(b.selected().unwrap().username, "alice");
        assert_eq!(b.online(), 2);

        b.on_status("carol", OnlineStatus::Offline);
        assert_eq!(b.list[0].username, "Bob");
        assert_eq!(b.online(), 1);
    }

    #[test]
    fn remove_keeps_cursor_in_range() {
        let mut b = Buddies::new(vec!["a".into(), "b".into()]);
        b.selected = 1;
        assert_eq!(b.remove_selected().as_deref(), Some("b"));
        assert_eq!(b.selected, 0);
        assert_eq!(b.remove_selected().as_deref(), Some("a"));
        assert_eq!(b.remove_selected(), None);
    }
}
