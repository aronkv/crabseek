//! The download list shown on the Transfers tab.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crabseek_net::{DownloadId, DownloadState};

use super::results::cmp_ignore_case;
use crate::persist::{SavedDownload, SavedStatus};

/// Samples closer together than this are skipped: two progress events
/// handled in one burst would otherwise divide by a few microseconds.
const MIN_SAMPLE_GAP: Duration = Duration::from_millis(100);

/// Progress arrives every 250 ms while data flows, so a longer silence
/// means the transfer has stalled and the shown speed should fall.
const STALL_AFTER: Duration = Duration::from_secs(1);

/// Smoothed bytes-per-second from progress samples.
#[derive(Default)]
pub struct SpeedMeter {
    pub speed: f64,
    last_sample: Option<(Instant, u64)>,
}

impl SpeedMeter {
    pub fn sample(&mut self, bytes: u64) {
        self.sample_at(Instant::now(), bytes);
    }

    fn sample_at(&mut self, now: Instant, bytes: u64) {
        if let Some((then, before)) = self.last_sample {
            let elapsed = now.duration_since(then);
            if elapsed < MIN_SAMPLE_GAP && bytes >= before {
                return;
            }
            if bytes >= before {
                let sample = (bytes - before) as f64 / elapsed.as_secs_f64();
                self.speed = if self.speed == 0.0 {
                    sample
                } else {
                    0.7 * self.speed + 0.3 * sample
                };
            }
        }
        self.last_sample = Some((now, bytes));
    }

    /// Halves the speed for every tick without progress, down to zero.
    pub fn decay(&mut self, now: Instant) {
        let Some((then, _)) = self.last_sample else {
            return;
        };
        if self.speed > 0.0 && now.duration_since(then) > STALL_AFTER {
            self.speed /= 2.0;
            if self.speed < 1024.0 {
                self.speed = 0.0;
            }
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

fn folder_key(t: &Transfer) -> (String, String) {
    (t.username.clone(), t.folder().to_owned())
}

pub fn basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

pub struct Transfer {
    pub id: DownloadId,
    pub username: String,
    pub filename: String,
    pub state: DownloadState,
    pub meter: SpeedMeter,
}

impl Transfer {
    pub fn basename(&self) -> &str {
        basename(&self.filename)
    }

    /// The remote folder the file is in, empty for a bare file name.
    pub fn folder(&self) -> &str {
        self.filename
            .rfind(['\\', '/'])
            .map_or("", |i| &self.filename[..i])
    }

    pub fn is_finished(&self) -> bool {
        matches!(
            self.state,
            DownloadState::Completed { .. } | DownloadState::Failed { .. }
        )
    }
}

/// A line of the download list, by index into [`Transfers::list`]. In the
/// grouped view a user and each of their folders get a heading line,
/// carrying the index of the group's first transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferRow {
    User(usize),
    Folder(usize),
    Transfer(usize),
}

impl Default for TransferRow {
    fn default() -> Self {
        Self::Transfer(0)
    }
}

/// What the downloads under a heading are doing.
#[derive(Default)]
pub struct Summary {
    pub total: u64,
    pub done: u64,
    pub failed: u64,
    pub running: u64,
    pub speed: f64,
}

#[derive(Default)]
pub struct Transfers {
    /// In the order they were queued.
    pub list: Vec<Transfer>,
    /// Indices into `list`, so the cursor stays on its row when others
    /// arrive.
    cursor: TransferRow,
    /// Listed in queue order instead of grouped by user and folder.
    flat: bool,
    closed_users: HashSet<String>,
    /// `(username, folder)`.
    closed_folders: HashSet<(String, String)>,
    /// Something worth saving changed (not just progress).
    pub dirty: bool,
    /// Removed from the list; a running one still reports its cancel,
    /// which must not bring the row back.
    removed: HashSet<DownloadId>,
}

impl Transfers {
    pub fn update(
        &mut self,
        id: DownloadId,
        username: String,
        filename: String,
        state: DownloadState,
    ) {
        if self.removed.contains(&id) {
            return;
        }
        let t = match self.list.iter_mut().find(|t| t.id == id) {
            Some(t) => {
                if std::mem::discriminant(&t.state) != std::mem::discriminant(&state) {
                    self.dirty = true;
                }
                t
            }
            None => {
                self.dirty = true;
                self.list.push(Transfer {
                    id,
                    username,
                    filename,
                    state: state.clone(),
                    meter: SpeedMeter::default(),
                });
                self.list.last_mut().unwrap()
            }
        };
        if let DownloadState::Transferring { received, .. } = state {
            t.meter.sample(received);
        } else {
            t.meter.reset();
        }
        t.state = state;
    }

    /// Lets the speeds of stalled transfers fall.
    pub fn tick(&mut self, now: Instant) {
        for t in &mut self.list {
            t.meter.decay(now);
        }
    }

    /// The download under the cursor; `None` on a heading.
    pub fn selected(&self) -> Option<&Transfer> {
        match self.cursor {
            TransferRow::Transfer(i) => self.list.get(i),
            TransferRow::User(_) | TransferRow::Folder(_) => None,
        }
    }

    /// The download under the cursor, or the first one under its heading.
    pub fn cursor_transfer(&self) -> Option<&Transfer> {
        let (TransferRow::User(i) | TransferRow::Folder(i) | TransferRow::Transfer(i)) =
            self.cursor;
        self.list.get(i)
    }

    pub fn cursor(&self) -> TransferRow {
        self.cursor
    }

    pub fn is_flat(&self) -> bool {
        self.flat
    }

    pub fn set_flat(&mut self, flat: bool) {
        self.flat = flat;
        self.fix_cursor();
    }

    /// Indices into `list` in display order: queue order, or grouped by
    /// user, then folder (both alphabetical), then queue order.
    fn order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.list.len()).collect();
        if !self.flat {
            // Stable, so a folder's files keep their queue order.
            order.sort_by(|&a, &b| {
                let (a, b) = (&self.list[a], &self.list[b]);
                cmp_ignore_case(&a.username, &b.username)
                    .then_with(|| cmp_ignore_case(a.folder(), b.folder()))
            });
        }
        order
    }

