//! Rendering. Only the visible rows of each list are built, so large
//! result sets stay cheap to draw.

use crabseek_net::{DistribStatus, DownloadState, PortMapStatus, UploadState};
use crabseek_proto::server::OnlineStatus;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row as TableRow, Table, TableState, Tabs, Wrap};

use super::app::{App, Focus, SharesStatus, Tab};
use super::buddies::Known;
use super::chat::ChatInput;
use super::input::TextInput;
use super::results::{FormatFilter, Results, Row};
use super::settings::Item;
use super::transfers::{Summary, Transfer, TransferRow};
use crate::config::{self, display_path};
use crate::search::{human_size, quality};
use unicode_width::UnicodeWidthStr;

/// The cursor row: one even bar. Setting the foreground too keeps cells
/// with their own colours (green, cyan, dark gray) readable on it.
const SELECTED: Style = Style::new()
    .fg(Color::White)
    .bg(Color::DarkGray)
    .add_modifier(Modifier::BOLD);

/// The cursor row of a list whose input box has the focus: still visible,
/// but quieter than [`SELECTED`].
const SELECTED_DIM: Style = Style::new().fg(Color::Gray).bg(Color::DarkGray);

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
        Tab::Wishlist => render_wishlist(frame, app, body),
        Tab::Chat => render_chat(frame, app, body),
    }
    frame.render_widget(
        Paragraph::new(app.status.as_str()).fg(
            if app.status_error.as_deref() == Some(app.status.as_str()) {
                Color::Red
            } else {
                Color::Yellow
            },
        ),
        status,
    );
    let mut help_spans = vec![Span::raw(" ? help ·").cyan()];
    let line = if app.background {
        help_line(app).replace("q quit", "q detach · Q stop")
    } else {
        help_line(app).to_owned()
    };
    help_spans.push(Span::raw(line).dark_gray());
    frame.render_widget(Paragraph::new(Line::from(help_spans)), help);

    if app.help {
        super::help::render(frame, app);
    }
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    // Tabs pads each title with one space on both sides.
    let counted = |label: &str, n: usize| {
        Line::from(if n > 0 {
            format!("{label} ({n})")
        } else {
            label.to_owned()
        })
    };
    let titles: Vec<Line> = Tab::ORDER
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let label = format!("{} {}", i + 1, tab.label());
            match tab {
                Tab::Transfers => counted(&label, app.transfers.active()),
                Tab::Uploads => counted(&label, app.uploads.active()),
                Tab::Buddies => counted(&label, app.buddies.online()),
                Tab::Chat => counted(&label, app.chats.total_unread()),
                Tab::Wishlist => match app.wishlist.total_new() {
                    0 => Line::from(label),
                    n => Line::from(format!("{label} ({n} new)")),
                },
                _ => Line::from(label),
            }
        })
        .collect();
    let selected = app.tab.index();
    let tabs_width: u16 = titles.iter().map(|t| t.width() as u16 + 2).sum();
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(Style::new().bold().reversed())
            .divider(""),
        area,
    );

    // The right side gets what the tabs leave; when that is short, the
    // least important parts go first (sharing, username, then speeds).
    let down = app.transfers.total_speed();
    let up = app.uploads.total_speed();
    let mut parts: Vec<(u8, Span)> = Vec::new();
    if down > 0.0 {
        parts.push((2, Span::raw(format!("↓ {}/s  ", human_size(down as u64)))));
    }
    if up > 0.0 {
        parts.push((2, Span::raw(format!("↑ {}/s  ", human_size(up as u64)))));
    }
    parts.push((
        0,
        match &app.shares {
            SharesStatus::Scanning => Span::raw("scanning shares  ").dark_gray(),
            SharesStatus::Ready { files, .. } => {
                Span::raw(format!("sharing {files} files  ")).dark_gray()
            }
        },
    ));
    parts.push((1, Span::raw(format!("{} ", app.username))));
    parts.push((
        3,
        if app.connected {
            Span::raw("● online ").green()
        } else {
            Span::raw("● offline ").red()
        },
    ));
    let room = area.width.saturating_sub(tabs_width + 1) as usize;
    let mut keep: Vec<bool> = vec![true; parts.len()];
    let width = |keep: &[bool]| -> usize {
        parts
            .iter()
            .zip(keep)
            .filter(|(_, k)| **k)
            .map(|((_, s), _)| s.width())
            .sum()
    };
    for priority in 0..3 {
        if width(&keep) <= room {
            break;
        }
        for (k, (p, _)) in keep.iter_mut().zip(&parts) {
            if *p == priority {
                *k = false;
            }
        }
    }
    let right: Vec<Span> = parts
        .into_iter()
        .zip(keep)
        .filter_map(|((_, span), k)| k.then_some(span))
        .collect();
    let right_area = Rect {
        x: area.x + tabs_width.min(area.width),
        width: area.width.saturating_sub(tabs_width),
        ..area
    };
    frame.render_widget(
        Paragraph::new(Line::from(right)).right_aligned(),
        right_area,
    );
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
    render_input(frame, input_area, input_block, &app.input, editing);

    let title = match &app.search {
        None => format!(" Results{} ", filter_note(&mut app.results)),
        Some(s) => {
            let secs = s.started.elapsed().as_secs();
            let note = filter_note(&mut app.results);
            format!(
                " {:?}: {} users, {} files ({}s){note} ",
                s.query,
                app.results.users,
                app.results.file_count(),
                secs,
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

/// ` · [FLAC] 12 folders, 140 files` while a format filter is on: what is
/// actually listed, next to the unfiltered totals.
fn filter_note(r: &mut Results) -> String {
    let filter = r.filter();
    if filter == FormatFilter::All {
        return String::new();
    }
    if r.is_empty() {
        return format!(" · [{}]", filter.label());
    }
    format!(
        " · [{}] {} folders, {} files",
        filter.label(),
        r.visible_folders(),
        r.shown_file_count()
    )
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
    let cols = ResultColumns::fit(inner.width, list.show_user);
    let rows: Vec<TableRow> = visible
        .into_iter()
        .map(|row| result_row(results, row, list.show_user, cols))
        .collect();
    let mut widths = vec![
        Constraint::Fill(1),
        Constraint::Length(QUALITY_W),
        Constraint::Length(SIZE_W),
    ];
    if cols.user {
        widths.push(Constraint::Length(USER_W));
    }
    if cols.availability {
        widths.push(Constraint::Length(AVAILABILITY_W));
    }
    let table = Table::new(rows, widths).row_highlight_style(if list.focused {
        SELECTED
    } else {
        SELECTED_DIM
    });
    let mut state = TableState::new().with_selected(Some(selected - *offset));
    frame.render_stateful_widget(table, inner, &mut state);
}

const QUALITY_W: u16 = 22;
const SIZE_W: u16 = 9;
const USER_W: u16 = 18;
const AVAILABILITY_W: u16 = 20;
/// Narrower than this, the name loses to the optional columns.
const NAME_MIN: u16 = 32;

/// The result list's name width and which optional columns fit. On a
/// narrow terminal availability goes first, then the user.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ResultColumns {
    name: u16,
    user: bool,
    availability: bool,
}

impl ResultColumns {
    fn fit(width: u16, show_user: bool) -> Self {
        // Each column after the first is preceded by one space.
        let base = QUALITY_W + 1 + SIZE_W + 1;
        let user = base + USER_W + 1;
        let all = user + AVAILABILITY_W + 1;
        let (taken, user, availability) = if show_user && width >= all + NAME_MIN {
            (all, true, true)
        } else if show_user && width >= user + NAME_MIN {
            (user, true, false)
        } else {
            (base, false, false)
        };
        Self {
            name: width.saturating_sub(taken),
            user,
            availability,
        }
    }
}

/// Columns of the Downloads and Uploads lists. On a narrow terminal the
/// user goes first, then the bar shrinks, so the file name keeps
/// [`NAME_MIN`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct TransferColumns {
    bar: usize,
    user: bool,
    name: usize,
}

impl TransferColumns {
    const STATUS_W: usize = 12;
    const SPEED_W: usize = 11;
    const USER_W: usize = 18;

    fn fit(width: u16) -> Self {
        let with = |bar: usize, user: bool| {
            // The bar is followed by its percentage; each column after the
            // first is preceded by one space.
            let mut taken = Self::STATUS_W + 1 + bar + 6 + 1 + Self::SPEED_W + 1;
            if user {
                taken += Self::USER_W + 1;
            }
            Self {
                bar,
                user,
                name: usize::from(width).saturating_sub(taken),
            }
        };
        [with(20, true), with(20, false)]
            .into_iter()
            .find(|c| c.name >= usize::from(NAME_MIN))
            .unwrap_or_else(|| with(10, false))
    }

    fn widths(self) -> Vec<Constraint> {
        let mut widths = vec![
            Constraint::Length(Self::STATUS_W as u16),
            Constraint::Length(self.bar as u16 + 6),
            Constraint::Length(Self::SPEED_W as u16),
        ];
        if self.user {
            widths.push(Constraint::Length(Self::USER_W as u16));
        }
        widths.push(Constraint::Fill(1));
        widths
    }
}

fn result_row(
    results: &Results,
    row: Row,
    show_user: bool,
    cols: ResultColumns,
) -> TableRow<'static> {
    let name_width = usize::from(cols.name);
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
            // The file count stays visible; the name gives way.
            let count = format!("  ({})", f.visible_count(filter));
            let room = name_width.saturating_sub(2 + count.width());
            let mut cells = vec![
                Cell::from(format!("{marker} {}{count}", ellipsis(&name, room))).bold(),
                Cell::from(f.quality(filter)),
                Cell::from(human_size(f.total_size(filter))),
            ];
            if cols.user {
                cells.push(Cell::from(ellipsis(&f.username, USER_W.into())).cyan());
            }
            if cols.availability {
                cells.push(Cell::from(if f.slot_free {
                    Span::raw(format!("free  {}/s", human_size(f.avg_speed.into()))).green()
                } else {
                    Span::raw(format!("queue {}", f.queue_length)).yellow()
                }));
            }
            TableRow::new(cells)
        }
        Row::File(id, i) => {
            let file = &results.folder(id).files[i];
            TableRow::new(vec![
                Cell::from(format!(
                    "    {}",
                    ellipsis(file.basename(), name_width.saturating_sub(4))
                )),
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
            Paragraph::new(
                "Nothing downloading. On the Search or Browse tab, select a file or a whole \
                 folder and press d; it shows up here with its progress. Unfinished downloads \
                 continue after a restart. ? explains more.",
            )
            .wrap(Wrap { trim: true })
            .dark_gray(),
            inner,
        );
    } else {
        let height = inner.height as usize;
        app.page_size = height;
        let transfers = &app.transfers;
        let rows = transfers.rows();
        let selected = rows
            .iter()
            .position(|&r| r == transfers.cursor())
            .unwrap_or(0);
        let mut offset = scroll(app.transfers_offset, selected, height);
        // Moving up onto a group's first file brings its headings along.
        while offset > 0
            && !matches!(rows[offset - 1], TransferRow::Transfer(_))
            && selected + 1 - (offset - 1) <= height
        {
            offset -= 1;
        }
        app.transfers_offset = offset;
        let mut cols = TransferColumns::fit(inner.width);
        // Grouped, the user heads their downloads instead of filling a column.
        if !transfers.is_flat() && cols.user {
            cols.user = false;
            cols.name += TransferColumns::USER_W + 1;
        }

        let table_rows: Vec<TableRow> = rows
            .iter()
            .skip(offset)
            .take(height)
            .map(|&row| match row {
                TransferRow::User(i) => {
                    let t = &transfers.list[i];
                    let mut cells = group_cells(&transfers.summary(row), None);
                    let marker = open_marker(transfers.is_open(row));
                    cells.push(
                        Cell::from(format!(
                            "{marker} {}",
                            ellipsis(&t.username, cols.name.saturating_sub(2))
                        ))
                        .cyan()
                        .bold(),
                    );
                    TableRow::new(cells)
                }
                TransferRow::Folder(i) => {
                    let t = &transfers.list[i];
                    let mut cells = group_cells(&transfers.summary(row), Some(cols.bar));
                    let marker = open_marker(transfers.is_open(row));
                    let name = last_components(t.folder(), 2);
                    cells.push(
                        Cell::from(format!(
                            "  {marker} {}",
                            ellipsis(&name, cols.name.saturating_sub(4))
                        ))
                        .bold(),
                    );
                    TableRow::new(cells)
                }
                TransferRow::Transfer(i) => {
                    let t = &transfers.list[i];
                    let name = if transfers.is_flat() {
                        ellipsis(t.basename(), cols.name)
                    } else {
                        format!(
                            "    {}",
                            ellipsis(t.basename(), cols.name.saturating_sub(4))
                        )
                    };
                    TableRow::new(transfer_cells(t, cols, name))
                }
            })
            .collect();
        let table = Table::new(table_rows, cols.widths()).row_highlight_style(SELECTED);
        let mut state = TableState::new().with_selected(Some(selected - offset));
        frame.render_stateful_widget(table, inner, &mut state);
    }

    // Inside the detail block's borders.
    let width = usize::from(detail_area.width.saturating_sub(2));
    let transfers = &app.transfers;
    let detail: Vec<Line> = match (transfers.cursor(), transfers.cursor_transfer()) {
        (_, None) => vec![],
        (row @ (TransferRow::User(_) | TransferRow::Folder(_)), Some(t)) => {
            let sum = transfers.summary(row);
            let mut counts = format!("{} files, {} done", sum.total, sum.done);
            if sum.running > 0 {
                counts.push_str(&format!(", {} downloading", sum.running));
            }
            if sum.failed > 0 {
                counts.push_str(&format!(", {} failed", sum.failed));
            }
            let head = match row {
                TransferRow::Folder(_) => t.folder(),
                _ => &t.username,
            };
            vec![
                Line::from(ellipsis_start(head, width)).dark_gray(),
                Line::from(ellipsis(&counts, width)),
            ]
        }
        (TransferRow::Transfer(_), Some(t)) => {
            let state = match &t.state {
                DownloadState::Queued { place: Some(p) } => format!("queued, place {p}"),
                DownloadState::Queued { place: None } => "queued, waiting for the peer".to_owned(),
                DownloadState::Transferring { received, size } => {
                    format!("{} of {}", human_size(*received), human_size(*size))
                }
                DownloadState::Completed { path } => format!(
                    "saved to {}",
                    ellipsis_start(&path.display().to_string(), width.saturating_sub(9))
                ),
                DownloadState::Failed { reason } => format!("failed: {reason}"),
            };
            vec![
                Line::from(ellipsis_start(&t.filename, width)).dark_gray(),
                Line::from(ellipsis(&state, width)),
            ]
        }
    };
    frame.render_widget(
        Paragraph::new(detail).block(Block::bordered().border_style(Style::new().dark_gray())),
        detail_area,
    );
}

