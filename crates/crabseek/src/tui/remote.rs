//! Background mode: the whole UI runs in a background process, and the
//! `crabseek` command only attaches a terminal to it, like tmux.
//!
//! The process listens on a Unix socket (`$XDG_RUNTIME_DIR/crabseek.sock`).
//! An attached terminal sends its size and its key and resize events; the
//! process renders with a crossterm backend whose output goes back over the
//! socket and is written to the terminal as is. One terminal at a time: a
//! new one takes over. With none attached, everything keeps running
//! (transfers, sharing, wishlist, notifications) and output is dropped.
//!
//! Frames: one tag byte, a `u32` length (little-endian), then the payload.

use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use crossterm::event::{Event as TermEvent, EventStream};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use super::{Host, Input, InputStream};
use crate::config::Config;

const TAG_OUTPUT: u8 = 0;
const TAG_BYE: u8 = 1;
const TAG_CONTROL: u8 = 2;

/// What an attached terminal tells the background process.
#[derive(Debug, Serialize, Deserialize)]
enum Control {
    Hello { cols: u16, rows: u16 },
    Event(TermEvent),
    Stop,
}

/// What the background process tells an attached terminal.
#[derive(Debug, PartialEq, Eq)]
enum Frame {
    Output(Vec<u8>),
    Bye(String),
}

async fn write_frame(w: &mut (impl AsyncWrite + Unpin), tag: u8, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(tag);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf).await
}

async fn read_frame(r: &mut (impl AsyncRead + Unpin)) -> io::Result<(u8, Vec<u8>)> {
    let tag = r.read_u8().await?;
    let len = r.read_u32_le().await? as usize;
    if len > 64 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut payload = vec![0; len];
    r.read_exact(&mut payload).await?;
    Ok((tag, payload))
}

async fn send_control(w: &mut (impl AsyncWrite + Unpin), msg: &Control) -> io::Result<()> {
    write_frame(w, TAG_CONTROL, &serde_json::to_vec(msg)?).await
}

/// The socket of a running background process, if there is one.
pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("crabseek.sock"),
        None => std::env::temp_dir().join(format!("crabseek-{}.sock", unsafe { libc::getuid() })),
    }
}

/// Whether a background process is running (its socket accepts).
pub fn is_running() -> bool {
    std::os::unix::net::UnixStream::connect(socket_path()).is_ok()
}

// ---------------------------------------------------------------- daemon --

/// The attached terminal, shared by the renderer and the socket tasks.
struct Attached {
    out: Option<mpsc::UnboundedSender<Frame>>,
    size: Size,
    /// Which connection `out` belongs to.
    id: u64,
}

/// Collects rendered bytes and sends them on flush; dropped when no
/// terminal is attached.
struct SharedWriter {
    attached: Arc<Mutex<Attached>>,
    buf: Vec<u8>,
}

impl Write for SharedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let bytes = std::mem::take(&mut self.buf);
        if let Some(out) = &self.attached.lock().unwrap().out
            && !bytes.is_empty()
        {
            let _ = out.send(Frame::Output(bytes));
        }
        Ok(())
    }
}

/// A crossterm backend drawing into the socket, sized like the attached
/// terminal.
struct RemoteBackend {
    inner: CrosstermBackend<SharedWriter>,
    attached: Arc<Mutex<Attached>>,
}

impl Backend for RemoteBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        // Asking a terminal that may not be there would hang.
        Ok(Position::ORIGIN)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.attached.lock().unwrap().size)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size()?,
            pixels: Size::default(),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

struct RemoteHost {
    attached: Arc<Mutex<Attached>>,
}

impl RemoteHost {
    fn bye(&self, reason: &str) {
        if let Some(out) = self.attached.lock().unwrap().out.take() {
            let _ = out.send(Frame::Bye(reason.to_owned()));
        }
    }
}

impl Host for RemoteHost {
    fn detach(&mut self) {
        self.bye("detached");
    }
}

