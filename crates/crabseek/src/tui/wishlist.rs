//! The wishlist: saved searches that run in the background, one per server
//! `WishlistInterval` (usually 12 minutes), round-robin. Results collect
//! per query, and files not seen before count as new.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crabseek_proto::search::SearchResponse;

use super::results::Results;

/// The server's usual interval until it tells us otherwise.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(12 * 60);
/// The first query runs shortly after login, once we are settled.
const FIRST_RUN_DELAY: Duration = Duration::from_secs(30);
/// Results trickle in for a while; older runs' tokens are dropped.
const KEEP_TOKENS: usize = 2;

pub struct WishItem {
    pub query: String,
    pub results: Results,
    /// Files found since the user last opened this item.
    pub new: usize,
    pub last_run: Option<Instant>,
    seen: HashSet<(String, String)>,
    tokens: Vec<u32>,
}

impl WishItem {
    fn new(query: String) -> Self {
        Self {
            query,
            results: Results::default(),
            new: 0,
            last_run: None,
            seen: HashSet::new(),
            tokens: Vec::new(),
        }
    }

    pub fn latest_token(&self) -> Option<u32> {
        self.tokens.last().copied()
    }
}

pub struct Wishlist {
    pub items: Vec<WishItem>,
    pub selected: usize,
    pub interval: Duration,
    next_due: Instant,
    /// Round-robin position.
    next: usize,
    /// A query being typed on the Wishlist tab.
    pub adding: Option<String>,
}

impl Default for Wishlist {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Wishlist {
    pub fn new(queries: Vec<String>) -> Self {
        Self {
            items: queries.into_iter().map(WishItem::new).collect(),
            selected: 0,
            interval: DEFAULT_INTERVAL,
            next_due: Instant::now() + FIRST_RUN_DELAY,
            next: 0,
            adding: None,
        }
    }

    pub fn queries(&self) -> Vec<String> {
        self.items.iter().map(|i| i.query.clone()).collect()
    }

    /// Returns false if the query is empty or already on the list.
    pub fn add(&mut self, query: &str) -> bool {
        let query = query.trim();
        if query.is_empty()
            || self
                .items
                .iter()
                .any(|i| i.query.eq_ignore_ascii_case(query))
        {
            return false;
        }
        self.items.push(WishItem::new(query.to_owned()));
        true
    }

    /// Removes the selected query; returns it and its search tokens.
    pub fn remove_selected(&mut self) -> Option<(String, Vec<u32>)> {
        if self.selected >= self.items.len() {
            return None;
        }
        let item = self.items.remove(self.selected);
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        Some((item.query, item.tokens))
    }

    pub fn move_by(&mut self, delta: isize) {
        if !self.items.is_empty() {
            self.selected = self
                .selected
                .saturating_add_signed(delta)
                .min(self.items.len() - 1);
        }
    }

    pub fn set_interval(&mut self, secs: u32) {
        self.interval = Duration::from_secs(u64::from(secs.max(60)));
    }

    /// The item whose turn it is, if the interval has passed.
    pub fn due(&mut self, now: Instant) -> Option<usize> {
        if self.items.is_empty() || now < self.next_due {
            return None;
        }
        let index = self.next % self.items.len();
        self.next = index + 1;
        Some(index)
    }

    /// Records that `index` ran with `token` (scheduled or by hand) and
    /// restarts the interval. Returns a token that is no longer needed.
    pub fn mark_run(&mut self, index: usize, token: u32, now: Instant) -> Option<u32> {
        self.next_due = now + self.interval;
        let item = &mut self.items[index];
        item.last_run = Some(now);
        item.tokens.push(token);
        (item.tokens.len() > KEEP_TOKENS).then(|| item.tokens.remove(0))
    }

    /// Time until the next scheduled query.
    pub fn next_in(&self, now: Instant) -> Duration {
        self.next_due.saturating_duration_since(now)
    }

    /// Files a wishlist search found; returns the query and how many of
    /// the files are new, or `None` if the token is not ours.
    pub fn on_result(&mut self, resp: SearchResponse) -> Option<(String, usize)> {
        let item = self
            .items
            .iter_mut()
            .find(|i| i.tokens.contains(&resp.token))?;
        let new = resp
            .files
            .iter()
            .filter(|f| {
                item.seen
                    .insert((resp.username.clone(), f.filename.clone()))
            })
            .count();
        if new > 0 {
            item.new += new;
            item.results.add(resp);
        }
        Some((item.query.clone(), new))
    }

    pub fn total_new(&self) -> usize {
        self.items.iter().map(|i| i.new).sum()
    }
}

#[cfg(test)]
mod tests {
    use crabseek_proto::search::SearchFile;

    use super::*;

    fn resp(token: u32, user: &str, files: &[&str]) -> SearchResponse {
        SearchResponse {
            username: user.into(),
            token,
            files: files
                .iter()
                .map(|f| SearchFile {
                    filename: (*f).into(),
                    ..Default::default()
                })
                .collect(),
            slot_free: true,
            avg_speed: 0,
            queue_length: 0,
            private_files: vec![],
        }
    }

    #[test]
    fn round_robin_after_the_interval() {
        let mut w = Wishlist::new(vec!["a".into(), "b".into()]);
        let start = Instant::now();
        assert_eq!(w.due(start), None, "waits for the first-run delay");
        let t0 = start + FIRST_RUN_DELAY;
        assert_eq!(w.due(t0), Some(0));
        w.mark_run(0, 1, t0);
        assert_eq!(w.due(t0 + Duration::from_secs(60)), None);
        let t1 = t0 + DEFAULT_INTERVAL;
        assert_eq!(w.due(t1), Some(1));
        w.mark_run(1, 2, t1);
        assert_eq!(w.due(t1 + DEFAULT_INTERVAL), Some(0));
    }

    #[test]
    fn counts_only_new_files() {
        let mut w = Wishlist::new(vec!["boc".into()]);
        let now = Instant::now();
        w.mark_run(0, 7, now);
        assert_eq!(
            w.on_result(resp(7, "u", &["a\\1", "a\\2"])),
            Some(("boc".into(), 2))
        );
        // The next run finds the same files again plus one more.
        w.mark_run(0, 8, now);
        assert_eq!(
            w.on_result(resp(8, "u", &["a\\1", "a\\2", "a\\3"])),
            Some(("boc".into(), 1))
        );
        assert_eq!(w.total_new(), 3);
        assert_eq!(w.on_result(resp(99, "u", &["x"])), None);
    }

    #[test]
    fn old_tokens_are_released() {
        let mut w = Wishlist::new(vec!["q".into()]);
        let now = Instant::now();
        assert_eq!(w.mark_run(0, 1, now), None);
        assert_eq!(w.mark_run(0, 2, now), None);
        assert_eq!(w.mark_run(0, 3, now), Some(1));
    }

    #[test]
    fn add_remove() {
        let mut w = Wishlist::default();
        assert!(w.add("Boards of Canada"));
        assert!(!w.add("boards of canada"));
        assert!(!w.add("  "));
        w.mark_run(0, 5, Instant::now());
        assert_eq!(
            w.remove_selected(),
            Some(("Boards of Canada".into(), vec![5]))
        );
        assert!(w.items.is_empty());
    }
}