    /// The lines to show: headings, and the files of open folders.
    pub fn rows(&self) -> Vec<TransferRow> {
        let order = self.order();
        if self.flat {
            return order.into_iter().map(TransferRow::Transfer).collect();
        }
        let mut rows = Vec::with_capacity(order.len() + 2);
        let mut prev: Option<&Transfer> = None;
        let (mut user_open, mut folder_open) = (true, true);
        for i in order {
            let t = &self.list[i];
            let new_user = prev.is_none_or(|p| p.username != t.username);
            let new_folder = new_user || prev.is_some_and(|p| p.folder() != t.folder());
            prev = Some(t);
            if new_user {
                rows.push(TransferRow::User(i));
                user_open = !self.closed_users.contains(&t.username);
            }
            if !user_open {
                continue;
            }
            if new_folder {
                rows.push(TransferRow::Folder(i));
                folder_open = !self.closed_folders.contains(&folder_key(t));
            }
            if folder_open {
                rows.push(TransferRow::Transfer(i));
            }
        }
        rows
    }

    /// Whether a heading shows what is under it; files always count as open.
    pub fn is_open(&self, row: TransferRow) -> bool {
        match row {
            TransferRow::User(i) => self
                .list
                .get(i)
                .is_none_or(|t| !self.closed_users.contains(&t.username)),
            TransferRow::Folder(i) => self
                .list
                .get(i)
                .is_none_or(|t| !self.closed_folders.contains(&folder_key(t))),
            TransferRow::Transfer(_) => true,
        }
    }

