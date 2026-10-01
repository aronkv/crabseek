mod config;
mod download;
mod persist;
mod search;
mod tui;

use std::time::Duration;

use clap::{Parser, Subcommand};
use crabseek_net::{Client, Event, ServerConnection};
use crabseek_proto::peer::PeerMsg;
use crabseek_proto::server::{ServerRequest, ServerResponse};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(version, about = "Soulseek client for the terminal")]
struct Cli {
    /// Without a subcommand, the TUI starts.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Log in, announce the listen port and print what the server sends.
    Login {
        /// How long to keep listening after login.
        #[arg(long, default_value_t = 5)]
        listen_secs: u64,
    },
    /// Connect to a user and print their user info (tests peer connections).
    Userinfo {
        username: String,
        #[arg(long, default_value_t = 40)]
        timeout_secs: u64,
    },
    /// Stay online and print peer activity (tests inbound connections).
    Online {
        #[arg(long, default_value_t = 300)]
        secs: u64,
    },
    /// Search the network and print results grouped by user and folder.
    Search {
        query: String,
        /// How long to collect results.
        #[arg(long, default_value_t = 10)]
        secs: u64,
        /// How many users to show.
        #[arg(long, default_value_t = 15)]
        top: usize,
        /// Print full remote paths, ready to paste into `download`.
        #[arg(long)]
        full_paths: bool,
        /// Send it as a wishlist search (server code 103) instead.
        #[arg(long)]
        wishlist: bool,
    },
    /// Download one file. FILENAME is the full remote path, as printed
    /// by `search --full-paths`.
    Download { username: String, filename: String },
    /// Scan the shared folders and show what other users see; with a
    /// query, show what a search for it would return.
    Shares {
        query: Option<String>,
        /// Scan this folder instead of the configured ones (repeatable).
        #[arg(long = "dir")]
        dirs: Vec<std::path::PathBuf>,
    },
    /// List a user's shared folders.
    Browse {
        username: String,
        #[arg(long, default_value_t = 60)]
        timeout_secs: u64,
    },
    /// Send a private message, then print replies for a while.
    Message {
        username: String,
        text: String,
        /// How long to wait for replies.
        #[arg(long, default_value_t = 30)]
        wait_secs: u64,
    },
    /// Check automatic port forwarding (UPnP): open the listen port on the
    /// router, report, and close it again.
    Portmap,
    /// Quit crabseek running in the background.
    Stop,
    /// Run in the background (started by `crabseek` in background mode or a
    /// systemd user service); `crabseek` attaches to it.
    #[command(hide = true)]
    Daemon,
    /// Forget the saved username and password.
    Logout,
    /// Print the config file location.
    ConfigPath,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    raise_open_file_limit();
    let cli = Cli::parse();
    for moved in config::migrate_from_seekr()? {
        eprintln!("moved {moved} (seekr is now called crabseek)");
    }
    let Some(command) = cli.command else {
        return run_tui().await;
    };
    if let Command::Daemon = command {
        return run_daemon().await;
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    match command {
        Command::Login { listen_secs } => login(listen_secs).await,
        Command::Userinfo {
            username,
            timeout_secs,
        } => userinfo(username, timeout_secs).await,
        Command::Online { secs } => online(secs).await,
        Command::Download { username, filename } => {
            let (client, events) = start_client().await?;
            download::run(client, events, &username, &filename).await
        }
        Command::Search {
            query,
            secs,
            top,
            full_paths,
            wishlist,
        } => {
            let (client, events) = start_client().await?;
            search::run(client, events, &query, secs, top, full_paths, wishlist).await
        }
        Command::Shares { query, dirs } => shares(query, dirs),
        Command::Browse {
            username,
            timeout_secs,
        } => browse(username, timeout_secs).await,
        Command::Message {
            username,
            text,
            wait_secs,
        } => {
            let (client, mut events) = start_client().await?;
            client.message_user(&username, &text)?;
            println!("sent to {username}; waiting {wait_secs}s for messages...");
            let deadline = tokio::time::sleep(Duration::from_secs(wait_secs));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    event = events.recv() => match event {
                        Some(Event::PrivateMessage { username, message, new, .. }) => {
                            println!("[{username}]{} {message}", if new { "" } else { " (missed)" });
                        }
                        Some(Event::ServerClosed { reason }) => anyhow::bail!("disconnected: {reason}"),
                        Some(_) => {}
                        None => break,
                    },
                }
            }
            Ok(())
        }
        Command::Portmap => {
            let port = config::load_or_default()?.listen_port;
            println!("asking the router to open TCP port {port}...");
            match crabseek_net::portmap::map(port).await {
                Ok(m) => {
                    if m.already_mapped {
                        println!("the router already has a rule for port {port} (manual forward?)");
                    } else {
                        println!("port {port} opened on the router");
                    }
                    if let Some(ip) = m.external_ip {
                        println!("router's public address: {ip}");
                    }
                    if !m.already_mapped {
                        crabseek_net::portmap::unmap(port).await;
                        println!("removed the test mapping again");
                    }
                }
                Err(e) => println!("UPnP not available: {e}"),
            }
            Ok(())
        }
        Command::Stop => {
            if tui::remote::stop().await? {
                println!("crabseek stopped");
            } else {
                println!("crabseek is not running in the background");
            }
            Ok(())
        }
        Command::Daemon => {
            unreachable!("handled before logging is set up")
        }
        Command::Logout => {
            if config::clear_credentials()? {
                println!("logged out; run `crabseek` to log in again");
            } else {
                println!("not logged in");
            }
            Ok(())
        }
        Command::ConfigPath => {
            println!("{}", config::path()?.display());
            Ok(())
        }
    }
}

