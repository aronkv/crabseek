//! Desktop notifications (freedesktop, over D-Bus), off unless the user
//! turns them on in Settings.
//!
//! Finished downloads are batched: an album finishes as a burst of files,
//! which should be one notification, not twenty.

use std::time::{Duration, Instant};

/// Downloads finishing within this window share one notification.
const BATCH_WINDOW: Duration = Duration::from_secs(3);

/// Shows a notification without blocking the UI. Failures (no
/// notification daemon, no session bus) only go to the log.
pub fn send(summary: String, body: String) {
    tokio::task::spawn_blocking(move || {
        let result = notify_rust::Notification::new()
            .appname("crabseek")
            .summary(&summary)
            .body(&body)
            .icon("folder-download")
            .show();
        if let Err(e) = result {
            tracing::info!(%e, "desktop notification failed");
        }
    });
}

/// Collects finished downloads until the burst is over.
#[derive(Default)]
pub struct DownloadBatch {
    names: Vec<String>,
    last: Option<Instant>,
}

impl DownloadBatch {
    pub fn push(&mut self, name: String, now: Instant) {
        self.names.push(name);
        self.last = Some(now);
    }

    /// The notification text once no download has finished for a while.
    pub fn take_due(&mut self, now: Instant) -> Option<(String, String)> {
        let last = self.last?;
        if now.duration_since(last) < BATCH_WINDOW {
            return None;
        }
        self.last = None;
        let names = std::mem::take(&mut self.names);
        Some(match names.as_slice() {
            [one] => ("Download finished".to_owned(), one.clone()),
            [first, rest @ ..] => (
                format!("{} downloads finished", names.len()),
                format!("{first} and {} more", rest.len()),
            ),
            [] => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_a_burst() {
        let mut b = DownloadBatch::default();
        let t = Instant::now();
        assert_eq!(b.take_due(t), None);
        b.push("01.flac".into(), t);
        b.push("02.flac".into(), t + Duration::from_secs(1));
        assert_eq!(
            b.take_due(t + Duration::from_secs(2)),
            None,
            "still arriving"
        );
        assert_eq!(
            b.take_due(t + Duration::from_secs(5)),
            Some(("2 downloads finished".into(), "01.flac and 1 more".into()))
        );
        assert_eq!(b.take_due(t + Duration::from_secs(10)), None);

        b.push("solo.mp3".into(), t);
        assert_eq!(
            b.take_due(t + BATCH_WINDOW),
            Some(("Download finished".into(), "solo.mp3".into()))
        );
    }
}

/// Shows a real notification; run with `--ignored` on a desktop session.
#[cfg(test)]
mod live {
    #[test]
    #[ignore]
    fn shows_on_the_desktop() {
        notify_rust::Notification::new()
            .appname("crabseek")
            .summary("crabseek")
            .body("Test notification: desktop notifications work")
            .icon("folder-download")
            .show()
            .expect("notification daemon reachable");
    }
}
