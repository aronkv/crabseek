//! The terminal UI: log in, search, pick files or folders, watch downloads.

mod app;
mod login;
mod results;
mod settings;
mod transfers;
mod ui;

use std::time::Duration;

use crossterm::event::{
    Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures::StreamExt;
use seekr_net::{Client, Event};
use tokio::sync::mpsc;

use crate::config::{self, Config};
use app::App;
use login::{LoginAction, LoginForm};

/// Redraw at least this often so timers and speeds stay current.
const TICK: Duration = Duration::from_millis(500);

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = login_then_run(&mut terminal, cfg).await;
    ratatui::restore();
    result
}

/// Next key press, or `None` when the terminal input ends.
async fn next_key(input: &mut EventStream) -> anyhow::Result<Option<KeyEvent>> {
    loop {
        match input.next().await {
            Some(Ok(TermEvent::Key(key))) if key.kind == KeyEventKind::Press => {
                return Ok(Some(key));
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.into()),
            None => return Ok(None),
        }
    }
}

/// Logs in with the saved credentials (showing only a small "connecting"
/// box), or asks for them when there are none or the server rejects them.
/// Credentials typed into the form are saved only after the server
/// accepted them; the config file and its folder are created then.
async fn login_then_run(
    terminal: &mut ratatui::DefaultTerminal,
    mut cfg: Config,
) -> anyhow::Result<()> {
    enum Screen {
        Form,
        /// Saved credentials: connecting, or a non-credential error.
        Splash(Option<String>),
    }

    let mut input = EventStream::new();
    let config_path = config::display_path(&config::path()?);
    let mut form = LoginForm::new(cfg.username.clone());
    let mut from_form = false;
    let mut attempt = cfg.has_credentials();
    let mut screen = if attempt {
        Screen::Splash(None)
    } else {
        Screen::Form
    };

    loop {
        if attempt {
            attempt = false;
            form.busy = from_form;
            let mut start = Box::pin(Client::start(cfg.client_config()?));
            let result = loop {
                terminal.draw(|f| match screen {
                    Screen::Form => login::render(f, &form, &config_path),
                    Screen::Splash(_) => login::render_splash(f, &cfg.username, None),
                })?;
                tokio::select! {
                    result = &mut start => break result,
                    key = next_key(&mut input) => match key? {
                        Some(key) if is_quit(key) => return Ok(()),
                        Some(_) => {}
                        None => return Ok(()),
                    },
                }
            };
            match result {
                Ok((client, _info, events)) => {
                    if from_form {
                        config::save_credentials(&cfg.username, &cfg.password)?;
                    }
                    let app = App::new(client, cfg.username.clone(), &cfg)?;
                    return event_loop(terminal, &mut input, app, events).await;
                }
                Err(e) => {
                    tracing::warn!(%e, "login failed");
                    if from_form || login::is_credential_error(&e) {
                        form.failed(login::explain(&e));
                        screen = Screen::Form;
                    } else {
                        screen = Screen::Splash(Some(login::explain(&e)));
                    }
                }
            }
        }

        terminal.draw(|f| match &screen {
            Screen::Form => login::render(f, &form, &config_path),
            Screen::Splash(error) => login::render_splash(f, &cfg.username, error.as_deref()),
        })?;
        let Some(key) = next_key(&mut input).await? else {
            return Ok(());
        };
        match screen {
            Screen::Form => match form.on_key(key) {
                LoginAction::Submit { username, password } => {
                    cfg.username = username;
                    cfg.password = password;
                    from_form = true;
                    attempt = true;
                }
                LoginAction::Quit => return Ok(()),
                LoginAction::None => {}
            },
            Screen::Splash(_) if is_quit(key) || key.code == KeyCode::Char('q') => return Ok(()),
            Screen::Splash(_) => {
                if matches!(key.code, KeyCode::Char('r') | KeyCode::Enter) {
                    screen = Screen::Splash(None);
                    attempt = true;
                }
            }
        }
    }
}