fn transfer_cells(t: &Transfer, cols: TransferColumns, name: String) -> Vec<Cell<'static>> {
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
            progress_bar(*received, *size, cols.bar),
            speed_text(t.meter.speed),
        ),
        DownloadState::Completed { .. } => (
            Span::raw("done").green(),
            finished_bar(cols.bar),
            String::new(),
        ),
        DownloadState::Failed { .. } => (Span::raw("failed").red(), Line::default(), String::new()),
    };
    let mut cells = vec![Cell::from(status), Cell::from(bar), Cell::from(speed)];
    if cols.user {
        cells.push(Cell::from(ellipsis(&t.username, 18)).cyan());
    }
    cells.push(Cell::from(name));
    cells
}

/// Status, bar and speed cells summing up a group of downloads: how many
/// files are done, coloured by what the rest are doing. The bar counts
/// files, as queued ones have no known size yet; `None` leaves it out.
fn group_cells(sum: &Summary, bar: Option<usize>) -> Vec<Cell<'static>> {
    let status = Span::raw(format!("{}/{} done", sum.done, sum.total));
    let status = if sum.done == sum.total {
        status.green()
    } else if sum.running > 0 {
        status.cyan()
    } else if sum.failed > 0 {
        status.red()
    } else {
        status.yellow()
    };
    vec![
        Cell::from(status),
        Cell::from(match bar {
            None => Line::default(),
            Some(w) if sum.done == sum.total => finished_bar(w),
            Some(w) => progress_bar(sum.done, sum.total, w),
        }),
        Cell::from(speed_text(sum.speed)),
    ]
}