    /// Opens or closes a heading in the grouped view.
    fn set_open(&mut self, row: TransferRow, open: bool) {
        if self.flat {
            return;
        }
        match row {
            TransferRow::User(i) => {
                let Some(t) = self.list.get(i) else { return };
                if open {
                    self.closed_users.remove(&t.username);
                } else {
                    self.closed_users.insert(t.username.clone());
                }
            }
            TransferRow::Folder(i) => {
                let Some(t) = self.list.get(i) else { return };
                if open {
                    self.closed_folders.remove(&folder_key(t));
                } else {
                    self.closed_folders.insert(folder_key(t));
                }
            }
            TransferRow::Transfer(_) => return,
        }
        self.fix_cursor();
    }

    /// Opens or closes the heading under the cursor; on a file, closes its
    /// folder.
    pub fn toggle(&mut self) {
        match self.cursor {
            TransferRow::Transfer(_) => self.collapse(),
            row => self.set_open(row, !self.is_open(row)),
        }
    }

    pub fn expand(&mut self) {
        self.set_open(self.cursor, true);
    }

    /// Closes the folder of the file under the cursor, the folder itself,
    /// or once it is closed, its user.
    pub fn collapse(&mut self) {
        let row = match self.cursor {
            TransferRow::Transfer(i) => TransferRow::Folder(i),
            TransferRow::Folder(i) if !self.is_open(TransferRow::Folder(i)) => TransferRow::User(i),
            row => row,
        };
        self.set_open(row, false);
    }

    /// Totals over the downloads under `row`.
    pub fn summary(&self, row: TransferRow) -> Summary {
        let (TransferRow::User(i) | TransferRow::Folder(i) | TransferRow::Transfer(i)) = row;
        let mut sum = Summary::default();
        let Some(head) = self.list.get(i) else {
            return sum;
        };
        let under = |t: &Transfer| match row {
            TransferRow::User(_) => t.username == head.username,
            TransferRow::Folder(_) => t.username == head.username && t.folder() == head.folder(),
            TransferRow::Transfer(_) => t.id == head.id,
        };
        for t in self.list.iter().filter(|t| under(t)) {
            sum.total += 1;
            match t.state {
                DownloadState::Completed { .. } => sum.done += 1,
                DownloadState::Failed { .. } => sum.failed += 1,
                DownloadState::Transferring { .. } => {
                    sum.running += 1;
                    sum.speed += t.meter.speed;
                }
                DownloadState::Queued { .. } => {}
            }
        }
        sum
    }

    /// Where the cursor is among the rows shown.
    fn position(&self) -> Option<usize> {
        self.rows().iter().position(|&r| r == self.cursor)
    }

    pub fn move_by(&mut self, delta: isize) {
        let Some(pos) = self.position() else {
            return;
        };
        self.select_at(pos.saturating_add_signed(delta));
    }

    pub fn remove(&mut self, id: DownloadId) {
        let pos = self.position().unwrap_or(0);
        self.list.retain(|t| t.id != id);
        self.removed.insert(id);
        self.dirty = true;
        self.forget_closed();
        self.select_at(pos);
    }

    pub fn clear_finished(&mut self) -> usize {
        let pos = self.position().unwrap_or(0);
        let before = self.list.len();
        self.list.retain(|t| !t.is_finished());
        self.dirty = true;
        self.forget_closed();
        self.select_at(pos);
        before - self.list.len()
    }

    /// Drops closed headings that have no downloads left, so the same
    /// folder downloaded again shows up open.
    fn forget_closed(&mut self) {
        let list = &self.list;
        self.closed_users
            .retain(|u| list.iter().any(|t| &t.username == u));
        self.closed_folders
            .retain(|(u, f)| list.iter().any(|t| &t.username == u && t.folder() == f));
    }

    /// The list as saved between runs.
    pub fn snapshot(&self) -> Vec<SavedDownload> {
        self.list
            .iter()
            .map(|t| SavedDownload {
                username: t.username.clone(),
                filename: t.filename.clone(),
                status: match &t.state {
                    DownloadState::Queued { .. } | DownloadState::Transferring { .. } => {
                        SavedStatus::Pending
                    }
                    DownloadState::Completed { path } => {
                        SavedStatus::Completed { path: path.clone() }
                    }
                    DownloadState::Failed { reason } => SavedStatus::Failed {
                        reason: reason.clone(),
                    },
                },
            })
            .collect()
    }

