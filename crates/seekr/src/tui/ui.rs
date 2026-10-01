//! Rendering. Only the visible rows of each list are built, so large
//! result sets stay cheap to draw.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row as TableRow, Table, TableState, Tabs};
use seekr_net::{DistribStatus, DownloadState, PortMapStatus, UploadState};
use seekr_proto::server::OnlineStatus;

use super::app::{App, Focus, SharesStatus, Tab};
use super::buddies::Known;
use super::results::{FormatFilter, Results, Row};
use super::settings::Item;
use crate::config::{self, display_path};
use crate::search::{human_size, quality};

const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

pub fn render(frame: &mut Frame, app: &mut App) {
    let [header, body, status, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(frame, app, header);
    match app.tab {
        Tab::Search => render_search(frame, app, body),
        Tab::Transfers => render_transfers(frame, app, body),
        Tab::Uploads => render_uploads(frame, app, body),
        Tab::Browse => render_browse(frame, app, body),
        Tab::Settings => render_settings(frame, app, body),
        Tab::Buddies => render_buddies(frame, app, body),
    }
    frame.render_widget(
        Paragraph::new(app.status.as_str()).fg(Color::Yellow),
        status,
    );
    let mut help_spans = Vec::new();
    if let Some(count) = app.count {
        help_spans.push(Span::raw(format!(" {count} ")).black().on_yellow());
    }
    help_spans.push(Span::raw(help_line(app)).dark_gray());
    frame.render_widget(Paragraph::new(Line::from(help_spans)), help);
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let counted = |label: &str, n: usize| {
        Line::from(if n > 0 {
            format!(" {label} ({n}) ")
        } else {
            format!(" {label} ")
        })
    };
    let titles = vec![
        Line::from(" 1 Search "),
        counted("2 Downloads", app.transfers.active()),
        counted("3 Uploads", app.uploads.active()),
        Line::from(" 4 Settings "),
        Line::from(" 5 Browse "),
        counted("6 Buddies", app.buddies.online()),
    ];
    let selected = app.tab.index();
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(Style::new().bold().reversed())
            .divider(""),
        area,
    );

    let down = app.transfers.total_speed();
    let up = app.uploads.total_speed();
    let mut right = vec![];
    if down > 0.0 {
        right.push(Span::raw(format!("↓ {}/s  ", human_size(down as u64))));
    }
    if up > 0.0 {
        right.push(Span::raw(format!("↑ {}/s  ", human_size(up as u64))));
    }
    right.push(match &app.shares {
        SharesStatus::Scanning => Span::raw("scanning shares  ").dark_gray(),
        SharesStatus::Ready { files, .. } => {
            Span::raw(format!("sharing {files} files  ")).dark_gray()
        }
    });
    right.push(Span::raw(format!("{} ", app.username)));
    right.push(if app.connected {
        Span::raw("● online ").green()
    } else {
        Span::raw("● offline ").red()
    });
    frame.render_widget(Paragraph::new(Line::from(right)).right_aligned(), area);
}

fn render_search(frame: &mut Frame, app: &mut App, area: Rect) {
    let [input_area, list_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);

    let editing = app.focus == Focus::Input;
    let input_block = Block::bordered()
        .title(" Search ")
        .border_style(if editing {
            Style::new().cyan()
        } else {
            Style::new().dark_gray()
        });
    frame.render_widget(
        Paragraph::new(app.input.as_str()).block(input_block),
        input_area,
    );
    if editing {
        let x = input_area.x + 1 + app.input.chars().count() as u16;
        frame.set_cursor_position(Position::new(
            x.min(input_area.right().saturating_sub(2)),
            input_area.y + 1,
        ));
    }

    let title = match &app.search {
        None => " Results ".to_owned(),
        Some(s) => {
            let secs = s.started.elapsed().as_secs();
            let filter = app.results.filter();
            let shown = if filter == FormatFilter::All {
                String::new()
            } else {
                format!(
                    " · [{}] {} folders",
                    filter.label(),
                    app.results.visible_folders()
                )
            };
            format!(
                " {:?}: {} users, {} files ({}s){shown} ",
                s.query,
                app.results.users,
                app.results.file_count(),
                secs
            )
        }
    };
    let empty = if !app.results.is_empty() {
        "no results in this format – press f to change the filter"
    } else if app.search.is_some() {
        "waiting for results..."
    } else {
        "press s or / to search"
    };
    render_result_list(
        frame,
        &mut app.results,
        &mut app.results_offset,
        &mut app.page_size,
        ResultList {
            area: list_area,
            title,
            focused: !editing,
            empty,
            show_user: true,
        },
    );
}