/// The TUI owns the terminal, so logs go to a file instead.
/// Peers, transfers and indirect connection attempts each hold a socket;
/// the default soft limit is often 1024 (notably under systemd), while the
/// hard limit allows far more. Raise the soft limit, as Go programs do.
fn raise_open_file_limit() {
    const WANTED: libc::rlim_t = 65_536;
    unsafe {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 && limit.rlim_cur < WANTED {
            limit.rlim_cur = WANTED.min(limit.rlim_max);
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

/// `crabseek` without a subcommand: attach to the background process if
/// one runs; otherwise start one (background mode) or run right here.
async fn run_tui() -> anyhow::Result<()> {
    if tui::remote::is_running() {
        return tui::remote::attach().await;
    }
    let cfg = config::load_or_default()?;
    if cfg.background {
        tui::remote::spawn_daemon()?;
        tui::remote::wait_until_running().await?;
        return tui::remote::attach().await;
    }
    init_file_logging()?;
    tui::run(cfg).await
}

async fn run_daemon() -> anyhow::Result<()> {
    init_file_logging()?;
    tui::remote::run_daemon(config::load_or_default()?).await
}

/// The UI owns the terminal, so logs go to a file instead.
fn init_file_logging() -> anyhow::Result<()> {
    let log_path = config::log_path()?;
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let log = std::fs::File::create(&log_path)?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "crabseek=info,crabseek_net=info".into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log))
        .init();
    Ok(())
}

fn shares(query: Option<String>, dirs: Vec<std::path::PathBuf>) -> anyhow::Result<()> {
    use crabseek_net::shares::{MAX_SEARCH_RESULTS, MetadataCache, ShareIndex};

    let cfg = config::load_or_default()?;
    let dirs = if dirs.is_empty() {
        cfg.shared_dirs()?
    } else {
        dirs
    };
    let cache_path = cfg.client_config()?.share_cache;
    let old = cache_path
        .as_deref()
        .map(MetadataCache::load)
        .unwrap_or_default();
    let started = std::time::Instant::now();
    let report = ShareIndex::scan(&dirs, &old);
    if let Some(path) = &cache_path {
        report.cache.save(path)?;
    }
    for dir in &dirs {
        println!("shared: {}", config::display_path(dir));
    }
    for e in &report.errors {
        println!("warning: {e}");
    }
    let index = report.index;
    println!(
        "{} files in {} folders (scanned in {:.1}s)",
        index.file_count(),
        index.folder_count(),
        started.elapsed().as_secs_f64()
    );
    if let Some(query) = query {
        let results = index.search(&query, MAX_SEARCH_RESULTS);
        println!("\n{} results for {query:?}:", results.len());
        for f in results.iter().take(20) {
            println!(
                "  {}  {}  {}",
                f.filename,
                search::human_size(f.size),
                search::quality(f)
            );
        }
    }
    Ok(())
}

async fn browse(username: String, timeout_secs: u64) -> anyhow::Result<()> {
    let (client, mut events) = start_client().await?;
    println!("asking {username} for their share list...");
    client.browse(&username)?;
    let list = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        loop {
            match events.recv().await {
                Some(Event::BrowseResult { username: u, list }) if u == username => {
                    return Ok(list);
                }
                Some(Event::PeerConnectFailed {
                    username: u,
                    reason,
                }) if u == username => {
                    anyhow::bail!("could not reach {username}: {reason}")
                }
                Some(_) => {}
                None => anyhow::bail!("client stopped"),
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("no share list from {username} within {timeout_secs}s"))??;

    let files: usize = list.dirs.iter().map(|d| d.files.len()).sum();
    let bytes: u64 = list
        .dirs
        .iter()
        .flat_map(|d| &d.files)
        .map(|f| f.size)
        .sum();
    println!(
        "{username} shares {files} files in {} folders ({})",
        list.dirs.len(),
        search::human_size(bytes)
    );
    for dir in &list.dirs {
        println!("  {}  ({} files)", dir.path, dir.files.len());
    }
    Ok(())
}

async fn login(listen_secs: u64) -> anyhow::Result<()> {
    let cfg = config::load()?;
    println!("connecting to {} as {}", cfg.server, cfg.username);

    let (mut conn, info) =
        ServerConnection::login(&cfg.server, &cfg.username, &cfg.password).await?;
    println!("logged in, external IP {}", info.own_ip);
    println!("supporter: {}", info.is_supporter);
    println!("greeting: {}", info.greeting);

    conn.send(&ServerRequest::SetWaitPort {
        port: cfg.listen_port.into(),
    })
    .await?;

    println!("\nmessages in the next {listen_secs}s:");
    let deadline = tokio::time::sleep(Duration::from_secs(listen_secs));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            msg = conn.recv() => match msg? {
                Some(ServerResponse::Unknown { code, payload }) => {
                    println!("  code {code:>4}  {} bytes", payload.len());
                }
                Some(other) => println!("  {other:?}"),
                None => {
                    println!("server closed the connection");
                    break;
                }
            },
        }
    }
    Ok(())
}

