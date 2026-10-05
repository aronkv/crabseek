//! One-line text editing for every text box: a cursor the arrows move,
//! word motions and deletes, and the usual readline keys.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default, Clone)]
pub struct TextInput {
    text: String,
    /// Byte offset into `text`, always on a char boundary.
    cursor: usize,
    /// Display columns scrolled off the left edge; kept by the renderer.
    pub scroll: Cell<usize>,
}

impl TextInput {
    /// `text` with the cursor at its end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            cursor: text.len(),
            text,
            scroll: Cell::new(0),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    /// Replaces the text; the cursor goes to its end.
    pub fn set(&mut self, text: impl Into<String>) {
        *self = Self::new(text);
    }

    /// Empties the box and returns what was in it.
    pub fn take(&mut self) -> String {
        std::mem::take(self).text
    }

    /// Applies an editing key. Returns false for keys it does not edit
    /// with (Enter, Esc, Tab, Up, Down, …), which are the caller's.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            // Terminals send Ctrl-Backspace as Ctrl-h unless the kitty
            // keyboard protocol is on, which reports it as itself.
            KeyCode::Backspace if ctrl || alt => self.delete_to(self.word_start()),
            KeyCode::Char('h' | 'w') if ctrl => self.delete_to(self.word_start()),
            KeyCode::Backspace => self.delete_to(self.prev_char()),
            KeyCode::Delete if ctrl || alt => self.delete_to(self.word_end()),
            KeyCode::Char('d') if alt => self.delete_to(self.word_end()),
            KeyCode::Delete => self.delete_to(self.next_char()),
            KeyCode::Char('u') if ctrl => self.delete_to(0),
            KeyCode::Char('k') if ctrl => self.delete_to(self.text.len()),
            KeyCode::Left if ctrl || alt => self.cursor = self.word_start(),
            KeyCode::Char('b') if alt => self.cursor = self.word_start(),
            KeyCode::Right if ctrl || alt => self.cursor = self.word_end(),
            KeyCode::Char('f') if alt => self.cursor = self.word_end(),
            KeyCode::Left => self.cursor = self.prev_char(),
            KeyCode::Right => self.cursor = self.next_char(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Char(c) if !ctrl && !alt => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => return false,
        }
        true
    }

    /// Deletes between the cursor and `other`, leaving the cursor where the
    /// deleted text started.
    fn delete_to(&mut self, other: usize) {
        let (from, to) = (self.cursor.min(other), self.cursor.max(other));
        self.text.replace_range(from..to, "");
        self.cursor = from;
    }

    fn prev_char(&self) -> usize {
        self.before_cursor()
            .chars()
            .next_back()
            .map_or(0, |c| self.cursor - c.len_utf8())
    }

    fn next_char(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    /// Start of the word before the cursor: back over separators, then
    /// over the word. Punctuation separates, so a path loses one folder.
    fn word_start(&self) -> usize {
        let before = self.before_cursor();
        let trimmed = before.trim_end_matches(|c| !is_word(c));
        trimmed.trim_end_matches(is_word).len()
    }

    /// End of the word after the cursor.
    fn word_end(&self) -> usize {
        let after = &self.text[self.cursor..];
        let trimmed = after.trim_start_matches(|c| !is_word(c));
        self.text.len() - trimmed.trim_start_matches(is_word).len()
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Earlier entries of a text box, walked with Up and Down like a shell's.
#[derive(Debug, Default)]
pub struct History {
    /// Oldest first.
    entries: Vec<String>,
    /// The entry on show while walking; `None` means the typed text.
    pos: Option<usize>,
    /// What was typed before walking started, restored past the newest.
    draft: String,
}

impl History {
    const MAX: usize = 100;

    /// Adds `entry` as the newest; an earlier copy moves up instead.
    pub fn push(&mut self, entry: &str) {
        self.entries.retain(|e| e != entry);
        self.entries.push(entry.to_owned());
        if self.entries.len() > Self::MAX {
            self.entries.remove(0);
        }
        self.pos = None;
    }

    /// Up: shows the next older entry. The box still holds the last
    /// search after it ran, so an entry equal to it is skipped.
    pub fn older(&mut self, input: &mut TextInput) {
        let pos = match self.pos {
            None if self.entries.is_empty() => return,
            None => {
                self.draft = input.text().to_owned();
                let newest = self.entries.len() - 1;
                if newest > 0 && self.entries[newest] == self.draft {
                    newest - 1
                } else {
                    newest
                }
            }
            Some(i) => i.saturating_sub(1),
        };
        self.pos = Some(pos);
        input.set(self.entries[pos].clone());
    }

    /// Down: shows the next newer entry, then what was typed.
    pub fn newer(&mut self, input: &mut TextInput) {
        match self.pos {
            None => {}
            Some(i) if i + 1 < self.entries.len() => {
                self.pos = Some(i + 1);
                input.set(self.entries[i + 1].clone());
            }
            Some(_) => {
                self.pos = None;
                input.set(std::mem::take(&mut self.draft));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut TextInput, code: KeyCode, modifiers: KeyModifiers) -> bool {
        input.on_key(KeyEvent::new(code, modifiers))
    }

    fn key(input: &mut TextInput, code: KeyCode) {
        assert!(
            press(input, code, KeyModifiers::NONE),
            "{code:?} not handled"
        );
    }

    fn ctrl(input: &mut TextInput, code: KeyCode) {
        assert!(press(input, code, KeyModifiers::CONTROL), "Ctrl-{code:?}");
    }

    fn alt(input: &mut TextInput, code: KeyCode) {
        assert!(press(input, code, KeyModifiers::ALT), "Alt-{code:?}");
    }

    fn type_text(input: &mut TextInput, text: &str) {
        for c in text.chars() {
            key(input, KeyCode::Char(c));
        }
    }

    /// The text with `|` at the cursor.
    fn shown(input: &TextInput) -> String {
        format!(
            "{}|{}",
            input.before_cursor(),
            &input.text()[input.cursor..]
        )
    }

    #[test]
    fn arrows_move_and_typing_inserts_at_the_cursor() {
        let mut input = TextInput::new("boards canada");
        for _ in 0..6 {
            key(&mut input, KeyCode::Left);
        }
        type_text(&mut input, "of ");
        assert_eq!(shown(&input), "boards of |canada");
        key(&mut input, KeyCode::Home);
        key(&mut input, KeyCode::Right);
        assert_eq!(shown(&input), "b|oards of canada");
        key(&mut input, KeyCode::End);
        key(&mut input, KeyCode::Right);
        assert_eq!(shown(&input), "boards of canada|");
        ctrl(&mut input, KeyCode::Char('a'));
        key(&mut input, KeyCode::Left);
        assert_eq!(shown(&input), "|boards of canada");
        ctrl(&mut input, KeyCode::Char('e'));
        assert_eq!(shown(&input), "boards of canada|");
    }

    #[test]
    fn backspace_and_delete_at_the_cursor() {
        let mut input = TextInput::new("abc");
        key(&mut input, KeyCode::Left);
        key(&mut input, KeyCode::Backspace);
        assert_eq!(shown(&input), "a|c");
        key(&mut input, KeyCode::Delete);
        assert_eq!(shown(&input), "a|");
        key(&mut input, KeyCode::Delete);
        key(&mut input, KeyCode::Home);
        key(&mut input, KeyCode::Backspace);
        assert_eq!(shown(&input), "|a");
    }

    #[test]
    fn ctrl_backspace_deletes_a_word_however_the_terminal_sends_it() {
        let check = |send: &dyn Fn(&mut TextInput)| {
            let mut input = TextInput::new("boards of canada  ");
            send(&mut input);
            assert_eq!(shown(&input), "boards of |");
            send(&mut input);
            assert_eq!(shown(&input), "boards |");
        };
        // With the kitty keyboard protocol.
        check(&|i| ctrl(i, KeyCode::Backspace));
        // kitty, xterm, alacritty and others without it.
        check(&|i| ctrl(i, KeyCode::Char('h')));
        check(&|i| alt(i, KeyCode::Backspace));
        check(&|i| ctrl(i, KeyCode::Char('w')));
    }

    #[test]
    fn word_motions_and_deletes_stop_at_punctuation() {
        let mut input = TextInput::new("~/Music/Some Album");
        ctrl(&mut input, KeyCode::Backspace);
        assert_eq!(shown(&input), "~/Music/Some |");
        ctrl(&mut input, KeyCode::Left);
        ctrl(&mut input, KeyCode::Left);
        assert_eq!(shown(&input), "~/|Music/Some ");
        ctrl(&mut input, KeyCode::Right);
        assert_eq!(shown(&input), "~/Music|/Some ");
        alt(&mut input, KeyCode::Char('b'));
        alt(&mut input, KeyCode::Char('f'));
        alt(&mut input, KeyCode::Char('f'));
        assert_eq!(shown(&input), "~/Music/Some| ");
        key(&mut input, KeyCode::Home);
        ctrl(&mut input, KeyCode::Delete);
        assert_eq!(shown(&input), "|/Some ");
        alt(&mut input, KeyCode::Char('d'));
        assert_eq!(shown(&input), "| ");
    }

    #[test]
    fn ctrl_u_and_ctrl_k_delete_to_either_end() {
        let mut input = TextInput::new("aphex twin");
        for _ in 0..4 {
            key(&mut input, KeyCode::Left);
        }
        ctrl(&mut input, KeyCode::Char('k'));
        assert_eq!(shown(&input), "aphex |");
        key(&mut input, KeyCode::Left);
        ctrl(&mut input, KeyCode::Char('u'));
        assert_eq!(shown(&input), "| ");
    }

    #[test]
    fn multibyte_text_keeps_the_cursor_on_char_boundaries() {
        let mut input = TextInput::new("árvíz 🦀");
        key(&mut input, KeyCode::Left);
        key(&mut input, KeyCode::Left);
        type_text(&mut input, "é");
        assert_eq!(shown(&input), "árvízé| 🦀");
        ctrl(&mut input, KeyCode::Char('h'));
        assert_eq!(shown(&input), "| 🦀");
        key(&mut input, KeyCode::End);
        key(&mut input, KeyCode::Backspace);
        assert_eq!(shown(&input), " |");
    }

    #[test]
    fn leaves_other_keys_to_the_caller() {
        let mut input = TextInput::new("x");
        for code in [KeyCode::Enter, KeyCode::Esc, KeyCode::Tab, KeyCode::Up] {
            assert!(!press(&mut input, code, KeyModifiers::NONE), "{code:?}");
        }
        // Control keys never type their letter.
        assert!(!press(&mut input, KeyCode::Char('s'), KeyModifiers::ALT));
        assert!(!press(
            &mut input,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        ));
        assert_eq!(input.text(), "x");
        // Shifted letters are typed.
        assert!(press(&mut input, KeyCode::Char('Y'), KeyModifiers::SHIFT));
        assert_eq!(input.take(), "xY");
        assert_eq!(shown(&input), "|");
    }

    #[test]
    fn history_walks_like_a_shell() {
        let mut history = History::default();
        let mut input = TextInput::default();
        history.older(&mut input);
        assert_eq!(input.text(), "", "nothing to recall yet");

        for query in ["aphex", "boards", "autechre", "boards"] {
            history.push(query);
        }
        input.set("half typed");
        history.older(&mut input);
        assert_eq!(input.text(), "boards");
        history.older(&mut input);
        assert_eq!(input.text(), "autechre");
        history.older(&mut input);
        history.older(&mut input);
        assert_eq!(input.text(), "aphex", "stops at the oldest");
        assert_eq!(shown(&input), "aphex|");
        history.newer(&mut input);
        history.newer(&mut input);
        assert_eq!(input.text(), "boards");
        history.newer(&mut input);
        assert_eq!(input.text(), "half typed");
        history.newer(&mut input);
        assert_eq!(input.text(), "half typed");

        // After a search the box still shows it; Up goes past it.
        input.set("boards");
        history.older(&mut input);
        assert_eq!(input.text(), "autechre");
    }
}