struct ResultList<'a> {
    area: Rect,
    title: String,
    focused: bool,
    empty: &'a str,
    /// Search results show who has each folder; a browsed user's own
    /// listing does not need it.
    show_user: bool,
}

fn render_result_list(
    frame: &mut Frame,
    results: &mut Results,
    offset: &mut usize,
    page_size: &mut usize,
    list: ResultList,
) {
    let block = Block::bordered()
        .title(list.title)
        .border_style(if list.focused {
            Style::new().cyan()
        } else {
            Style::new().dark_gray()
        });
    let inner = block.inner(list.area);
    frame.render_widget(block, list.area);

    if results.rows().is_empty() {
        frame.render_widget(Paragraph::new(list.empty).dark_gray(), inner);
        return;
    }

    let height = inner.height as usize;
    *page_size = height;
    let selected = results.selected_index().unwrap_or(0);
    *offset = scroll(*offset, selected, height);
    let visible: Vec<Row> = results
        .rows()
        .iter()
        .skip(*offset)
        .take(height)
        .copied()
        .collect();
    let rows: Vec<TableRow> = visible
        .into_iter()
        .map(|row| result_row(results, row, list.show_user))
        .collect();
    let widths = if list.show_user {
        vec![
            Constraint::Fill(1),
            Constraint::Length(22),
            Constraint::Length(9),
            Constraint::Length(18),
            Constraint::Length(20),
        ]
    } else {
        vec![
            Constraint::Fill(1),
            Constraint::Length(22),
            Constraint::Length(9),
        ]
    };
    let table = Table::new(rows, widths).row_highlight_style(if list.focused {
        SELECTED
    } else {
        Style::new()
    });
    let mut state = TableState::new().with_selected(Some(selected - *offset));
    frame.render_stateful_widget(table, inner, &mut state);
}

fn result_row(results: &Results, row: Row, show_user: bool) -> TableRow<'static> {
    match row {
        Row::Folder(id) => {
            let filter = results.filter();
            let f = results.folder(id);
            let marker = if results.is_expanded(id) {
                "▾"
            } else {
                "▸"
            };
            // Browsing one user's tree, more of the path is useful.
            let name = last_components(&f.path, if show_user { 2 } else { 4 });
            let availability = if f.slot_free {
                Span::raw(format!("free  {}/s", human_size(f.avg_speed.into()))).green()
            } else {
                Span::raw(format!("queue {}", f.queue_length)).yellow()
            };
            let mut cells = vec![
                Cell::from(format!(
                    "{marker} {name}  ({})",
                    f.visible_files(filter).len()
                ))
                .bold(),
                Cell::from(f.quality(filter)),
                Cell::from(human_size(f.total_size(filter))),
            ];
            if show_user {
                cells.push(Cell::from(f.username.clone()).cyan());
                cells.push(Cell::from(availability));
            }
            TableRow::new(cells)
        }
        Row::File(id, i) => {
            let file = &results.folder(id).files[i];
            TableRow::new(vec![
                Cell::from(format!("    {}", file.basename())),
                Cell::from(quality(file)).dark_gray(),
                Cell::from(human_size(file.size)),
            ])
        }
    }
}