fn open_marker(open: bool) -> &'static str {
    if open { "▾" } else { "▸" }
}

fn speed_text(speed: f64) -> String {
    if speed > 0.0 {
        format!("{}/s", human_size(speed as u64))
    } else {
        String::new()
    }
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
    render_input(frame, input_area, input_block, &app.browse_input, editing);

    let (title, empty) = match &app.browse {
        None => (
            format!(" Shares{} ", filter_note(&mut app.browse_results)),
            "See everything one user shares. Press b to type a username, or b on a search \
             result. ? explains more."
                .to_owned(),
        ),
        Some(b) if b.loaded => {
            let r = &mut app.browse_results;
            let title = format!(
                " {}: {} folders, {} files{} ",
                b.username,
                r.folder_count(),
                r.file_count(),
                filter_note(r)
            );
            let empty = if r.is_empty() {
                format!("{} shares nothing", b.username)
            } else {
                "nothing in this format – press f to change the filter".to_owned()
            };
            (title, empty)
        }
        Some(b) => (
            format!(" {}{} ", b.username, filter_note(&mut app.browse_results)),
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
                "you share nothing yet – add a folder in Settings (8)"
            }
            _ => {
                "Nobody is downloading from you right now. When someone does, it shows up \
                 here. Other users find your files through their searches and by browsing you."
            }
        };
        frame.render_widget(Paragraph::new(hint).dark_gray(), inner);
        return;
    }

    let height = inner.height as usize;
    app.page_size = height;
    let selected = app.uploads.selected;
    app.uploads_offset = scroll(app.uploads_offset, selected, height);
    let offset = app.uploads_offset;
    let cols = TransferColumns::fit(inner.width);
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
                    progress_bar(*sent, *size, cols.bar),
                    if u.meter.speed > 0.0 {
                        format!("{}/s", human_size(u.meter.speed as u64))
                    } else {
                        String::new()
                    },
                ),
                UploadState::Completed => (
                    Span::raw("done").green(),
                    finished_bar(cols.bar),
                    String::new(),
                ),
                UploadState::Failed { reason } => (
                    Span::raw("failed").red(),
                    Line::from(ellipsis(reason, cols.bar + 6)).dark_gray(),
                    String::new(),
                ),
            };
            let mut row = vec![
                Cell::from(status),
                Cell::from(bar),
                Cell::from(speed),
                Cell::from(ellipsis(&u.username, 18)).cyan(),
                Cell::from(ellipsis(u.basename(), cols.name)),
            ];
            if !cols.user {
                row.remove(3);
            }
            TableRow::new(row)
        })
        .collect();
    let table = Table::new(rows, cols.widths()).row_highlight_style(SELECTED);
    let mut state = TableState::new().with_selected(Some(selected - offset));
    frame.render_stateful_widget(table, inner, &mut state);
}

