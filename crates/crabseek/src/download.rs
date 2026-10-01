//! `crabseek download`: queue one file and show its progress until it
//! finishes.

use std::io::Write;
use std::time::Instant;

use crabseek_net::{Client, DownloadState, Event};
use tokio::sync::mpsc;

use crate::search::human_size;

pub async fn run(
    client: Client,
    mut events: mpsc::UnboundedReceiver<Event>,
    username: &str,
    filename: &str,
) -> anyhow::Result<()> {
    let download = client.download(username, filename)?;
    println!("queueing {filename} from {username}...");

    // Speed is measured from the first progress report of this run, so a
    // resumed download does not look faster than it is.
    let mut start: Option<(Instant, u64)> = None;
    let mut last_state: Option<DownloadState> = None;
    while let Some(event) = events.recv().await {
        match event {
            Event::Download { id, state, .. } if id == download => {
                match &state {
                    DownloadState::Queued { place } => {
                        if last_state.as_ref() != Some(&state) {
                            match place {
                                Some(place) => println!("queued, place {place}"),
                                None => println!("queued, waiting for the peer..."),
                            }
                        }
                    }
                    DownloadState::Transferring { received, size } => {
                        let (t0, r0) = *start.get_or_insert((Instant::now(), *received));
                        let secs = t0.elapsed().as_secs_f64();
                        let speed = if secs > 0.5 {
                            format!("{}/s", human_size(((received - r0) as f64 / secs) as u64))
                        } else {
                            String::new()
                        };
                        let pct = if *size > 0 {
                            *received as f64 / *size as f64 * 100.0
                        } else {
                            100.0
                        };
                        eprint!(
                            "\r\x1b[K{pct:5.1}%  {} / {}  {speed}",
                            human_size(*received),
                            human_size(*size)
                        );
                        let _ = std::io::stderr().flush();
                    }
                    DownloadState::Completed { path } => {
                        eprintln!();
                        println!("saved to {}", path.display());
                        return Ok(());
                    }
                    DownloadState::Failed { reason } => {
                        eprintln!();
                        anyhow::bail!("download failed: {reason}");
                    }
                }
                last_state = Some(state);
            }
            Event::PeerConnected {
                username: u,
                method,
            } if u == username => {
                println!("connected to {u} ({method:?})");
            }
            Event::ServerClosed { reason } => anyhow::bail!("server connection closed: {reason}"),
            _ => {}
        }
    }
    Ok(())
}
