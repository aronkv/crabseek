//! Rendering. Only the visible rows of each list are built, so large
//! result sets stay cheap to draw.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row as TableRow, Table, TableState, Tabs};
use seekr_net::DownloadState;

use super::app::{App, Focus, Tab};
use super::results::{FormatFilter, Row};
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
        Tab::Settings => render_settings(frame, app, body),
    }
    frame.render_widget(
        Paragraph::new(app.status.as_str()).fg(Color::Yellow),
        status,
    );
    frame.render_widget(Paragraph::new(help_line(app)).dark_gray(), help);
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let active = app.transfers.active();
    let titles = vec![
        Line::from(" 1 Search "),
        Line::from(if active > 0 {
            format!(" 2 Transfers ({active}) ")
        } else {
            " 2 Transfers ".to_owned()
        }),
        Line::from(" 3 Settings "),
    ];
    let selected = match app.tab {
        Tab::Search => 0,
        Tab::Transfers => 1,
        Tab::Settings => 2,
    };
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(Style::new().bold().reversed())
            .divider(""),
        area,
    );

    let speed = app.transfers.total_speed();
    let mut right = vec![];
    if speed > 0.0 {
        right.push(Span::raw(format!("↓ {}/s  ", human_size(speed as u64))));
    }
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
    let block = Block::bordered().title(title).border_style(if editing {
        Style::new().dark_gray()
    } else {
        Style::new().cyan()
    });
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);

    if app.results.rows().is_empty() {
        let msg = if !app.results.is_empty() {
            "no results in this format – press f to change the filter"
        } else if app.search.is_some() {
            "waiting for results..."
        } else {
            "type a query and press Enter"
        };
        frame.render_widget(Paragraph::new(msg).dark_gray(), inner);
        return;
    }

    let height = inner.height as usize;
    app.page_size = height;
    let selected = app.results.selected_index().unwrap_or(0);
    app.results_offset = scroll(app.results_offset, selected, height);
    let offset = app.results_offset;

    let visible: Vec<Row> = app
        .results
        .rows()
        .iter()
        .skip(offset)
        .take(height)
        .copied()
        .collect();
    let rows: Vec<TableRow> = visible
        .into_iter()
        .map(|row| result_row(app, row))
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(14),
            Constraint::Length(9),
            Constraint::Length(18),
            Constraint::Length(20),
        ],
    )
    .row_highlight_style(if editing { Style::new() } else { SELECTED });
    let mut state = TableState::new().with_selected(Some(selected - offset));
    frame.render_stateful_widget(table, inner, &mut state);
}

fn result_row(app: &App, row: Row) -> TableRow<'static> {
    match row {
        Row::Folder(id) => {
            let filter = app.results.filter();
            let f = app.results.folder(id);
            let marker = if app.results.is_expanded(id) {
                "▾"
            } else {
                "▸"
            };
            let name = last_components(&f.path, 2);
            let availability = if f.slot_free {
                Span::raw(format!("free  {}/s", human_size(f.avg_speed.into()))).green()
            } else {
                Span::raw(format!("queue {}", f.queue_length)).yellow()
            };
            TableRow::new(vec![
                Cell::from(format!(
                    "{marker} {name}  ({})",
                    f.visible_files(filter).len()
                ))
                .bold(),
                Cell::from(f.quality(filter)),
                Cell::from(human_size(f.total_size(filter))),
                Cell::from(f.username.clone()).cyan(),
                Cell::from(availability),
            ])
        }
        Row::File(id, i) => {
            let file = &app.results.folder(id).files[i];
            TableRow::new(vec![
                Cell::from(format!("    {}", file.basename())),
                Cell::from(quality(file)).dark_gray(),
                Cell::from(human_size(file.size)),
                Cell::from(""),
                Cell::from(""),
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
                        if t.speed > 0.0 {
                            format!("{}/s", human_size(t.speed as u64))
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

fn help_line(app: &App) -> &'static str {
    match (app.tab, app.focus) {
        (Tab::Search, Focus::Input) => " Enter search · Esc results · Ctrl-u clear · Ctrl-c quit",
        (Tab::Search, Focus::List) => {
            " j/k move · Enter open folder · h/l collapse/expand · d download · f/F format filter · / search · Tab transfers · q quit"
        }
        (Tab::Transfers, _) => {
            " j/k move · c cancel · r retry failed · x clear finished · / search · Tab settings · q quit"
        }
        (Tab::Settings, _) if app.settings.is_editing() => {
            " Tab complete folder · Enter save · Esc cancel · Ctrl-u clear"
        }
        (Tab::Settings, _) => {
            " j/k move · Enter edit · a add shared folder · x remove · / search · Tab search · q quit"
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
    lines.push(Line::from("Shared folders").bold());
    lines.push(
        Line::from("  Music you offer to other users (sharing starts in a coming version).")
            .dark_gray(),
    );
    for (i, dir) in s.shared.iter().enumerate() {
        push_item(&mut lines, i + 1, Item::Shared(i), display_path(dir));
    }
    push_item(
        &mut lines,
        s.shared.len() + 1,
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