/// The lines of `conv` that fit a `width` × `height` view, `scroll` lines
/// up from the newest, and that scroll clamped to the conversation.
/// Messages are walked from the newest back, only as far as the view needs.
fn conversation_lines(
    conv: &super::chat::Conversation,
    me: &str,
    width: usize,
    height: usize,
    scroll: usize,
) -> (Vec<Line<'static>>, usize) {
    let want = height.saturating_add(scroll);
    // Newest line first.
    let mut lines: Vec<Line> = Vec::new();
    for m in conv.messages.iter().rev() {
        let (who, style) = if m.from_me {
            (me, Style::new().dark_gray())
        } else {
            (conv.username.as_str(), Style::new().cyan())
        };
        let prefix = format!("{} {who}: ", super::chat::format_time(m.timestamp));
        // Continuation lines are indented under the text, at most half
        // the width; a wider prefix gets a line of its own.
        let indent = prefix.width().min(width / 2);
        let mut message: Vec<Line> = Vec::new();
        let mut parts = wrap(&m.text, width.saturating_sub(indent)).into_iter();
        if prefix.width() > indent {
            message.push(Line::styled(ellipsis(&prefix, width), style));
        } else if let Some(first) = parts.next() {
            message.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::raw(first),
            ]));
        }
        message.extend(parts.map(|part| Line::from(format!("{}{part}", " ".repeat(indent)))));
        lines.extend(message.into_iter().rev());
        if lines.len() >= want {
            break;
        }
    }
    let scroll = scroll.min(lines.len().saturating_sub(height));
    let shown = lines.into_iter().skip(scroll).take(height).rev().collect();
    (shown, scroll)
}

