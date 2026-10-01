//! The `?` help window: what the current tab is for, how it works, and its
//! keys, followed by the keys that work everywhere.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::app::Tab;

struct Page {
    title: &'static str,
    /// What the tab is for and how it behaves, one paragraph per entry.
    about: &'static [&'static str],
    keys: &'static [(&'static str, &'static str)],
}

fn page(tab: Tab) -> Page {
    match tab {
        Tab::Search => Page {
            title: "Search",
            about: &[
                "Searches the whole Soulseek network. Results stream in for as long as other \
                 users answer, grouped into one row per folder. Users with a free upload slot \
                 and a fast connection come first.",
                "The quality column shows kbps for every file (~ means estimated from size and \
                 length), plus sample rate and bit depth for lossless files.",
            ],
            keys: &[
                (
                    "s or /",
                    "type a search (Enter runs it, Esc back to the list)",
                ),
                ("Enter / Space", "open or close a folder"),
                ("d", "download the file, or the whole folder"),
                (
                    "f / F",
                    "next / previous format filter: FLAC, lossless, MP3 320, MP3, M4A",
                ),
                ("b", "browse everything this user shares"),
                (
                    "w",
                    "put this search on the wishlist (keeps searching in the background)",
                ),
                ("A", "add this user to your buddies"),
            ],
        },
        Tab::Transfers => Page {
            title: "Downloads",
            about: &[
                "Everything you queued for download. Uploaders send files when a slot frees \
                 up, so 'queued #n' is your place in their queue.",
                "Unfinished downloads survive a restart and continue where they stopped; the \
                 partial data waits in .part files.",
            ],
            keys: &[
                ("c", "cancel the selected download"),
                ("r", "retry a failed download"),
                ("x", "clear finished downloads from the list"),
                ("A", "add the uploader to your buddies"),
            ],
        },
        Tab::Uploads => Page {
            title: "Uploads",
            about: &[
                "Files other users download from your shared folders. A few uploads run at \
                 once (upload_slots, default 2), at most one per user, so nobody takes every \
                 slot; the rest wait in a queue.",
                "The title also counts searches your shares answered: that is how other users \
                 find your files.",
            ],
            keys: &[
                ("c", "cancel the selected upload"),
                ("x", "clear finished uploads from the list"),
            ],
        },
        Tab::Settings => Page {
            title: "Settings",
            about: &[
                "Changes apply and are saved at once (~/.config/seekr/config.toml).",
                "Listen port: other users connect to you on it. With UPnP on, seekr opens it \
                 on your router by itself; otherwise forward it by hand.",
                "Shared folders: the music others can search, browse and download from you. \
                 Soulseek expects everyone to share.",
                "Distributed network: seekr joins the network-wide search tree, so searches \
                 reach your shares.",
            ],
            keys: &[
                ("Enter", "edit a folder or the port, or switch UPnP on/off"),
                ("Tab (while editing)", "complete a folder path"),
                ("a / x", "add / remove a shared folder"),
            ],
        },
        Tab::Browse => Page {
            title: "Browse",
            about: &[
                "Shows everything one user shares, as the same folder tree as search results, \
                 with the same filter and download keys. Big shares take a few seconds to \
                 arrive.",
            ],
            keys: &[
                ("/", "type a username (Enter loads it)"),
                ("b on a search result", "browse that result's user"),
                ("Enter / d / f", "open a folder / download / format filter"),
            ],
        },
        Tab::Buddies => Page {
            title: "Buddies",
            about: &[
                "Users you want to keep an eye on: their status (online, away, offline), how \
                 much they share and their speed. The list is saved between runs.",
            ],
            keys: &[
                ("a", "type a username to add"),
                ("A (on other tabs)", "add the user of the selected row"),
                ("Enter / b", "browse the buddy's shares"),
                ("x", "remove the buddy"),
            ],
        },
        Tab::Chat => Page {
            title: "Chat",
            about: &[
                "Private messages with other Soulseek users, one conversation per user, the \
                 newest on top. New messages show in the status line and as a count on the tab.",
                "Messages sent to you while you are offline wait on the server and arrive when \
                 you log in. The last 500 messages per user are kept between runs \
                 (chats.json, readable only by you).",
            ],
            keys: &[
                (
                    "Enter or i",
                    "write to the selected user (Enter sends, Esc stops)",
                ),
                ("a", "start a conversation: type a username"),
                ("m (on other tabs)", "write to the user of the selected row"),
                ("b", "browse the user's shares"),
                ("x", "delete the conversation"),
            ],
        },
        Tab::Wishlist => Page {
            title: "Wishlist",
            about: &[
                "Searches that keep running in the background, for things that are hard to \
                 find. The server allows one wishlist search every 12 minutes, so seekr runs \
                 your wishes one after the other, in turn.",
                "Results collect per wish. Files you have not seen before count as new: the \
                 tab title shows how many, and the status line tells you when some arrive.",
                "The wishes are saved; the collected results last until you quit.",
            ],
            keys: &[
                ("w (on search results)", "add the current search"),
                ("a", "type a new wish"),
                (
                    "Enter",
                    "open everything found for the wish on the Search tab",
                ),
                (
                    "r",
                    "run the wish now (the next scheduled run waits a full interval)",
                ),
                ("x", "remove the wish"),
            ],
        },
    }
}

const GENERAL: &[(&str, &str)] = &[
    ("Tab / Shift-Tab, Alt-1…8, F1…F8", "switch tabs"),
    ("m", "write a private message to the selected row's user"),
    ("j / k, ↓ / ↑, PgUp / PgDn, g / G", "move"),
    ("10j, 10k, 5G", "vim counts: move 10 rows, jump to row 5"),
    ("Alt-s", "jump to the search box from anywhere"),
    ("?", "this help (Esc or ? closes it)"),
    ("q / Ctrl-c", "quit (q asks again while transfers run)"),
];

fn key_lines(keys: &[(&str, &str)]) -> Vec<Line<'static>> {
    let width = keys
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    keys.iter()
        .map(|(k, what)| {
            Line::from(vec![
                Span::raw(format!("  {k:<width$}  ")).cyan(),
                Span::raw((*what).to_owned()),
            ])
        })
        .collect()
}

pub fn render(frame: &mut Frame, tab: Tab) {
    let page = page(tab);
    let mut lines: Vec<Line> = Vec::new();
    for paragraph in page.about {
        lines.push(Line::from(*paragraph));
        lines.push(Line::default());
    }
    lines.push(Line::from("Keys on this tab").bold());
    lines.extend(key_lines(page.keys));
    lines.push(Line::default());
    lines.push(Line::from("Everywhere").bold());
    lines.extend(key_lines(GENERAL));

    let area = centered(frame.area(), 86, 30);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(format!(" Help – {} ", page.title).bold())
                .title_bottom(Line::from(" Esc or ? closes ").dark_gray())
                .border_style(Style::new().cyan())
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        area,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
}
