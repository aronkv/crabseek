//! The upload list shown on the Uploads tab.

use crabseek_net::{UploadId, UploadState};

use super::transfers::{SpeedMeter, basename};

pub struct UploadRow {
    pub id: UploadId,
    pub username: String,
    pub filename: String,
    pub state: UploadState,
    pub meter: SpeedMeter,
}

impl UploadRow {
    pub fn basename(&self) -> &str {
        basename(&self.filename)
    }

    pub fn is_finished(&self) -> bool {
        matches!(
            self.state,
            UploadState::Completed | UploadState::Failed { .. }
        )
    }
}

#[derive(Default)]
pub struct Uploads {
    pub list: Vec<UploadRow>,
    pub selected: usize,
    pub completed: usize,
}

impl Uploads {
    pub fn update(&mut self, id: UploadId, username: String, filename: String, state: UploadState) {
        let row = match self.list.iter_mut().position(|u| u.id == id) {
            Some(i) => &mut self.list[i],
            None => {
                self.list.push(UploadRow {
                    id,
                    username,
                    filename,
                    state: state.clone(),
                    meter: SpeedMeter::default(),
                });
                self.list.last_mut().unwrap()
            }
        };
        if state == UploadState::Completed && row.state != UploadState::Completed {
            self.completed += 1;
        }
        match state {
            UploadState::Transferring { sent, .. } => row.meter.sample(sent),
            _ => row.meter.reset(),
        }
        row.state = state;
    }

    pub fn selected(&self) -> Option<&UploadRow> {
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

    pub fn clear_finished(&mut self) -> usize {
        let before = self.list.len();
        self.list.retain(|u| !u.is_finished());
        self.selected = self.selected.min(self.list.len().saturating_sub(1));
        before - self.list.len()
    }

    pub fn active(&self) -> usize {
        self.list
            .iter()
            .filter(|u| {
                matches!(
                    u.state,
                    UploadState::Starting | UploadState::Transferring { .. }
                )
            })
            .count()
    }

    pub fn total_speed(&self) -> f64 {
        self.list
            .iter()
            .filter(|u| matches!(u.state, UploadState::Transferring { .. }))
            .map(|u| u.meter.speed)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_clears() {
        let mut u = Uploads::default();
        u.update(1, "a".into(), "M\\x.flac".into(), UploadState::Queued);
        u.update(
            2,
            "b".into(),
            "M\\y.flac".into(),
            UploadState::Transferring { sent: 1, size: 10 },
        );
        u.update(1, "a".into(), "M\\x.flac".into(), UploadState::Completed);
        u.update(1, "a".into(), "M\\x.flac".into(), UploadState::Completed);
        assert_eq!(u.completed, 1);
        assert_eq!(u.active(), 1);
        assert_eq!(u.clear_finished(), 1);
        assert_eq!(u.list[0].basename(), "y.flac");
    }
}