    /// Puts the cursor on the `pos`th row shown, or the last one.
    fn select_at(&mut self, pos: usize) {
        let rows = self.rows();
        self.cursor = rows
            .get(pos.min(rows.len().saturating_sub(1)))
            .copied()
            .unwrap_or_default();
    }

    /// Moves a cursor whose row is no longer shown (closed or flattened)
    /// to the folder or user heading it went under, or to the file a
    /// heading stood for.
    fn fix_cursor(&mut self) {
        let rows = self.rows();
        if rows.contains(&self.cursor) {
            return;
        }
        let (TransferRow::User(i) | TransferRow::Folder(i) | TransferRow::Transfer(i)) =
            self.cursor;
        let Some(t) = self.list.get(i) else {
            self.cursor = rows.first().copied().unwrap_or_default();
            return;
        };
        let find = |pick: &dyn Fn(TransferRow) -> bool| rows.iter().copied().find(|&r| pick(r));
        self.cursor = find(&|r| {
            matches!(r, TransferRow::Folder(j)
                if self.list[j].username == t.username && self.list[j].folder() == t.folder())
        })
        .or_else(|| {
            find(&|r| matches!(r, TransferRow::User(j) if self.list[j].username == t.username))
        })
        .or_else(|| find(&|r| r == TransferRow::Transfer(i)))
        .or_else(|| rows.first().copied())
        .unwrap_or_default();
    }

    pub fn active(&self) -> usize {
        self.list.iter().filter(|t| !t.is_finished()).count()
    }

