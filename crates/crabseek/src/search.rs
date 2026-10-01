//! `crabseek search`: collect results for a while, then print them grouped by
//! user and folder.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;

use crabseek_net::{Client, Event};
use crabseek_proto::search::{SearchFile, SearchResponse};
use tokio::sync::mpsc;

pub async fn run(
    client: Client,
    mut events: mpsc::UnboundedReceiver<Event>,
    query: &str,
    secs: u64,
    top: usize,
    full_paths: bool,
    wishlist: bool,
) -> anyhow::Result<()> {
    let token = if wishlist {
        client.wishlist_search(query)?
    } else {
        client.search(query)?
    };
    println!("searching for {query:?} for {secs}s...");

    let mut results: Vec<SearchResponse> = Vec::new();
    let deadline = tokio::time::sleep(Duration::from_secs(secs));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            event = events.recv() => match event {
                Some(Event::SearchResult(resp)) if resp.token == token && !resp.files.is_empty() => {
                    results.push(resp);
                    let files: usize = results.iter().map(|r| r.files.len()).sum();
                    eprint!("\r{} users, {files} files", results.len());
                    let _ = std::io::stderr().flush();
                }
                Some(Event::ServerClosed { reason }) => {
                    eprintln!("\nserver connection closed: {reason}");
                    break;
                }
                Some(_) => {}
                None => break,
            },
        }
    }
    client.stop_search(token)?;
    eprintln!();

    if results.is_empty() {
        println!("no results");
        return Ok(());
    }

    // Users who can start uploading right away first, then short queues,
    // then fast connections.
    results.sort_by(|a, b| {
        b.slot_free
            .cmp(&a.slot_free)
            .then(a.queue_length.cmp(&b.queue_length))
            .then(b.avg_speed.cmp(&a.avg_speed))
    });

    for resp in results.iter().take(top) {
        print_user(resp, full_paths);
    }
    if results.len() > top {
        println!(
            "\n... and {} more users (use --top to show more)",
            results.len() - top
        );
    }
    Ok(())
}

fn print_user(resp: &SearchResponse, full_paths: bool) {
    let availability = if resp.slot_free {
        "free slot".to_owned()
    } else {
        format!("queue {}", resp.queue_length)
    };
    println!(
        "\n{}  [{availability}, {}/s]",
        resp.username,
        human_size(resp.avg_speed.into())
    );

    let mut folders: BTreeMap<&str, Vec<&SearchFile>> = BTreeMap::new();
    for file in &resp.files {
        folders.entry(file.folder()).or_default().push(file);
    }
    for (folder, mut files) in folders {
        println!("  {folder}");
        files.sort_by(|a, b| a.filename.cmp(&b.filename));
        for file in files {
            println!(
                "    {:<60} {:>9}  {}",
                if full_paths {
                    &file.filename
                } else {
                    file.basename()
                },
                human_size(file.size),
                quality(file)
            );
        }
    }
}

const LOSSY: &[&str] = &["mp3", "m4a", "aac", "ogg", "opus", "wma"];

/// Above this an `.m4a` is ALAC rather than AAC.
const LOSSLESS_KBPS: u32 = 600;

fn extension(f: &SearchFile) -> String {
    f.basename()
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default()
}

/// Bitrate in kbps and whether it is estimated: the reported one, or else
/// size × 8 / duration (many clients leave it out for M4A).
pub(crate) fn kbps(f: &SearchFile) -> Option<(u32, bool)> {
    if let Some(bitrate) = f.bitrate().filter(|&b| b > 0) {
        return Some((bitrate, false));
    }
    let secs = u64::from(f.duration().filter(|&d| d > 0)?);
    // Tiny or bogus sizes give 0, which says nothing.
    let estimate = (f.size * 8 / 1000 / secs) as u32;
    (estimate > 0).then_some((estimate, true))
}

/// Lossy formats are described by bitrate, lossless ones by sample rate
/// and bit depth.
pub(crate) fn is_lossy(f: &SearchFile) -> bool {
    let ext = extension(f);
    if !LOSSY.contains(&ext.as_str()) {
        return false;
    }
    ext != "m4a" || kbps(f).is_none_or(|(k, _)| k < LOSSLESS_KBPS)
}

pub(crate) fn format_kbps((kbps, estimated): (u32, bool)) -> String {
    if estimated {
        format!("~{kbps}kbps")
    } else {
        format!("{kbps}kbps")
    }
}

pub(crate) fn quality(f: &SearchFile) -> String {
    let mut parts = Vec::new();
    let sample = match (f.sample_rate(), f.bit_depth()) {
        (Some(rate), Some(depth)) => Some(format!("{:.1}kHz/{depth}bit", rate as f64 / 1000.0)),
        (Some(rate), None) => Some(format!("{:.1}kHz", rate as f64 / 1000.0)),
        _ => None,
    };
    if !is_lossy(f) {
        parts.extend(sample);
    }
    parts.extend(kbps(f).map(format_kbps));
    if let Some(secs) = f.duration() {
        parts.push(format!("{}:{:02}", secs / 60, secs % 60));
    }
    parts.join("  ")
}

pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(31_000_000), "31.0 MB");
        assert_eq!(human_size(1_500_000_000), "1.5 GB");
    }

    #[test]
    fn quality_strings() {
        let flac = SearchFile {
            attributes: vec![(1, 245), (4, 44100), (5, 16)],
            ..Default::default()
        };
        assert_eq!(quality(&flac), "44.1kHz/16bit  4:05");
        let mp3 = SearchFile {
            attributes: vec![(0, 320), (1, 200), (2, 0)],
            ..Default::default()
        };
        assert_eq!(quality(&mp3), "320kbps  3:20");
        // AAC without a reported bitrate: estimated from size and length.
        let aac = SearchFile {
            filename: "a\\x.m4a".into(),
            size: 6_400_000,
            attributes: vec![(1, 200), (4, 44100), (5, 16)],
            ..Default::default()
        };
        assert_eq!(quality(&aac), "~256kbps  3:20");
        // ALAC in an .m4a stays lossless.
        let alac = SearchFile {
            size: 40_000_000,
            ..aac.clone()
        };
        assert_eq!(quality(&alac), "44.1kHz/16bit  ~1600kbps  3:20");
    }
}