/// Hard-wraps `text` to `width` columns (wide characters count double).
fn wrap(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for ch in paragraph.chars() {
            let w = ch.width().unwrap_or(0);
            if used + w > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(ch);
            used += w;
        }
        lines.push(line);
    }
    lines
}

fn render_chat(frame: &mut Frame, app: &mut App, area: Rect) {
    let [list_area, conv_area] =
        Layout::horizontal([Constraint::Length(26), Constraint::Min(20)]).areas(area);
    let [messages_area, input_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(conv_area);
    let inner = Block::bordered().inner(messages_area);
    app.chats.page = inner.height.into();
    // Clamped here, where the conversation's length in lines is known.
    let (lines, lines_up) = match app.chats.selected() {
        Some(conv) => conversation_lines(
            conv,
            &app.username,
            inner.width.into(),
            inner.height.into(),
            app.chats.scroll,
        ),
        None => (Vec::new(), 0),
    };
    app.chats.scroll = lines_up;
    // Kept between frames, so the list only scrolls when the cursor
    // reaches its edge.
    let list_height = usize::from(list_area.height.saturating_sub(2));
    app.chats_offset = scroll(app.chats_offset, app.chats.selected, list_height);
    let chats = &app.chats;

    // Conversations.
    let list_block =
        Block::bordered()
            .title(" Conversations ")
            .border_style(if chats.input.is_none() {
                Style::new().cyan()
            } else {
                Style::new().dark_gray()
            });
    let list_inner = list_block.inner(list_area);
    frame.render_widget(list_block, list_area);
    let rows: Vec<TableRow> = chats
        .list
        .iter()
        .map(|c| {
            let name = Cell::from(c.username.clone());
            if c.unread > 0 {
                TableRow::new(vec![name.bold(), Cell::from(c.unread.to_string()).green()])
            } else {
                TableRow::new(vec![name, Cell::from("")])
            }
        })
        .collect();
    let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(4)])
        .row_highlight_style(SELECTED);
    let mut state = TableState::new()
        .with_offset(app.chats_offset)
        .with_selected((!chats.list.is_empty()).then_some(chats.selected));
    frame.render_stateful_widget(table, list_inner, &mut state);

    // The selected conversation and the input box.
    let title = match chats.selected() {
        Some(c) if lines_up > 0 => {
            format!(" {} · {lines_up} lines up, PgDn for newer ", c.username)
        }
        Some(c) => format!(" {} ", c.username),
        None => " Private messages ".to_owned(),
    };
    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().dark_gray());
    frame.render_widget(block, messages_area);

    match chats.selected() {
        None => {
            frame.render_widget(
                Paragraph::new(
                    "Private messages with other Soulseek users. Press a and type a username to \
                     start a conversation, or m on a search result, download, upload or buddy. \
                     Messages that arrive while you are offline are delivered when you log in. \
                     ? explains more.",
                )
                .dark_gray()
                .wrap(Wrap { trim: true }),
                inner,
            );
        }
        Some(_) => frame.render_widget(Paragraph::new(lines), inner),
    }

    let empty = TextInput::default();
    let (label, text) = match &chats.input {
        Some(ChatInput::Message(t)) => (" Message ", t),
        Some(ChatInput::NewUser(t)) => (" Write to user ", t),
        None => (" Enter or i to write ", &empty),
    };
    let block = Block::bordered()
        .title(label)
        .border_style(if chats.input.is_some() {
            Style::new().cyan()
        } else {
            Style::new().dark_gray()
        });
    render_input(frame, input_area, block, text, chats.input.is_some());
}