async fn start_client() -> anyhow::Result<(Client, mpsc::UnboundedReceiver<Event>)> {
    if tui::remote::is_running() {
        // A second login would throw the background one off the server.
        anyhow::bail!("crabseek is running in the background; stop it first with `crabseek stop`");
    }
    let cfg = config::load()?;
    println!(
        "logging in as {} (listening on port {})",
        cfg.username, cfg.listen_port
    );
    let (client, info, events) = Client::start(cfg.client_config()?).await?;
    println!("logged in, external IP {}", info.own_ip);
    Ok((client, events))
}

/// Prints an event; returns false once the client has stopped.
fn print_event(event: &Event) -> bool {
    match event {
        Event::PeerConnected { username, method } => {
            println!("[{username}] connected ({method:?})")
        }
        Event::PeerConnectFailed { username, reason } => {
            println!("[{username}] connection failed: {reason}")
        }
        Event::PeerDisconnected { username, reason } => {
            println!("[{username}] disconnected: {reason}")
        }
        Event::PeerMessage { username, msg } => match msg {
            PeerMsg::UserInfoResponse(info) => {
                println!("[{username}] user info:");
                println!(
                    "  description:   {}",
                    info.description.replace('\n', "\n                 ")
                );
                println!(
                    "  picture:       {}",
                    info.picture
                        .as_ref()
                        .map_or("none".to_owned(), |p| format!("{} bytes", p.len()))
                );
                println!("  total uploads: {}", info.total_uploads);
                println!("  queue size:    {}", info.queue_size);
                println!("  free slots:    {}", info.slots_free);
            }
            PeerMsg::Unknown { code, payload } => {
                println!("[{username}] peer message {code} ({} bytes)", payload.len())
            }
            other => println!("[{username}] {other:?}"),
        },
        Event::SearchResult(resp) => println!(
            "[{}] {} search results (token {})",
            resp.username,
            resp.files.len(),
            resp.token
        ),
        Event::Download {
            username,
            filename,
            state,
            ..
        } => println!("[{username}] {filename}: {state:?}"),
        Event::Upload {
            username,
            filename,
            state,
            ..
        } => println!("[{username}] upload {filename}: {state:?}"),
        Event::SharesScanned {
            folders,
            files,
            errors,
        } => {
            println!("sharing {files} files in {folders} folders");
            for e in errors {
                println!("  warning: {e}");
            }
        }
        Event::Distrib(status) => println!("distributed network: {status:?}"),
        Event::PrivateMessage {
            username, message, ..
        } => println!("[{username}] says: {message}"),
        Event::PortMap(status) => println!("port forwarding: {status:?}"),
        Event::BrowseResult { username, list } => {
            println!("[{username}] shares {} folders", list.dirs.len())
        }
        Event::SearchAnswered {
            username,
            query,
            results,
        } => println!("[{username}] searched {query:?}: answered with {results} files"),
        Event::ServerMessage(_) | Event::ListenPort { .. } | Event::SharesScanning => {}
        Event::ServerClosed { reason } => {
            println!("server connection closed: {reason}");
            return false;
        }
    }
    true
}

