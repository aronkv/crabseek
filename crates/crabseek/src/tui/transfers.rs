//! The download list shown on the Transfers tab.

use std::time::Instant;

use crabseek_net::{DownloadId, DownloadState};

use crate::persist::{SavedDownload, SavedStatus};

/// Smoothed bytes-per-second from progress samples.
#[derive(Default)]
pub struct SpeedMeter {
    pub speed: f64,
    last_sample: Option<(Instant, u64)>,
}

impl SpeedMeter {
    pub fn sample(&mut self, bytes: u64) {
        let now = Instant::now();
        if let Some((then, before)) = self.last_sample {
            let secs = now.duration_since(then).as_secs_f64();
            if secs > 0.0 && bytes >= before {
                let sample = (bytes - before) as f64 / secs;
                self.speed = if self.speed == 0.0 {
                    sample
                } else {
                    0.7 * self.speed + 0.3 * sample
                };
            }
        }
        self.last_sample = Some((now, bytes));
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
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

    pub fn is_finished(&self) -> bool {
        matches!(
            self.state,
            DownloadState::Completed { .. } | DownloadState::Failed { .. }
        )
    }
}

#[derive(Default)]
pub struct Transfers {
    /// In the order they were queued.
    pub list: Vec<Transfer>,
    pub selected: usize,
    /// Something worth saving changed (not just progress).
    pub dirty: bool,
}

impl Transfers {
    pub fn update(
        &mut self,
        id: DownloadId,
        username: String,
        filename: String,
        state: DownloadState,
    ) {
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

    pub fn selected(&self) -> Option<&Transfer> {
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

    pub fn remove(&mut self, id: DownloadId) {
        self.list.retain(|t| t.id != id);
        self.dirty = true;
        self.clamp();
    }

    pub fn clear_finished(&mut self) -> usize {
        let before = self.list.len();
        self.list.retain(|t| !t.is_finished());
        self.dirty = true;
        self.clamp();
        before - self.list.len()
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

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.list.len().saturating_sub(1));
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
        t.selected = 2;
        assert_eq!(t.clear_finished(), 2);
        assert_eq!(t.list.len(), 1);
        assert_eq!(t.selected, 0);
    }
}