fn render_transfers(frame: &mut Frame, app: &mut App, area: Rect) {
    let [list_area, detail_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(4)]).areas(area);

    let block = Block::bordered()
        .title(" Downloads ")
        .border_style(Style::new().cyan());
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);

    if app.transfers.list.is_empty() {
        frame.render_widget(
            Paragraph::new("nothing yet – select a file or folder in Search and press d")
                .dark_gray(),
            inner,
        );
    } else {
        let height = inner.height as usize;
        app.page_size = height;
        let selected = app.transfers.selected;
        app.transfers_offset = scroll(app.transfers_offset, selected, height);
        let offset = app.transfers_offset;

        let rows: Vec<TableRow> = app
            .transfers
            .list
            .iter()
            .skip(offset)
            .take(height)
            .map(|t| {
                let (status, bar, speed) = match &t.state {
                    DownloadState::Queued { place } => (
                        Span::raw(match place {
                            Some(p) => format!("queued #{p}"),
                            None => "queued".to_owned(),
                        })
                        .yellow(),
                        Line::default(),
                        String::new(),
                    ),
                    DownloadState::Transferring { received, size } => (
                        Span::raw("downloading").cyan(),
                        progress_bar(*received, *size, 20),
                        if t.meter.speed > 0.0 {
                            format!("{}/s", human_size(t.meter.speed as u64))
                        } else {
                            String::new()
                        },
                    ),
                    DownloadState::Completed { .. } => (
                        Span::raw("done").green(),
                        progress_bar(1, 1, 20),
                        String::new(),
                    ),
                    DownloadState::Failed { .. } => {
                        (Span::raw("failed").red(), Line::default(), String::new())
                    }
                };
                TableRow::new(vec![
                    Cell::from(status),
                    Cell::from(bar),
                    Cell::from(speed),
                    Cell::from(t.username.clone()).cyan(),
                    Cell::from(t.basename().to_owned()),
                ])
            })
            .collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(26),
                Constraint::Length(11),
                Constraint::Length(18),
                Constraint::Fill(1),
            ],
        )
        .row_highlight_style(SELECTED);
        let mut state = TableState::new().with_selected(Some(selected - offset));
        frame.render_stateful_widget(table, inner, &mut state);
    }

    let detail: Vec<Line> = match app.transfers.selected() {
        None => vec![],
        Some(t) => {
            let state = match &t.state {
                DownloadState::Queued { place: Some(p) } => format!("queued, place {p}"),
                DownloadState::Queued { place: None } => "queued, waiting for the peer".to_owned(),
                DownloadState::Transferring { received, size } => {
                    format!("{} of {}", human_size(*received), human_size(*size))
                }
                DownloadState::Completed { path } => format!("saved to {}", path.display()),
                DownloadState::Failed { reason } => format!("failed: {reason}"),
            };
            vec![
                Line::from(t.filename.clone()).dark_gray(),
                Line::from(state),
            ]
        }
    };
    frame.render_widget(
        Paragraph::new(detail).block(Block::bordered().border_style(Style::new().dark_gray())),
        detail_area,
    );
}