fn render_wishlist(frame: &mut Frame, app: &mut App, area: Rect) {
    let list_area = match &app.wishlist.adding {
        Some(input) => {
            let [input_area, list_area] =
                Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
            let block = Block::bordered()
                .title(" Add to wishlist ")
                .border_style(Style::new().cyan());
            render_input(frame, input_area, block, input, true);
            list_area
        }
        None => area,
    };

    let w = &mut app.wishlist;
    let now = std::time::Instant::now();
    let next = w.next_in(now).as_secs();
    let block = Block::bordered()
        .title(format!(
            " Wishlist · one query every {} min · next in {}:{:02} ",
            w.interval.as_secs() / 60,
            next / 60,
            next % 60
        ))
        .border_style(if w.adding.is_some() {
            Style::new().dark_gray()
        } else {
            Style::new().cyan()
        });
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);

    if w.items.is_empty() {
        frame.render_widget(
            Paragraph::new(
                "The wishlist keeps searching for you in the background: the server allows one \
                 search every 12 minutes, and crabseek runs your wishes in turn, telling you when \
                 new files turn up. Press a to add a wish, or w on a search's results. \
                 ? explains more.",
            )
            .wrap(Wrap { trim: true })
            .dark_gray(),
            inner,
        );
        return;
    }

    let rows: Vec<TableRow> = w
        .items
        .iter_mut()
        .map(|item| {
            let new = if item.new > 0 {
                Span::raw(format!("{} new", item.new)).green().bold()
            } else {
                Span::raw("")
            };
            let found = format!(
                "{} files from {} users",
                item.results.file_count(),
                item.results.users
            );
            let last = match item.last_run {
                None => "not run yet".to_owned(),
                Some(t) => {
                    let mins = now.duration_since(t).as_secs() / 60;
                    if mins == 0 {
                        "ran just now".to_owned()
                    } else {
                        format!("ran {mins} min ago")
                    }
                }
            };
            TableRow::new(vec![
                Cell::from(item.query.clone()).bold(),
                Cell::from(new),
                Cell::from(found).dark_gray(),
                Cell::from(last).dark_gray(),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(10),
            Constraint::Length(26),
            Constraint::Length(16),
        ],
    )
    .row_highlight_style(if w.adding.is_some() {
        SELECTED_DIM
    } else {
        SELECTED
    });
    // Kept between frames, so the list only scrolls when the cursor
    // reaches its edge.
    app.wishlist_offset = scroll(app.wishlist_offset, w.selected, inner.height.into());
    let mut state = TableState::new()
        .with_offset(app.wishlist_offset)
        .with_selected(Some(w.selected));
    frame.render_stateful_widget(table, inner, &mut state);
}

fn render_buddies(frame: &mut Frame, app: &mut App, area: Rect) {
    let list_area = match &app.buddies.adding {
        Some(input) => {
            let [input_area, list_area] =
                Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
            let block = Block::bordered()
                .title(" Add buddy ")
                .border_style(Style::new().cyan());
            render_input(frame, input_area, block, input, true);
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
            Paragraph::new(
                "Buddies are users you want to keep an eye on: whether they are online, how \
                 much they share. Press a to add one, or A on a search result, download or \
                 upload. ? explains more.",
            )
            .wrap(Wrap { trim: true })
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

/// Keys for the current tab and focus, the global ones first, so a
/// narrow terminal cuts the least important.
fn help_line(app: &App) -> &'static str {
    match (app.tab, app.focus) {
        (Tab::Search, Focus::Input) => {
            " Ctrl-c quit · Enter search · ↑/↓ earlier searches · Esc results · Ctrl-Backspace delete word"
        }
        (Tab::Search, Focus::List) => {
            " q quit · 1-8 tabs · j/k move · Enter open/download · d download · w wishlist · b browse user · A add buddy · f/F format filter · s search"
        }
        (Tab::Transfers, _) => {
            " q quit · 1-8 tabs · j/k move · c cancel · r retry / queue place · x remove · X clear finished · Enter open/close · f group/flat · A add buddy · s search"
        }
        (Tab::Browse, _) if app.browse_focus == Focus::Input => {
            " Ctrl-c quit · Enter browse user · Esc list · Ctrl-u clear"
        }
        (Tab::Browse, _) => {
            " q quit · 1-8 tabs · j/k move · Enter open/download · d download · b other user · A add buddy · f/F format filter"
        }
        (Tab::Uploads, _) => {
            " q quit · 1-8 tabs · j/k move · c cancel · x remove · X clear finished · A add buddy · s search"
        }
        (Tab::Settings, _) if app.settings.is_editing() => {
            " Tab complete folder · Enter save · Esc cancel · Ctrl-u clear"
        }
        (Tab::Settings, _) => {
            " q quit · 1-8 tabs · j/k move · Enter edit/toggle · a add shared folder · x remove · s search"
        }
        (Tab::Buddies, _) if app.buddies.adding.is_some() => {
            " Ctrl-c quit · Enter add buddy · Esc cancel · Ctrl-u clear"
        }
        (Tab::Buddies, _) => {
            " q quit · 1-8 tabs · j/k move · a add buddy · x remove · Enter/b browse shares · s search"
        }
        (Tab::Chat, _) if matches!(app.chats.input, Some(ChatInput::NewUser(_))) => {
            " Ctrl-c quit · Enter open conversation · Esc cancel · Ctrl-u clear"
        }
        (Tab::Chat, _) if app.chats.input.is_some() => {
            " Ctrl-c quit · Enter send · Esc stop typing · Ctrl-u clear"
        }
        (Tab::Chat, _) => {
            " q quit · 1-8 tabs · j/k conversation · PgUp/PgDn scroll · Enter/i write · a new conversation · b browse · x delete · m on other tabs"
        }
        (Tab::Wishlist, _) if app.wishlist.adding.is_some() => {
            " Ctrl-c quit · Enter add to wishlist · Esc cancel · Ctrl-u clear"
        }
        (Tab::Wishlist, _) => {
            " q quit · 1-8 tabs · j/k move · Enter open results · a add · r run now · x remove · w on a search adds it"
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
                    marker.width() + edit.text.before_cursor().width(),
                ));
                lines.push(Line::from(vec![
                    Span::raw(marker),
                    Span::raw(edit.text.text().to_owned()).reversed(),
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
    lines.push(Line::from("Desktop notifications").bold());
    push_item(
        &mut lines,
        3,
        Item::Notifications,
        format!(
            "[{}] notify about finished downloads, private messages and new wishlist results",
            if s.notifications { "x" } else { " " }
        ),
    );
    lines.push(Line::default());
    lines.push(Line::from("Background mode").bold());
    push_item(
        &mut lines,
        4,
        Item::Background,
        format!(
            "[{}] keep running in the background after q (Q or `crabseek stop` quits)",
            if s.background { "x" } else { " " }
        ),
    );
    lines.push(
        Line::from(if app.background {
            "  running in the background now: q detaches this terminal"
        } else {
            "  off: q quits crabseek (sharing and downloads stop until the next start)"
        })
        .dark_gray(),
    );
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
        push_item(&mut lines, i + 5, Item::Shared(i), display_path(dir));
    }
    push_item(
        &mut lines,
        s.shared.len() + 5,
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

/// A one-line bordered text box. With `cursor`, the cursor sits after the
/// text; text too wide for the box scrolls so its end stays in view.
fn render_input(frame: &mut Frame, area: Rect, block: Block, input: &TextInput, cursor: bool) {
    let inner = block.inner(area);
    let (shown, x) = input_view(input, usize::from(inner.width));
    frame.render_widget(Paragraph::new(shown).block(block), area);
    if cursor {
        frame.set_cursor_position(Position::new(inner.x + x as u16, inner.y));
    }
}

/// The part of `input` that fits in `width` columns and the cursor's
/// column in it. The view scrolls only as far as needed to keep the cursor
/// inside, which needs a column of its own at the end of the text.
fn input_view(input: &TextInput, width: usize) -> (String, usize) {
    use unicode_width::UnicodeWidthChar;
    let col = input.before_cursor().width();
    let max_scroll = (input.text().width() + 1).saturating_sub(width);
    let mut scroll = input.scroll.get().min(max_scroll);
    if col < scroll {
        scroll = col;
    } else if col + 1 > scroll + width {
        scroll = (col + 1).saturating_sub(width);
    }
    input.scroll.set(scroll);

    let mut shown = String::new();
    let mut at = 0;
    for c in input.text().chars() {
        let w = c.width().unwrap_or(0);
        if at + w > scroll + width {
            break;
        }
        if at >= scroll {
            shown.push(c);
        } else if at + w > scroll {
            // A wide character cut by the left edge.
            shown.extend(std::iter::repeat_n(' ', at + w - scroll));
        }
        at += w;
    }
    (shown, col - scroll)
}

/// Cuts `s` to `width` display columns, ending in `…` when it had to cut.
fn ellipsis(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if s.width() <= width {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// Like [`ellipsis`], but keeps the end: for paths, where the file name
/// matters most.
fn ellipsis_start(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if s.width() <= width {
        return s.to_owned();
    }
    let mut tail = Vec::new();
    let mut used = 0;
    for c in s.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        tail.push(c);
        used += w;
    }
    let mut out = String::from(if width > 0 { "…" } else { "" });
    out.extend(tail.into_iter().rev());
    out
}

/// A full bar, dim: finished rows stay quiet next to active ones.
fn finished_bar(width: usize) -> Line<'static> {
    Line::from(format!("{} 100%", "━".repeat(width))).dark_gray()
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
    fn wraps_by_display_width() {
        assert_eq!(wrap("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(wrap("a\nb", 4), ["a", "b"]);
        // Wide characters take two columns.
        assert_eq!(wrap("日本語", 4), ["日本", "語"]);
        assert_eq!(wrap("", 4), [""]);
    }

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

    #[test]
    fn chat_lines_never_overflow_a_narrow_view() {
        let conv = super::super::chat::Conversation {
            username: "a_rather_long_soulseek_username".into(),
            messages: vec![super::super::chat::ChatMessage {
                timestamp: 0,
                from_me: false,
                text: "szia! megvan még a Geogaddi FLAC-ben? 日本語".into(),
            }],
            unread: 0,
        };
        let (lines, _) = conversation_lines(&conv, "me", 24, 10, 0);
        for line in &lines {
            assert!(line.width() <= 24, "{line:?} is {} wide", line.width());
        }
        // The prefix took a line of its own; the text follows in full.
        let text: String = lines[1..]
            .iter()
            .map(|l| l.to_string().trim().to_owned())
            .collect();
        assert!(text.starts_with("szia!"), "{text}");
        assert!(text.ends_with("日本語"), "{text}");
    }

    #[test]
    fn ellipsis_cuts_by_display_width() {
        assert_eq!(ellipsis("abc", 3), "abc");
        assert_eq!(ellipsis("abcdef", 4), "abc…");
        // A wide character that would straddle the edge is dropped.
        assert_eq!(ellipsis("日本語", 4), "日…");
        assert_eq!(ellipsis("abc", 0), "");
        assert_eq!(ellipsis_start("a\\b\\track.flac", 8), "…ck.flac");
        assert_eq!(ellipsis_start("日本語", 4), "…語");
    }

    #[test]
    fn narrow_transfer_lists_drop_the_user_then_shrink_the_bar() {
        let wide = TransferColumns::fit(108);
        assert_eq!((wide.bar, wide.user), (20, true));
        let mid = TransferColumns::fit(90);
        assert_eq!((mid.bar, mid.user), (20, false));
        // 80 columns minus the borders.
        let at_80 = TransferColumns::fit(78);
        assert_eq!((at_80.bar, at_80.user), (10, false));
        assert!(at_80.name >= usize::from(NAME_MIN), "{at_80:?}");
    }

    #[test]
    fn narrow_result_lists_drop_columns_before_the_name() {
        let wide = ResultColumns::fit(108, true);
        assert!(wide.user && wide.availability);
        assert_eq!(wide.name, 108 - 73);
        // 80 columns minus the borders.
        let at_80 = ResultColumns::fit(78, true);
        assert!(!at_80.user && !at_80.availability);
        assert!(at_80.name >= NAME_MIN, "{at_80:?}");
        let at_100 = ResultColumns::fit(98, true);
        assert!(at_100.user && !at_100.availability);
        assert!(at_100.name >= NAME_MIN, "{at_100:?}");
        // A browse listing has no user columns to drop.
        assert_eq!(ResultColumns::fit(78, false).name, 78 - 33);
    }
}