    pub fn total_speed(&self) -> f64 {
        self.list
            .iter()
            .filter(|t| matches!(t.state, DownloadState::Transferring { .. }))
            .map(|t| t.meter.speed)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_samples_do_not_spike_the_speed() {
        let start = Instant::now();
        let mut m = SpeedMeter::default();
        m.sample_at(start, 0);
        m.sample_at(start + Duration::from_secs(1), 1_000_000);
        // Drained in the same burst as the previous one.
        m.sample_at(start + Duration::from_micros(1_000_010), 1_250_000);
        assert_eq!(m.speed, 1_000_000.0);
        // The skipped bytes count towards the next real sample.
        m.sample_at(start + Duration::from_secs(2), 2_000_000);
        assert_eq!(m.speed, 1_000_000.0);
    }

    #[test]
    fn stalled_speed_falls_to_zero() {
        let start = Instant::now();
        let mut m = SpeedMeter::default();
        m.sample_at(start, 0);
        m.sample_at(start + Duration::from_secs(1), 1_000_000);
        m.decay(start + Duration::from_millis(1500));
        assert_eq!(m.speed, 1_000_000.0, "no decay before the stall timeout");
        let mut now = start + Duration::from_millis(2500);
        m.decay(now);
        assert_eq!(m.speed, 500_000.0);
        for _ in 0..20 {
            now += Duration::from_millis(500);
            m.decay(now);
        }
        assert_eq!(m.speed, 0.0);
    }

    fn queued() -> DownloadState {
        DownloadState::Queued { place: None }
    }

    #[test]
    fn update_inserts_then_modifies() {
        let mut t = Transfers::default();
        t.update(1, "u".into(), "a\\b.flac".into(), queued());
        t.update(
            1,
            "u".into(),
            "a\\b.flac".into(),
            DownloadState::Transferring {
                received: 10,
                size: 100,
            },
        );
        assert_eq!(t.list.len(), 1);
        assert_eq!(t.list[0].basename(), "b.flac");
        assert_eq!(t.active(), 1);
    }

    #[test]
    fn clear_finished_keeps_active() {
        let mut t = Transfers::default();
        t.update(1, "u".into(), "a".into(), queued());
        t.update(
            2,
            "u".into(),
            "b".into(),
            DownloadState::Failed { reason: "x".into() },
        );
        t.update(
            3,
            "u".into(),
            "c".into(),
            DownloadState::Completed {
                path: "/tmp/c".into(),
            },
        );
        t.move_by(isize::MAX);
        assert_eq!(t.selected().unwrap().id, 3);
        assert_eq!(t.clear_finished(), 2);
        assert_eq!(t.list.len(), 1);
        assert_eq!(t.selected().unwrap().id, 1);
    }

    /// bob: B\\1; alice: Z\\1, A\\1, Z\\2, in that queue order.
    fn grouped() -> Transfers {
        let mut t = Transfers::default();
        t.update(1, "bob".into(), "B\\1.flac".into(), queued());
        t.update(2, "alice".into(), "Z\\1.flac".into(), queued());
        t.update(3, "alice".into(), "A\\1.flac".into(), queued());
        t.update(4, "alice".into(), "Z\\2.flac".into(), queued());
        t
    }

    #[test]
    fn grouped_by_user_then_folder() {
        use TransferRow::*;
        let mut t = grouped();
        assert_eq!(
            t.rows(),
            [
                User(2),
                Folder(2),
                Transfer(2),
                Folder(1),
                Transfer(1),
                Transfer(3),
                User(0),
                Folder(0),
                Transfer(0),
            ]
        );
        assert_eq!(t.summary(Folder(1)).total, 2);
        assert_eq!(t.summary(User(2)).total, 3);

        // Headings take the cursor too; they stand for their first file.
        t.move_by(isize::MIN);
        assert_eq!(t.cursor(), User(2));
        assert!(t.selected().is_none());
        assert_eq!(t.cursor_transfer().unwrap().id, 3);
        t.move_by(4);
        assert_eq!(t.selected().unwrap().id, 2);
        // The cursor stays on its download across views.
        t.set_flat(true);
        assert_eq!(
            t.rows(),
            [Transfer(0), Transfer(1), Transfer(2), Transfer(3)]
        );
        assert_eq!(t.selected().unwrap().id, 2);
        t.set_flat(false);
        assert_eq!(t.selected().unwrap().id, 2);

        // Removing moves the cursor to the next row shown.
        t.remove(2);
        assert_eq!(t.selected().unwrap().id, 4);
        t.remove(4);
        assert_eq!(t.cursor(), Folder(0));
    }

    #[test]
    fn headings_open_and_close() {
        use TransferRow::*;
        let mut t = grouped();
        t.move_by(isize::MIN);
        t.move_by(4);
        assert_eq!(t.selected().unwrap().id, 2);

        // On a file, closing closes its folder; again, its user.
        t.collapse();
        assert_eq!(t.cursor(), Folder(1));
        assert!(!t.is_open(Folder(1)));
        assert_eq!(
            t.rows(),
            [
                User(2),
                Folder(2),
                Transfer(2),
                Folder(1),
                User(0),
                Folder(0),
                Transfer(0)
            ]
        );
        t.collapse();
        assert_eq!(t.cursor(), User(2));
        assert_eq!(t.rows(), [User(2), User(0), Folder(0), Transfer(0)]);

        // The user opens with its folders as they were.
        t.toggle();
        assert_eq!(t.rows().len(), 7);
        t.move_by(3);
        assert_eq!(t.cursor(), Folder(1));
        t.expand();
        assert_eq!(t.rows().len(), 9);
        t.toggle();
        assert!(!t.is_open(Folder(1)));

        // Flat shows everything; back to grouped, a hidden file's cursor
        // goes to its closed folder.
        t.set_flat(true);
        assert_eq!(t.cursor(), Transfer(1));
        t.set_flat(false);
        assert_eq!(t.cursor(), Folder(1));

        // A folder emptied and downloaded again shows up open.
        t.remove(4);
        t.move_by(isize::MIN);
        t.move_by(3);
        assert!(!t.is_open(Folder(1)));
        t.remove(2);
        t.update(5, "alice".into(), "Z\\3.flac".into(), queued());
        assert!(t.rows().contains(&Transfer(2)));
    }
}
