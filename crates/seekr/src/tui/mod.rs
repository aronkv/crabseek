//! The terminal UI: log in, search, pick files or folders, watch downloads.

mod app;
mod buddies;
mod chat;
mod help;
mod login;
mod notify;
mod results;
mod settings;
mod transfers;
mod ui;
mod uploads;
mod wishlist;

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
                    let downloads_path = config::downloads_path()?;
                    let saved = crate::persist::load(&downloads_path);
                    let buddies_path = config::buddies_path()?;
                    let buddies = crate::persist::load_buddies(&buddies_path);
                    let app = App::new(
                        client,
                        cfg.username.clone(),
                        &cfg,
                        saved,
                        Some(downloads_path),
                    )?
                    .with_buddies(buddies, Some(buddies_path))
                    .with_chats(
                        chat::load(&config::chats_path()?),
                        Some(config::chats_path()?),
                    )
                    .with_wishlist(
                        crate::persist::load_wishlist(&config::wishlist_path()?),
                        Some(config::wishlist_path()?),
                    );
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
            _ = tick.tick() => {
                app.tick();
                app.persist();
            }
        }
    }
    app.persist();
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
    use super::chat;
    use super::*;
    use crate::config::Config;

    fn app_with_results() -> App {
        let mut app = App::new(
            Client::offline(),
            "me".into(),
            &Config::default(),
            vec![],
            None,
        )
        .unwrap();
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
        assert!(screen.contains("2 Downloads (1)"));
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
    fn restores_saved_downloads() {
        use crate::persist::{SavedDownload, SavedStatus};
        let saved = vec![
            SavedDownload {
                username: "a".into(),
                filename: "x\\1.flac".into(),
                status: SavedStatus::Completed {
                    path: "/m/1.flac".into(),
                },
            },
            SavedDownload {
                username: "a".into(),
                filename: "x\\2.flac".into(),
                status: SavedStatus::Pending,
            },
        ];
        let app = App::new(
            Client::offline(),
            "me".into(),
            &Config::default(),
            saved,
            None,
        )
        .unwrap();
        assert_eq!(app.transfers.list.len(), 2);
        assert!(matches!(
            app.transfers.list[0].state,
            DownloadState::Completed { .. }
        ));
        // The offline client cannot queue, so the pending one is marked failed
        // instead of disappearing.
        assert!(matches!(
            app.transfers.list[1].state,
            DownloadState::Failed { .. }
        ));
        assert!(!app.transfers.dirty);
    }

    #[test]
    fn vim_counts_and_alt_tabs() {
        use crossterm::event::KeyModifiers;
        let mut app = app_with_results();
        app.on_key(KeyEvent::from(KeyCode::Enter)); // expand: 4 rows
        app.on_key(KeyEvent::from(KeyCode::Char('3')));
        assert_eq!(app.count, Some(3));
        assert!(draw(&mut app).contains(" 3 "));
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.count, None);
        assert_eq!(app.results.selected_index(), Some(3));
        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        app.on_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.results.selected_index(), Some(1));
        app.on_key(KeyEvent::from(KeyCode::Char('1')));
        app.on_key(KeyEvent::from(KeyCode::Char('G')));
        assert_eq!(app.results.selected_index(), Some(0));
        assert_eq!(app.tab, Tab::Search);

        app.on_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT));
        assert_eq!(app.tab, Tab::Uploads);
        app.on_key(KeyEvent::from(KeyCode::F(4)));
        assert_eq!(app.tab, Tab::Settings);
    }

    #[test]
    fn browse_from_search_result() {
        use seekr_proto::shares::{SharedDirectory, SharedFileList};
        let mut app = app_with_results();
        app.on_key(KeyEvent::from(KeyCode::Char('b')));
        assert_eq!(app.tab, Tab::Browse);
        assert_eq!(app.browse.as_ref().unwrap().username, "alice");
        assert!(draw(&mut app).contains("asking alice for their share list"));

        let dir = |path: &str, name: &str| SharedDirectory {
            path: path.into(),
            files: vec![SearchFile {
                filename: name.into(),
                size: 1_000_000,
                extension: "flac".into(),
                attributes: vec![(1, 60), (4, 44100), (5, 16)],
            }],
        };
        app.on_event(Event::BrowseResult {
            username: "alice".into(),
            list: SharedFileList {
                dirs: vec![
                    dir("Music\\Aphex Twin\\SAW 85-92", "01 - Xtal.flac"),
                    dir(
                        "Music\\Boards of Canada\\Geogaddi",
                        "01 - Ready Lets Go.flac",
                    ),
                ],
                private_dirs: vec![],
            },
        });
        let screen = draw(&mut app);
        println!("{screen}");
        assert!(screen.contains("alice: 2 folders, 2 files"));
        assert!(screen.contains("Aphex Twin\\SAW 85-92"));
        // Downloading from the browse tree uses the full remote path.
        app.on_key(KeyEvent::from(KeyCode::Enter));
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(
            app.browse_results.selection_files(),
            [(
                "alice".to_owned(),
                "Music\\Aphex Twin\\SAW 85-92\\01 - Xtal.flac".to_owned()
            )]
        );
    }

    #[test]
    fn buddies_tab() {
        use crossterm::event::KeyModifiers;
        use seekr_proto::server::{OnlineStatus, ServerResponse, UserStats, WatchedUser};

        let mut app = app_with_results();
        // `A` on a search result adds its user.
        app.on_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert_eq!(app.status, "added alice to buddies");
        app.on_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert_eq!(app.status, "alice is already a buddy");

        // `a` on the Buddies tab asks for a name; digits are typed, not counts.
        app.on_key(KeyEvent::from(KeyCode::F(6)));
        assert_eq!(app.tab, Tab::Buddies);
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        for c in "bob2".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert!(draw(&mut app).contains("Add buddy"));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.status, "added bob2 to buddies");
        assert_eq!(app.tab, Tab::Buddies);

        app.on_event(Event::ServerMessage(ServerResponse::WatchUser {
            username: "alice".into(),
            user: Some(WatchedUser {
                status: OnlineStatus::Online,
                stats: UserStats {
                    avg_speed: 1_500_000,
                    upload_num: 0,
                    files: 12_345,
                    dirs: 830,
                },
                country: Some("HU".into()),
            }),
        }));
        app.on_event(Event::ServerMessage(ServerResponse::WatchUser {
            username: "bob2".into(),
            user: None,
        }));
        let screen = draw(&mut app);
        println!("{screen}");
        assert!(screen.contains("Buddies (1/2 online)"));
        assert!(screen.contains("6 Buddies (1)"));
        assert!(screen.contains("●  online        alice"));
        assert!(screen.contains("12345 files · 830 dirs"));
        assert!(screen.contains("HU"));
        assert!(screen.contains("✗  no such user  bob2"));

        app.on_event(Event::ServerMessage(ServerResponse::UserStatus {
            username: "alice".into(),
            status: OnlineStatus::Away,
            privileged: false,
        }));
        assert!(draw(&mut app).contains("◐  away"));

        // The cursor is on alice (first row); `x` removes her.
        app.on_key(KeyEvent::from(KeyCode::Char('x')));
        assert_eq!(app.status, "removed alice from buddies");
        assert_eq!(app.buddies.names(), ["bob2"]);
    }

    #[test]
    fn search_box_keys() {
        use crossterm::event::KeyModifiers;
        let alt_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT);
        let mut app = App::new(
            Client::offline(),
            "me".into(),
            &Config::default(),
            vec![],
            None,
        )
        .unwrap();
        // Starts in the list, so keys are commands, not text.
        assert_eq!((app.tab, app.focus), (Tab::Search, Focus::List));
        assert!(draw(&mut app).contains("press s or / to search"));
        app.on_key(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(app.focus, Focus::Input);
        // Inside the box `s` is just a letter; Alt-s goes back.
        app.on_key(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(app.input, "s");
        app.on_key(alt_s);
        assert_eq!(app.focus, Focus::List);
        assert_eq!(app.input, "s");

        // Alt-s and `/` open the search from other tabs, even from a
        // text box there.
        app.on_key(KeyEvent::from(KeyCode::F(6)));
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        app.on_key(alt_s);
        assert_eq!((app.tab, app.focus), (Tab::Search, Focus::Input));
        app.on_key(KeyEvent::from(KeyCode::Esc));
        app.on_key(KeyEvent::from(KeyCode::F(2)));
        app.on_key(KeyEvent::from(KeyCode::Char('/')));
        assert_eq!((app.tab, app.focus), (Tab::Search, Focus::Input));

        app.on_key(KeyEvent::from(KeyCode::Esc));
        app.on_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[test]
    fn format_filter_survives_new_browse_and_search() {
        use super::results::FormatFilter;
        use seekr_proto::shares::{SharedDirectory, SharedFileList};

        let mut app = app_with_results();
        app.on_key(KeyEvent::from(KeyCode::Char('f')));
        assert_eq!(app.results.filter(), FormatFilter::Flac);
        // A new search keeps the format.
        app.on_key(KeyEvent::from(KeyCode::Char('s')));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.results.filter(), FormatFilter::Flac);

        let share = |files: &[&str]| SharedFileList {
            dirs: vec![SharedDirectory {
                path: "Music\\Album".into(),
                files: files
                    .iter()
                    .map(|name| SearchFile {
                        filename: (*name).into(),
                        size: 1_000_000,
                        extension: String::new(),
                        attributes: vec![],
                    })
                    .collect(),
            }],
            private_dirs: vec![],
        };
        app.tab = Tab::Browse;
        // Browse starts on its list; `/` opens the username box.
        app.on_key(KeyEvent::from(KeyCode::Char('/')));
        app.browse_input = "alice".into();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        app.on_event(Event::BrowseResult {
            username: "alice".into(),
            list: share(&["1.flac", "2.mp3", "cover.jpg"]),
        });
        app.on_key(KeyEvent::from(KeyCode::Char('f')));
        let screen = draw(&mut app);
        println!("{screen}");
        // Totals, then what the filter lets through: the FLAC and the cover.
        assert!(screen.contains("alice: 1 folders, 3 files · [FLAC] 1 folders, 2 files"));

        // Browsing someone else keeps FLAC on.
        app.on_key(KeyEvent::from(KeyCode::Char('/')));
        app.browse_input = "bob".into();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(draw(&mut app).contains("bob · [FLAC]"));
        app.on_event(Event::BrowseResult {
            username: "bob".into(),
            list: share(&["1.mp3"]),
        });
        assert_eq!(app.browse_results.filter(), FormatFilter::Flac);
        assert!(draw(&mut app).contains("nothing in this format"));
    }

    #[test]
    fn wishlist_from_search_and_tab() {
        use crossterm::event::KeyModifiers;
        let mut app = app_with_results();
        // `w` on the results keeps the search on the wishlist.
        app.on_key(KeyEvent::from(KeyCode::Char('w')));
        assert_eq!(app.status, "added \"boards of canada\" to the wishlist");
        app.on_key(KeyEvent::from(KeyCode::Char('w')));
        assert!(app.status.contains("already on the wishlist"));

        app.on_key(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::ALT));
        assert_eq!(app.tab, Tab::Wishlist);
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        for c in "aphex twin".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.wishlist.queries(), ["boards of canada", "aphex twin"]);

        let screen = draw(&mut app);
        println!("{screen}");
        assert!(screen.contains("7 Wishlist"));
        assert!(screen.contains("aphex twin"));
        assert!(screen.contains("not run yet"));

        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        app.on_key(KeyEvent::from(KeyCode::Char('x')));
        assert_eq!(app.wishlist.queries(), ["boards of canada"]);
    }

    #[test]
    fn help_window_and_list_focus_on_start() {
        use crossterm::event::KeyModifiers;
        let mut app = App::new(
            Client::offline(),
            "me".into(),
            &Config::default(),
            vec![],
            None,
        )
        .unwrap();
        // Both Search and Browse start on their lists, not in a text box.
        assert_eq!(app.focus, Focus::List);
        app.on_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
        assert_eq!(app.tab, Tab::Browse);
        assert_eq!(app.browse_focus, Focus::List);
        // So `?` opens the help instead of being typed.
        app.on_key(KeyEvent::from(KeyCode::Char('?')));
        assert!(app.help);
        let screen = draw(&mut app);
        assert!(screen.contains("Help – Browse"));
        // Keys are swallowed while it is open; Esc closes it.
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        assert!(app.help);
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(!app.help);

        app.on_key(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::ALT));
        app.on_key(KeyEvent::from(KeyCode::Char('?')));
        let screen = draw(&mut app);
        println!("{screen}");
        assert!(screen.contains("Help – Wishlist"));
        assert!(screen.contains("12 minutes"));
        // `q` closes the help rather than quitting.
        app.on_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!app.help && !app.quit);
    }

    #[test]
    fn chat_receives_opens_and_renders() {
        use crossterm::event::KeyModifiers;
        let mut app = app_with_results();
        app.on_event(Event::PrivateMessage {
            timestamp: chat::now(),
            username: "carol".into(),
            message: "szia! megvan még a Geogaddi FLAC-ben?".into(),
            new: true,
        });
        assert_eq!(app.chats.total_unread(), 1);
        assert!(app.status.starts_with("message from carol"));
        assert!(draw(&mut app).contains("8 Chat (1)"));

        // `m` on a search result writes to that row's user.
        app.on_key(KeyEvent::from(KeyCode::Char('m')));
        assert_eq!(app.tab, Tab::Chat);
        assert_eq!(app.chats.selected().unwrap().username, "alice");
        assert!(app.chats.input.is_some());
        for c in "hello".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.on_key(KeyEvent::from(KeyCode::Esc));

        // Reading carol's conversation clears its unread count.
        app.on_key(KeyEvent::new(KeyCode::Char('8'), KeyModifiers::ALT));
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.chats.selected().unwrap().username, "carol");
        assert_eq!(app.chats.total_unread(), 0);
        let screen = draw(&mut app);
        println!("{screen}");
        assert!(screen.contains("carol: szia! megvan"));
        assert!(screen.contains("Conversations"));

        // Sending with an offline client reports the error and keeps the
        // conversation unchanged.
        app.on_key(KeyEvent::from(KeyCode::Char('i')));
        for c in "igen".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.status, "client has shut down");
        assert_eq!(app.chats.selected().unwrap().messages.len(), 1);
    }

    #[test]
    fn download_key_on_offline_client_reports_error() {
        let mut app = app_with_results();
        app.on_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.status, "client has shut down");
    }
}
