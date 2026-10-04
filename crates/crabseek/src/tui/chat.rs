//! Private messages: one conversation per user, newest conversation first.
//! Kept in `chats.json` (the last messages per user) between runs.

use serde::{Deserialize, Serialize};

/// Older messages are dropped beyond this, per conversation.
const MAX_MESSAGES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Unix seconds.
    pub timestamp: u32,
    pub from_me: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub username: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub unread: usize,
}

/// What is being typed on the Chat tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatInput {
    /// A message to the selected conversation.
    Message(String),
    /// The username of a new conversation.
    NewUser(String),
}

#[derive(Default)]
pub struct Chats {
    /// Most recently active first.
    pub list: Vec<Conversation>,
    pub selected: usize,
    pub input: Option<ChatInput>,
    /// Something worth saving changed.
    pub dirty: bool,
    /// Lines scrolled up from the newest message of the open conversation.
    /// The renderer clamps it to what the conversation has.
    pub scroll: usize,
    /// Height of the message view at the last draw: one PageUp's worth.
    pub page: usize,
}

impl Chats {
    pub fn new(list: Vec<Conversation>) -> Self {
        Self {
            list,
            ..Self::default()
        }
    }

    pub fn selected(&self) -> Option<&Conversation> {
        self.list.get(self.selected)
    }

    pub fn total_unread(&self) -> usize {
        self.list.iter().map(|c| c.unread).sum()
    }

    pub fn move_by(&mut self, delta: isize) {
        if !self.list.is_empty() {
            self.select(
                self.selected
                    .saturating_add_signed(delta)
                    .min(self.list.len() - 1),
            );
        }
    }

    /// Shows conversation `index`, from its newest message if it is
    /// another one.
    fn select(&mut self, index: usize) {
        if index != self.selected {
            self.scroll = 0;
        }
        self.selected = index;
    }

    /// Scrolls the open conversation by `pages` (positive is older).
    pub fn scroll_pages(&mut self, pages: isize) {
        let step = self.page.saturating_sub(1).max(1) as isize;
        self.scroll = self.scroll.saturating_add_signed(pages * step);
    }

    /// Moves `username`'s conversation (created if needed) to the top and
    /// returns it. The selection stays on the conversation it was on.
    fn bring_to_top(&mut self, username: &str) -> &mut Conversation {
        let selected_name = self.selected().map(|c| c.username.clone());
        let conv = match self.list.iter().position(|c| c.username == username) {
            Some(i) => self.list.remove(i),
            None => Conversation {
                username: username.to_owned(),
                messages: Vec::new(),
                unread: 0,
            },
        };
        self.list.insert(0, conv);
        if let Some(name) = selected_name {
            self.selected = self
                .list
                .iter()
                .position(|c| c.username == name)
                .unwrap_or(0);
        }
        self.dirty = true;
        &mut self.list[0]
    }

    fn push(conv: &mut Conversation, message: ChatMessage) {
        conv.messages.push(message);
        if conv.messages.len() > MAX_MESSAGES {
            let extra = conv.messages.len() - MAX_MESSAGES;
            conv.messages.drain(..extra);
        }
    }

    /// A message from `username`. `seen` means the user is looking at that
    /// conversation right now, so it does not count as unread.
    pub fn receive(&mut self, username: &str, text: String, timestamp: u32, seen: bool) {
        let conv = self.bring_to_top(username);
        Self::push(
            conv,
            ChatMessage {
                timestamp,
                from_me: false,
                text,
            },
        );
        if !seen {
            conv.unread += 1;
        }
    }

    pub fn sent(&mut self, username: &str, text: String, timestamp: u32) {
        let conv = self.bring_to_top(username);
        Self::push(
            conv,
            ChatMessage {
                timestamp,
                from_me: true,
                text,
            },
        );
        // Back to the newest message, where the one just sent is.
        self.scroll = 0;
        self.selected = 0;
    }

    /// Selects `username`'s conversation, creating an empty one if needed.
    pub fn open(&mut self, username: &str) {
        match self.list.iter().position(|c| c.username == username) {
            Some(i) => self.select(i),
            None => {
                self.bring_to_top(username);
                self.selected = 0;
                self.scroll = 0;
            }
        }
        self.mark_selected_read();
    }

    pub fn mark_selected_read(&mut self) {
        if let Some(conv) = self.list.get_mut(self.selected)
            && conv.unread > 0
        {
            conv.unread = 0;
            self.dirty = true;
        }
    }

    pub fn remove_selected(&mut self) -> Option<String> {
        if self.selected >= self.list.len() {
            return None;
        }
        let conv = self.list.remove(self.selected);
        self.selected = self.selected.min(self.list.len().saturating_sub(1));
        self.scroll = 0;
        self.dirty = true;
        Some(conv.username)
    }
}

/// The saved conversations; empty if there are none or the file is
/// unreadable.
pub fn load(path: &std::path::Path) -> Vec<Conversation> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(%e, "saved chats are corrupt");
            Vec::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            tracing::warn!(%e, "could not read the saved chats");
            Vec::new()
        }
    }
}

/// Saved with mode 600: private messages are private.
pub fn save(path: &std::path::Path, list: &[Conversation]) -> anyhow::Result<()> {
    crate::config::write_private(path, &serde_json::to_string(list)?)
}

/// Unix seconds now.
pub fn now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

/// `HH:MM` for today, `MM-DD HH:MM` for older messages, in local time.
pub fn format_time(timestamp: u32) -> String {
    use chrono::{Local, TimeZone};
    let Some(time) = Local.timestamp_opt(i64::from(timestamp), 0).single() else {
        return String::new();
    };
    if time.date_naive() == Local::now().date_naive() {
        time.format("%H:%M").to_string()
    } else {
        time.format("%m-%d %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_conversation_first_and_unread() {
        let mut c = Chats::default();
        c.receive("alice", "hi".into(), 1, false);
        c.receive("bob", "yo".into(), 2, false);
        assert_eq!(c.list[0].username, "bob");
        assert_eq!(c.total_unread(), 2);
        // The selection follows its conversation when another moves up.
        c.selected = 1; // alice
        c.receive("bob", "again".into(), 3, false);
        assert_eq!(c.selected().unwrap().username, "alice");

        c.open("bob");
        assert_eq!(c.selected().unwrap().username, "bob");
        assert_eq!(c.list[0].unread, 0);
        assert_eq!(c.total_unread(), 1);

        c.receive("bob", "seen".into(), 4, true);
        assert_eq!(c.list[0].unread, 0);
    }

    #[test]
    fn sending_and_opening_new() {
        let mut c = Chats::default();
        c.open("carol");
        assert_eq!(c.list.len(), 1);
        c.sent("carol", "szia".into(), 5);
        assert!(c.list[0].messages[0].from_me);
        assert_eq!(c.remove_selected(), Some("carol".into()));
        assert!(c.list.is_empty());
    }

    #[test]
    fn keeps_the_last_messages() {
        let mut c = Chats::default();
        for i in 0..(MAX_MESSAGES + 10) {
            c.receive("a", i.to_string(), i as u32, true);
        }
        let msgs = &c.list[0].messages;
        assert_eq!(msgs.len(), MAX_MESSAGES);
        assert_eq!(msgs[0].text, "10");
    }
}