async fn userinfo(username: String, timeout_secs: u64) -> anyhow::Result<()> {
    let (client, mut events) = start_client().await?;
    println!("connecting to {username}...");
    client.send_peer(&username, PeerMsg::UserInfoRequest)?;

    let deadline = tokio::time::sleep(Duration::from_secs(timeout_secs));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => anyhow::bail!("no answer from {username} within {timeout_secs}s"),
            event = events.recv() => {
                let Some(event) = event else { return Ok(()) };
                if !print_event(&event) {
                    return Ok(());
                }
                match event {
                    Event::PeerMessage { username: from, msg: PeerMsg::UserInfoResponse(_) }
                        if from == username => return Ok(()),
                    Event::PeerConnectFailed { username: from, .. } if from == username => {
                        anyhow::bail!("could not reach {username}")
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn online(secs: u64) -> anyhow::Result<()> {
    let (_client, mut events) = start_client().await?;
    println!("online for {secs}s, waiting for peers...");
    let deadline = tokio::time::sleep(Duration::from_secs(secs));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return Ok(()),
            event = events.recv() => match event {
                Some(event) if print_event(&event) => {}
                _ => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn open_file_limit_is_raised() {
        let get = || unsafe {
            let mut l = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::getrlimit(libc::RLIMIT_NOFILE, &mut l);
            l
        };
        let original = get();
        if original.rlim_max <= 1024 {
            return; // nothing to raise to on this machine
        }
        unsafe {
            libc::setrlimit(
                libc::RLIMIT_NOFILE,
                &libc::rlimit {
                    rlim_cur: 1024,
                    rlim_max: original.rlim_max,
                },
            );
        }
        super::raise_open_file_limit();
        let raised = get().rlim_cur;
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) };
        assert!(raised > 1024, "soft limit stayed at {raised}");
    }
}