/// Runs the UI in the background until `crabseek stop`, `Q` or SIGTERM.
pub async fn run_daemon(cfg: Config) -> anyhow::Result<()> {
    let path = socket_path();
    if is_running() {
        bail!("crabseek is already running in the background");
    }
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

    let attached = Arc::new(Mutex::new(Attached {
        out: None,
        size: Size::new(80, 24),
        id: 0,
    }));
    let (input_tx, input_rx) = mpsc::unbounded_channel::<Input>();

    tokio::spawn(accept_loop(listener, attached.clone(), input_tx.clone()));
    let sigterm_tx = input_tx.clone();
    tokio::spawn(async move {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            term.recv().await;
            let _ = sigterm_tx.send(Input::Stop);
        }
    });

    let backend = RemoteBackend {
        inner: CrosstermBackend::new(SharedWriter {
            attached: attached.clone(),
            buf: Vec::new(),
        }),
        attached: attached.clone(),
    };
    let mut terminal = Terminal::new(backend)?;
    let input: InputStream = Box::pin(futures::stream::unfold(input_rx, |mut rx| async move {
        rx.recv().await.map(|i| (Ok(i), rx))
    }));
    let mut host = RemoteHost {
        attached: attached.clone(),
    };
    tracing::info!("running in the background");
    let result = super::login_then_run(&mut terminal, input, &mut host, true, cfg).await;
    host.bye("crabseek stopped");
    // Let the goodbye reach the terminal before the process ends.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = std::fs::remove_file(&path);
    result
}

async fn accept_loop(
    listener: UnixListener,
    attached: Arc<Mutex<Attached>>,
    input: mpsc::UnboundedSender<Input>,
) {
    let mut next_id = 0;
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        next_id += 1;
        tokio::spawn(serve_terminal(
            stream,
            next_id,
            attached.clone(),
            input.clone(),
        ));
    }
}

async fn serve_terminal(
    stream: UnixStream,
    id: u64,
    attached: Arc<Mutex<Attached>>,
    input: mpsc::UnboundedSender<Input>,
) {
    let (mut read, mut write) = stream.into_split();
    let first = match read_frame(&mut read).await {
        Ok((TAG_CONTROL, payload)) => serde_json::from_slice::<Control>(&payload).ok(),
        _ => None,
    };
    let (cols, rows) = match first {
        Some(Control::Hello { cols, rows }) => (cols, rows),
        Some(Control::Stop) => {
            let _ = input.send(Input::Stop);
            return;
        }
        _ => return,
    };

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Frame>();
    {
        let mut a = attached.lock().unwrap();
        if let Some(old) = a.out.take() {
            let _ = old.send(Frame::Bye("attached from another terminal".to_owned()));
        }
        a.out = Some(out_tx);
        a.size = Size::new(cols, rows);
        a.id = id;
    }
    let _ = input.send(Input::Redraw);

    tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            let result = match &frame {
                Frame::Output(bytes) => write_frame(&mut write, TAG_OUTPUT, bytes).await,
                Frame::Bye(reason) => write_frame(&mut write, TAG_BYE, reason.as_bytes()).await,
            };
            if result.is_err() || matches!(frame, Frame::Bye(_)) {
                break;
            }
        }
    });

    while let Ok((tag, payload)) = read_frame(&mut read).await {
        if tag != TAG_CONTROL {
            continue;
        }
        match serde_json::from_slice::<Control>(&payload) {
            Ok(Control::Event(event)) => {
                if let TermEvent::Resize(cols, rows) = event {
                    let mut a = attached.lock().unwrap();
                    if a.id == id {
                        a.size = Size::new(cols, rows);
                    }
                }
                let _ = input.send(Input::Term(event));
            }
            Ok(Control::Stop) => {
                let _ = input.send(Input::Stop);
            }
            Ok(Control::Hello { .. }) | Err(_) => {}
        }
    }
    // The terminal went away (closed window, killed process).
    let mut a = attached.lock().unwrap();
    if a.id == id {
        a.out = None;
    }
}