fn render_browse(frame: &mut Frame, app: &mut App, area: Rect) {
    let [input_area, list_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
    let editing = app.browse_focus == Focus::Input;
    let input_block = Block::bordered()
        .title(" Browse user ")
        .border_style(if editing {
            Style::new().cyan()
        } else {
            Style::new().dark_gray()
        });
    frame.render_widget(
        Paragraph::new(app.browse_input.as_str()).block(input_block),
        input_area,
    );
    if editing {
        let x = input_area.x + 1 + app.browse_input.chars().count() as u16;
        frame.set_cursor_position(Position::new(
            x.min(input_area.right().saturating_sub(2)),
            input_area.y + 1,
        ));
    }

    let (title, empty) = match &app.browse {
        None => (
            " Shares ".to_owned(),
            "type a username and press Enter, or press b on a search result".to_owned(),
        ),
        Some(b) if b.loaded => {
            let r = &mut app.browse_results;
            let title = format!(
                " {}: {} folders, {} files ",
                b.username,
                r.visible_folders(),
                r.file_count()
            );
            let empty = if r.is_empty() {
                format!("{} shares nothing", b.username)
            } else {
                "nothing in this format – press f to change the filter".to_owned()
            };
            (title, empty)
        }
        Some(b) => (
            format!(" {} ", b.username),
            match &b.error {
                Some(e) => e.clone(),
                None => format!(
                    "asking {} for their share list... ({}s)",
                    b.username,
                    b.started.elapsed().as_secs()
                ),
            },
        ),
    };
    render_result_list(
        frame,
        &mut app.browse_results,
        &mut app.browse_offset,
        &mut app.page_size,
        ResultList {
            area: list_area,
            title,
            focused: !editing,
            empty: &empty,
            show_user: false,
        },
    );
}

fn render_uploads(frame: &mut Frame, app: &mut App, area: Rect) {
    let completed = app.uploads.completed;
    let block = Block::bordered()
        .title(format!(
            " Uploads · {completed} completed · {} searches answered this session ",
            app.searches_answered
        ))
        .border_style(Style::new().cyan());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.uploads.list.is_empty() {
        let hint = match &app.shares {
            SharesStatus::Ready { files: 0, .. } => {
                "you share nothing yet – add a folder in Settings (4)"
            }
            _ => "nobody is downloading from you right now",
        };
        frame.render_widget(Paragraph::new(hint).dark_gray(), inner);
        return;
    }

    let height = inner.height as usize;
    app.page_size = height;
    let selected = app.uploads.selected;
    app.uploads_offset = scroll(app.uploads_offset, selected, height);
    let offset = app.uploads_offset;
    let rows: Vec<TableRow> = app
        .uploads
        .list
        .iter()
        .skip(offset)
        .take(height)
        .map(|u| {
            let (status, bar, speed) = match &u.state {
                UploadState::Queued => {
                    (Span::raw("queued").yellow(), Line::default(), String::new())
                }
                UploadState::Starting => (
                    Span::raw("starting").yellow(),
                    Line::default(),
                    String::new(),
                ),
                UploadState::Transferring { sent, size } => (
                    Span::raw("uploading").cyan(),
                    progress_bar(*sent, *size, 20),
                    if u.meter.speed > 0.0 {
                        format!("{}/s", human_size(u.meter.speed as u64))
                    } else {
                        String::new()
                    },
                ),
                UploadState::Completed => (
                    Span::raw("done").green(),
                    progress_bar(1, 1, 20),
                    String::new(),
                ),
                UploadState::Failed { reason } => (
                    Span::raw("failed").red(),
                    Line::from(reason.clone()).dark_gray(),
                    String::new(),
                ),
            };
            TableRow::new(vec![
                Cell::from(status),
                Cell::from(bar),
                Cell::from(speed),
                Cell::from(u.username.clone()).cyan(),
                Cell::from(u.basename().to_owned()),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(26),
            Constraint::Length(11),
            Constraint::Length(18),
            Constraint::Fill(1),
        ],
    )
    .row_highlight_style(SELECTED);
    let mut state = TableState::new().with_selected(Some(selected - offset));
    frame.render_stateful_widget(table, inner, &mut state);
}

fn render_buddies(frame: &mut Frame, app: &mut App, area: Rect) {
    let list_area = match &app.buddies.adding {
        Some(input) => {
            let [input_area, list_area] =
                Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
            frame.render_widget(
                Paragraph::new(input.as_str()).block(
                    Block::bordered()
                        .title(" Add buddy ")
                        .border_style(Style::new().cyan()),
                ),
                input_area,
            );
            let x = input_area.x + 1 + input.chars().count() as u16;
            frame.set_cursor_position(Position::new(
                x.min(input_area.right().saturating_sub(2)),
                input_area.y + 1,
            ));
            list_area
        }
        None => area,
    };

    let b = &app.buddies;
    let block = Block::bordered()
        .title(format!(
            " Buddies ({}/{} online) ",
            b.online(),
            b.list.len()
        ))
        .border_style(if b.adding.is_some() {
            Style::new().dark_gray()
        } else {
            Style::new().cyan()
        });
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);

    if b.list.is_empty() {
        frame.render_widget(
            Paragraph::new("no buddies yet – press a to add one, or A on a search result")
                .dark_gray(),
            inner,
        );
        return;
    }

    let height = inner.height as usize;
    app.page_size = height;
    let selected = app.buddies.selected;
    app.buddies_offset = scroll(app.buddies_offset, selected, height);
    let offset = app.buddies_offset;
    let rows: Vec<TableRow> = app
        .buddies
        .list
        .iter()
        .skip(offset)
        .take(height)
        .map(|buddy| {
            let name = Cell::from(buddy.username.clone()).cyan();
            match &buddy.known {
                None => TableRow::new(vec![Cell::from("…  checking").dark_gray(), name]),
                Some(Known::Missing) => {
                    TableRow::new(vec![Cell::from("✗  no such user").red(), name])
                }
                Some(Known::Exists {
                    status,
                    stats,
                    country,
                }) => {
                    let status = match status {
                        OnlineStatus::Online => Span::raw("●  online").green(),
                        OnlineStatus::Away => Span::raw("◐  away").yellow(),
                        OnlineStatus::Offline => Span::raw("○  offline").dark_gray(),
                    };
                    let shares = format!("{} files · {} dirs", stats.files, stats.dirs);
                    let speed = if stats.avg_speed > 0 {
                        format!("{}/s", human_size(u64::from(stats.avg_speed)))
                    } else {
                        String::new()
                    };
                    TableRow::new(vec![
                        Cell::from(status),
                        name,
                        Cell::from(shares).dark_gray(),
                        Cell::from(speed).dark_gray(),
                        Cell::from(country.clone().unwrap_or_default()).dark_gray(),
                    ])
                }
            }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(24),
            Constraint::Length(26),
            Constraint::Length(12),
            Constraint::Fill(1),
        ],
    )
    .row_highlight_style(SELECTED);
    let mut state = TableState::new().with_selected(Some(selected - offset));
    frame.render_stateful_widget(table, inner, &mut state);
}

fn help_line(app: &App) -> &'static str {
    match (app.tab, app.focus) {
        (Tab::Search, Focus::Input) => {
            " Enter search · Esc/Alt-s results · Ctrl-u clear · Ctrl-c quit"
        }
        (Tab::Search, Focus::List) => {
            " 10j/10k jump · j/k move · Enter open folder · h/l collapse/expand · d download · b browse user · A add buddy · f/F format filter · s search · Tab/Alt-1…6 tabs · q quit"
        }
        (Tab::Transfers, _) => {
            " j/k move · c cancel · r retry failed · x clear finished · A add buddy · s search · Tab/Alt-1…6 tabs · q quit"
        }
        (Tab::Browse, _) if app.browse_focus == Focus::Input => {
            " Enter browse user · Esc list · Ctrl-u clear · Ctrl-c quit"
        }
        (Tab::Browse, _) => {
            " j/k move · Enter open folder · d download · A add buddy · f/F format filter · / other user · Tab/Alt-1…6 tabs · q quit"
        }
        (Tab::Uploads, _) => {
            " j/k move · c cancel · x clear finished · A add buddy · s search · Tab/Alt-1…6 tabs · q quit"
        }
        (Tab::Settings, _) if app.settings.is_editing() => {
            " Tab complete folder · Enter save · Esc cancel · Ctrl-u clear"
        }
        (Tab::Settings, _) => {
            " j/k move · Enter edit/toggle · a add shared folder · x remove · s search · Tab/Alt-1…6 tabs · q quit"
        }
        (Tab::Buddies, _) if app.buddies.adding.is_some() => {
            " Enter add buddy · Esc cancel · Ctrl-u clear · Ctrl-c quit"
        }
        (Tab::Buddies, _) => {
            " j/k move · a add buddy · x remove · Enter/b browse shares · s search · Tab/Alt-1…6 tabs · q quit"
        }
    }
}

fn render_settings(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered()
        .title(" Settings ")
        .border_style(Style::new().cyan());
    let inner = block.inner(area).inner(ratatui::layout::Margin::new(1, 0));
    frame.render_widget(block, area);

    let s = &app.settings;
    let editing = s.edit.as_ref();
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor = None;
    let mut push_item = |lines: &mut Vec<Line>, index: usize, item: Item, text: String| {
        let selected = s.selected == index;
        let marker = if selected { "› " } else { "  " };
        match editing.filter(|e| e.item == item) {
            Some(edit) => {
                cursor = Some((
                    lines.len(),
                    marker.chars().count() + edit.text.chars().count(),
                ));
                lines.push(Line::from(vec![
                    Span::raw(marker),
                    Span::raw(edit.text.clone()).reversed(),
                ]));
            }
            None if selected && editing.is_none() => lines.push(Line::from(vec![
                Span::raw(marker),
                Span::raw(text).bold().cyan(),
            ])),
            None => lines.push(Line::from(vec![Span::raw(marker), Span::raw(text)])),
        }
    };

    lines.push(Line::from("Download folder").bold());
    lines.push(Line::from("  Where downloads are saved, one folder per album.").dark_gray());
    push_item(
        &mut lines,
        0,
        Item::DownloadDir,
        display_path(&s.download_dir),
    );
    lines.push(Line::default());
    lines.push(Line::from("Listen port").bold());
    lines.push(
        Line::from("  TCP port other users connect to; forward it on your router.").dark_gray(),
    );
    push_item(&mut lines, 1, Item::ListenPort, s.listen_port.to_string());
    push_item(
        &mut lines,
        2,
        Item::Upnp,
        format!(
            "[{}] open it on the router automatically (UPnP)",
            if s.upnp { "x" } else { " " }
        ),
    );
    lines.push(match &app.portmap {
        PortMapStatus::Disabled => {
            Line::from("  off – forward the port on your router by hand").dark_gray()
        }
        PortMapStatus::Trying => Line::from("  asking the router...").yellow(),
        PortMapStatus::Mapped(m) if m.behind_another_nat() => Line::from(format!(
            "  port {} open on the router, but the router sits behind another NAT ({}) – \
             that device may need a forward too",
            m.port,
            m.external_ip.map(|ip| ip.to_string()).unwrap_or_default()
        ))
        .yellow(),
        PortMapStatus::Mapped(m) if m.already_mapped => Line::from(format!(
            "  the router already forwards port {} (manual rule)",
            m.port
        ))
        .green(),
        PortMapStatus::Mapped(m) => {
            Line::from(format!("  port {} open on the router", m.port)).green()
        }
        PortMapStatus::Failed(e) => Line::from(format!("  {e} – forward the port by hand")).red(),
    });
    lines.push(Line::default());
    lines.push(Line::from("Distributed network").bold());
    lines.push(
        Line::from(match &app.distrib {
            DistribStatus::Searching => "  looking for a parent...".to_owned(),
            DistribStatus::Parent { username, level } => {
                format!("  connected through {username} (level {})", level + 1)
            }
            DistribStatus::BranchRoot => {
                "  branch root: the server sends searches directly".to_owned()
            }
        })
        .green(),
    );
    lines.push(
        Line::from(format!(
            "  {} searches from other users answered this session",
            app.searches_answered
        ))
        .dark_gray(),
    );
    lines.push(Line::default());
    lines.push(Line::from("Shared folders").bold());
    lines.push(
        Line::from("  Music other users can search, browse and download from you.").dark_gray(),
    );
    lines.push(
        Line::from(match &app.shares {
            SharesStatus::Scanning => "  scanning...".to_owned(),
            SharesStatus::Ready { folders, files } => {
                format!("  sharing {files} files in {folders} folders")
            }
        })
        .green(),
    );
    for (i, dir) in s.shared.iter().enumerate() {
        push_item(&mut lines, i + 3, Item::Shared(i), display_path(dir));
    }
    push_item(
        &mut lines,
        s.shared.len() + 3,
        Item::AddShared,
        "+ add folder".to_owned(),
    );
    if let Some(error) = &s.error {
        lines.push(Line::default());
        lines.push(Line::from(error.as_str()).red());
    }
    lines.push(Line::default());
    lines.push(
        Line::from(format!(
            "  Saved to {}",
            config::path().map(|p| display_path(&p)).unwrap_or_default()
        ))
        .dark_gray(),
    );

    if let Some((row, col)) = cursor {
        frame.set_cursor_position(Position::new(
            (inner.x + col as u16).min(inner.right().saturating_sub(1)),
            inner.y + row as u16,
        ));
    }
    // No wrapping: the cursor position above assumes one line per entry.
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Keeps `selected` inside a window of `height` rows starting at `offset`.
fn scroll(offset: usize, selected: usize, height: usize) -> usize {
    if height == 0 {
        return selected;
    }
    if selected < offset {
        selected
    } else if selected >= offset + height {
        selected + 1 - height
    } else {
        offset
    }
}

/// A thin line bar: done part green, rest dim, then the percentage.
fn progress_bar(received: u64, size: u64, width: usize) -> Line<'static> {
    let ratio = if size == 0 {
        1.0
    } else {
        (received as f64 / size as f64).clamp(0.0, 1.0)
    };
    let filled = (ratio * width as f64).round() as usize;
    Line::from(vec![
        Span::raw("━".repeat(filled)).green(),
        Span::raw("─".repeat(width - filled)).dark_gray(),
        Span::raw(format!(" {:>3.0}%", ratio * 100.0)),
    ])
}

/// The last `n` path components, e.g. `Artist\Album`.
fn last_components(path: &str, n: usize) -> String {
    let parts: Vec<&str> = path.split(['\\', '/']).collect();
    parts[parts.len().saturating_sub(n)..].join("\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_window() {
        assert_eq!(scroll(0, 3, 10), 0);
        assert_eq!(scroll(0, 10, 10), 1);
        assert_eq!(scroll(5, 2, 10), 2);
        assert_eq!(scroll(5, 14, 10), 5);
    }

    #[test]
    fn bars_and_paths() {
        assert_eq!(progress_bar(50, 100, 4).to_string(), "━━──  50%");
        assert_eq!(progress_bar(0, 0, 2).to_string(), "━━ 100%");
        assert_eq!(
            last_components("@@a\\Music\\Artist\\Album", 2),
            "Artist\\Album"
        );
        assert_eq!(last_components("Album", 2), "Album");
    }
}
