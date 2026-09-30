//! The download list shown on the Transfers tab.

use std::time::Instant;

use seekr_net::{DownloadId, DownloadState};

pub struct Transfer {
    pub id: DownloadId,
    pub username: String,
    pub filename: String,
    pub state: DownloadState,
    /// Bytes per second, smoothed.
    pub speed: f64,
    last_sample: Option<(Instant, u64)>,
}

impl Transfer {
    pub fn basename(&self) -> &str {
        self.filename
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&self.filename)
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
}

impl Transfers {
    pub fn update(
        &mut self,
        id: DownloadId,
        username: String,
        filename: String,
        state: DownloadState,
    ) {
        let now = Instant::now();
        let t = match self.list.iter_mut().find(|t| t.id == id) {
            Some(t) => t,
            None => {
                self.list.push(Transfer {
                    id,
                    username,
                    filename,
                    state: state.clone(),
                    speed: 0.0,
                    last_sample: None,
                });
                self.list.last_mut().unwrap()
            }
        };
        if let DownloadState::Transferring { received, .. } = state {
            if let Some((then, before)) = t.last_sample {
                let secs = now.duration_since(then).as_secs_f64();
                if secs > 0.0 && received >= before {
                    let sample = (received - before) as f64 / secs;
                    t.speed = if t.speed == 0.0 {
                        sample
                    } else {
                        0.7 * t.speed + 0.3 * sample
                    };
                }
            }
            t.last_sample = Some((now, received));
        } else {
            t.speed = 0.0;
            t.last_sample = None;
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
        self.clamp();
    }

    pub fn clear_finished(&mut self) -> usize {
        let before = self.list.len();
        self.list.retain(|t| !t.is_finished());
        self.clamp();
        before - self.list.len()
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
            .map(|t| t.speed)
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