// ---------------------------------------------------------------- client --

/// Starts the background process, detached from this terminal and session.
pub fn spawn_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // A new session: closing this terminal must not take it down.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("starting crabseek in the background")?;
    Ok(())
}

/// Waits until a freshly spawned background process accepts connections.
pub async fn wait_until_running() -> anyhow::Result<()> {
    for _ in 0..50 {
        if is_running() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("crabseek did not start in the background (see the log in ~/.local/state/crabseek)")
}

/// Asks the background process to quit. Returns false if none runs.
pub async fn stop() -> anyhow::Result<bool> {
    let Ok(mut stream) = UnixStream::connect(socket_path()).await else {
        return Ok(false);
    };
    send_control(&mut stream, &Control::Stop).await?;
    // Wait for it to go away, so `stop && start` works.
    for _ in 0..50 {
        if !is_running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(true)
}

/// Shows the background process in this terminal until it detaches us.
pub async fn attach() -> anyhow::Result<()> {
    use crossterm::{cursor, execute, terminal};

    let stream = UnixStream::connect(socket_path())
        .await
        .context("connecting to crabseek in the background")?;
    let (mut read, mut write) = stream.into_split();

    let (cols, rows) = terminal::size()?;
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
    let restore = || {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show);
        default_hook(info);
    }));

    let result: anyhow::Result<String> = async {
        send_control(&mut write, &Control::Hello { cols, rows }).await?;
        let mut events = EventStream::new();
        let mut stdout = tokio::io::stdout();
        loop {
            tokio::select! {
                frame = read_frame(&mut read) => match frame {
                    Ok((TAG_OUTPUT, bytes)) => {
                        stdout.write_all(&bytes).await?;
                        stdout.flush().await?;
                    }
                    Ok((TAG_BYE, reason)) => return Ok(String::from_utf8_lossy(&reason).into_owned()),
                    Ok(_) => {}
                    Err(_) => return Ok("crabseek stopped".to_owned()),
                },
                event = events.next() => match event {
                    Some(Ok(event @ (TermEvent::Key(_) | TermEvent::Resize(..)))) => {
                        send_control(&mut write, &Control::Event(event)).await?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok("terminal closed".to_owned()),
                },
            }
        }
    }
    .await;
    restore();
    let reason = result?;
    match reason.as_str() {
        "detached" => println!(
            "crabseek keeps running in the background – run crabseek to come back, crabseek stop to quit"
        ),
        other => println!("{other}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, TAG_BYE, b"detached").await.unwrap();
        send_control(
            &mut a,
            &Control::Event(TermEvent::Key(crossterm::event::KeyEvent::from(
                crossterm::event::KeyCode::Char('q'),
            ))),
        )
        .await
        .unwrap();
        assert_eq!(
            read_frame(&mut b).await.unwrap(),
            (TAG_BYE, b"detached".to_vec())
        );
        let (tag, payload) = read_frame(&mut b).await.unwrap();
        assert_eq!(tag, TAG_CONTROL);
        assert!(matches!(
            serde_json::from_slice::<Control>(&payload).unwrap(),
            Control::Event(TermEvent::Key(_))
        ));
    }

    #[test]
    fn writer_drops_output_without_a_terminal() {
        let attached = Arc::new(Mutex::new(Attached {
            out: None,
            size: Size::new(80, 24),
            id: 0,
        }));
        let mut w = SharedWriter {
            attached: attached.clone(),
            buf: Vec::new(),
        };
        w.write_all(b"frame").unwrap();
        w.flush().unwrap();
        assert!(w.buf.is_empty());

        let (tx, mut rx) = mpsc::unbounded_channel();
        attached.lock().unwrap().out = Some(tx);
        w.write_all(b"frame").unwrap();
        w.flush().unwrap();
        assert_eq!(rx.try_recv().unwrap(), Frame::Output(b"frame".to_vec()));
    }
}