fn is_quit(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    input: &mut EventStream,
    mut app: App,
    mut events: mpsc::UnboundedReceiver<Event>,
) -> anyhow::Result<()> {
    let mut tick = tokio::time::interval(TICK);
    let mut client_alive = true;

    while !app.quit {
        terminal.draw(|frame| ui::render(frame, &mut app))?;
        tokio::select! {
            key = next_key(input) => match key? {
                Some(key) => app.on_key(key),
                None => break,
            },
            event = events.recv(), if client_alive => match event {
                Some(event) => {
                    app.on_event(event);
                    // Search results arrive in bursts; draw once per burst.
                    while let Ok(event) = events.try_recv() {
                        app.on_event(event);
                    }
                }
                None => client_alive = false,
            },
            _ = tick.tick() => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::{KeyCode, KeyEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use seekr_net::DownloadState;
    use seekr_proto::search::{SearchFile, SearchResponse};

    use super::app::{ActiveSearch, Focus, Tab};
    use super::*;
    use crate::config::Config;

    fn app_with_results() -> App {
        let mut app = App::new(Client::offline(), "me".into(), &Config::default()).unwrap();
        app.search = Some(ActiveSearch {
            token: 1,
            query: "boards of canada".into(),
            started: std::time::Instant::now(),
        });
        app.input = "boards of canada".into();
        app.focus = Focus::List;
        let files = (1..=3)
            .map(|i| SearchFile {
                filename: format!("@@x\\Music\\Boards of Canada\\Geogaddi\\0{i} - Track.flac"),
                size: 30_000_000,
                extension: "flac".into(),
                attributes: vec![(1, 200), (4, 44100), (5, 16)],
            })
            .collect();
        app.on_event(Event::SearchResult(SearchResponse {
            username: "alice".into(),
            token: 1,
            files,
            slot_free: true,
            avg_speed: 5_000_000,
            queue_length: 0,
            private_files: vec![],
        }));
        app
    }

    fn draw(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 16)).unwrap();
        terminal.draw(|f| ui::render(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_search_results() {
        let mut app = app_with_results();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let screen = draw(&mut app);
        println!("{screen}");
        assert!(screen.contains("▾ Boards of Canada\\Geogaddi  (3)"));
        assert!(screen.contains("02 - Track.flac"));
        assert!(screen.contains("FLAC 16/44.1"));
        assert!(screen.contains("alice"));
    }

    #[test]
    fn renders_transfers() {
        let mut app = app_with_results();
        app.tab = Tab::Transfers;
        app.on_event(Event::Download {
            id: 1,
            username: "alice".into(),
            filename: "a\\01 - Track.flac".into(),
            state: DownloadState::Transferring {
                received: 15_000_000,
                size: 30_000_000,
            },
        });
        app.on_event(Event::Download {
            id: 2,
            username: "bob".into(),
            filename: "a\\02 - Other.flac".into(),
            state: DownloadState::Completed {
                path: PathBuf::from("/tmp/02 - Other.flac"),
            },
        });
        let screen = draw(&mut app);
        println!("{screen}");
        assert!(screen.contains("downloading"));
        assert!(screen.contains("50%"));
        assert!(screen.contains("done"));
        assert!(screen.contains("2 Transfers (1)"));
    }

    #[test]
    fn renders_login() {
        let mut form = LoginForm::new("alice".into());
        form.on_key(KeyEvent::from(KeyCode::Char('x')));
        form.failed("Wrong password for this username.".into());
        let mut terminal = Terminal::new(TestBackend::new(90, 22)).unwrap();
        terminal
            .draw(|f| login::render(f, &form, "~/.config/seekr/config.toml"))
            .unwrap();
        let buf = terminal.backend().buffer();
        let screen: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        println!("{screen}");
        assert!(screen.contains(" alice"));
        assert!(screen.contains("Wrong password"));
    }

    #[test]
    fn renders_settings_while_editing() {
        let mut app = app_with_results();
        app.tab = Tab::Settings;
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        let screen = draw(&mut app);
        println!("{screen}");
        assert!(screen.contains("Download folder"));
        assert!(screen.contains("~/"));
        assert!(screen.contains("Tab complete folder"));
        // Keys go to the path editor, not to tab switching.
        app.on_key(KeyEvent::from(KeyCode::Char('1')));
        assert_eq!(app.tab, Tab::Settings);
    }

    #[test]
    fn download_key_on_offline_client_reports_error() {
        let mut app = app_with_results();
        app.on_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.status, "client has shut down");
    }
}
