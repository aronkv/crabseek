<div align="center">

# seekr

**A fast, keyboard-driven [Soulseek](https://www.slsknet.org/) client for the terminal, written in Rust.**

[![CI](https://github.com/DarkAaronfox/seekr/actions/workflows/ci.yml/badge.svg)](https://github.com/DarkAaronfox/seekr/actions/workflows/ci.yml)
![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)

</div>

```
  1 Search  2 Downloads (3)  3 Uploads  4 Settings   ↓ 8.8 MB/s  sharing 4210 files  me ● online
┌ Search ────────────────────────────────────────────────────────────────────────────────────┐
│boards of canada                                                                            │
└────────────────────────────────────────────────────────────────────────────────────────────┘
┌ "boards of canada": 659 users, 49925 files (10s) · [FLAC] 412 folders ─────────────────────┐
│▾ Boards of Canada\Geogaddi  (24)     FLAC 16/44.1 ~903kbps   512.3 MB  alice free 13.3 MB/s│
│    01 - Ready Lets Go.flac           44.1kHz/16bit ~1006kbps   4.4 MB                      │
│    02 - Music Is Math.flac           44.1kHz/16bit ~836kbps   33.6 MB                      │
│▸ Tomorrow's Harvest  (17)            FLAC 24/44.1 ~1914kbps  643.5 MB  bob   free 12.1 MB/s│
│▸ Music Has the Right to Children (19) MP3 320kbps            151.2 MB  carol  queue 2      │
└────────────────────────────────────────────────────────────────────────────────────────────┘
 10j/10k jump · Enter open folder · d download · w wishlist · f/F filter · Tab/Alt-1…7 tabs
```

seekr speaks the Soulseek protocol natively. There is no daemon, web UI or Python runtime:
you type `seekr`, search, and download whole albums in a few keystrokes.

## Features

- **Live search:** results stream in and are grouped by folder. Users with a free
  upload slot and fast connections come first, and the cursor stays on its row while
  new results arrive.
- **Format filter:** cycle between all formats, FLAC, lossless, MP3 320, MP3 and
  M4A/AAC with `f`. Folder downloads take only the matching audio files, plus the
  cover art and lyrics next to them.
- **Browse users:** `b` on any search result opens that user's whole share as the
  same folder tree, with the format filter and one-key downloads.
- **Buddies:** keep a list of users and see live whether they are online, away or
  offline, with their share size and speed. `A` on any search result, download or
  upload adds that user; `Enter` on a buddy browses their shares.
- **Wishlist:** saved searches keep running in the background, one every 12
  minutes as the server allows, and the Wishlist tab counts files you have not seen
  yet. `w` on a search's results adds it; `Enter` on a wish opens everything found.
- **One-key downloads:** `d` on a file or a whole folder. Interrupted downloads resume
  from where they stopped (`.part` files), and name clashes never overwrite anything.
- **Transfer view:** live progress, speed, queue position and failure reasons, with
  retry and cancel. The download list survives restarts, and unfinished downloads
  continue where they stopped.
- **Sharing:** other users find your music folders (default `~/Music`) through
  network-wide searches, and can browse and download from them. seekr joins the
  distributed search network on its own. Audio properties are read once and cached,
  and uploads are spread fairly over a configurable number of slots.
- **Quality at a glance:** kbps for every file and folder (estimated from size and
  length, marked `~`, when the peer does not send it), plus sample rate and bit depth
  for lossless files, duration and size.
- **Vim-style navigation:** `j`/`k`, counts like `10k` or `5G`, `g`/`G`.
- **Automatic port forwarding:** UPnP opens the listen port on the router, with a
  warning when another NAT sits in front of it.
- **Solid networking:** direct and firewall-piercing (indirect) peer connections are
  raced against each other, so peers behind NAT still work. Peers behind your own
  router are reached locally.
- **Scriptable CLI:** `seekr search`, `seekr download` and friends for quick checks
  and automation.

## Installation

### From source

You need a Rust toolchain (1.88 or newer). On Arch/CachyOS: `sudo pacman -S rustup && rustup default stable`.

```sh
git clone https://github.com/DarkAaronfox/seekr.git
cd seekr
scripts/install.sh          # builds and installs to ~/.local/bin/seekr
```

Then run `seekr`. If your shell cannot find it, add `~/.local/bin` to your `PATH`; the
script prints how. To install somewhere else, use `PREFIX=/usr/local sudo -E scripts/install.sh`.
Alternatively: `cargo install --git https://github.com/DarkAaronfox/seekr seekr`.

### Uninstall

```sh
cd seekr
scripts/uninstall.sh        # asks once, then removes everything
scripts/uninstall.sh -y     # same, without the question
```

This removes seekr completely:

| Removed | Path |
|---|---|
| the program | `~/.local/bin/seekr` (or `$PREFIX/bin/seekr`) |
| login and settings | `~/.config/seekr/` |
| download list | `~/.local/share/seekr/` |
| share cache | `~/.cache/seekr/` |
| logs | `~/.local/state/seekr/` |

Your downloaded music is **never** touched. The saved Soulseek password is deleted
too, and Soulseek has no password reset, so keep a note of it if you want to use the
account again. If you installed with `cargo install`, use `cargo uninstall seekr` for
the binary and the script for the rest.

### AUR

An AUR package is planned once seekr is stable. The draft lives in
[`packaging/aur/PKGBUILD`](packaging/aur/PKGBUILD).

## First run

Start `seekr`. There is nothing to set up by hand: the first time, it asks for a
Soulseek username and password, and it creates the config file itself once the server
accepts them. On later starts it logs in straight away. The login screen only comes
back if the saved password stops working.

- **Already have an account?** Log in with it.
- **New to Soulseek?** Pick any free name. The server creates the account the first
  time you log in with it, so double-check the spelling.

Your credentials are saved only after the server accepts them. See
[Security](#security) for where and how. To forget them, run `seekr logout`.

### Let peers reach you

Soulseek is peer-to-peer. Downloads and sharing work best when other users can connect
to you on your listen port (TCP **2234** by default):

- **Router:** seekr opens the port by itself with **UPnP** when the router supports
  it, and renews it every 30 minutes. The Settings tab shows the result; check it any
  time with `seekr portmap`. Without UPnP, forward TCP 2234 to your computer by hand.
- **Double NAT:** if Settings says the router "sits behind another NAT", your ISP's
  modem is in front of it. That device needs a forward to your router, or has to run
  in bridge mode.
- **Local firewall:** allow the port, e.g. `sudo ufw allow 2234/tcp`.

Without this, you can still download from peers that are reachable themselves.

## Usage

| Where | Key | Action |
|---|---|---|
| everywhere | `s`, `/` | open the search box (seekr starts in the results list) |
| | `?` | explain the current tab: what it is for, how it works, its keys |
| | `Alt-s` | toggle between the search box and the results, also while typing |
| | `Tab`, `Alt-1`–`7`, `F1`–`F7` | switch between Search, Downloads, Uploads, Settings, Browse, Buddies and Wishlist |
| | `A` | add the user of the selected result, download or upload as a buddy |
| | `q`, `Ctrl-c` | quit (`q` asks again while transfers are running) |
| lists | `10j`, `10k`, `10↑` … | vim-style counts: move 10 rows (the count shows bottom left) |
| | `5G` | jump to row 5 |
| search box | `Enter` | search |
| | `Esc`, `Alt-s` | back to the results |
| | `Ctrl-u` | clear |
| results | `j`/`k`, `↓`/`↑`, `PgUp`/`PgDn`, `g`/`G` | move |
| | `Enter`, `Space` | open or close a folder |
| | `l`/`h` | expand or collapse |
| | `d` | download the file or the whole folder |
| | `b` | browse all shares of that result's user |
| | `f` / `F` | next or previous format filter |
| browse | `/` | type a username (`Enter` loads it); the tab starts on the list |
| | same keys as results | open folders, filter, `d` download |
| downloads | `c` | cancel |
| | `r` | retry a failed download |
| | `x` | clear finished downloads |
| uploads | `c` | cancel |
| | `x` | clear finished uploads |
| settings | `Enter` | edit the selected folder or port (`Tab` completes paths), toggle UPnP |
| | `a` / `x` | add or remove a shared folder |
| buddies | `a` | type a username to add (`Enter` adds it) |
| | `x` | remove the selected buddy |
| | `Enter`, `b` | browse the buddy's shares |
| wishlist | `a` | type a query to add (`Enter` adds it); `w` on search results does the same |
| | `Enter` | open everything found for the wish on the Search tab |
| | `r` | run the wish now (the next scheduled one waits a full interval) |
| | `x` | remove the wish |

### Command line

```sh
seekr                                   # the TUI
seekr search "artist album" --full-paths [--wishlist]
seekr download <user> '<remote\path\to\file.flac>'
seekr userinfo <user>                   # test a peer connection
seekr browse <user>                     # list a user's shared folders
seekr portmap                           # test automatic port forwarding (UPnP)
seekr shares ["query"] [--dir PATH]     # what you share, and what a search would find
seekr logout                            # forget saved credentials
seekr config-path                       # where the config lives
```

## Configuration

The download folder, the listen port and the shared folders can be changed in the
**Settings** tab (`4`); changes apply and are saved immediately. Everything lives in
`~/.config/seekr/config.toml`, and every key except the credentials is optional:

```toml
username = "..."                        # written by the login screen
password = "..."
download_dir = "~/Downloads/seekr"      # default
shared_dirs = ["~/Music"]               # default
upload_slots = 2                        # default
listen_port = 2234                      # default
upnp = true                             # default: open the port on the router
server = "server.slsknet.org:2242"      # default
```

| File | Contents |
|---|---|
| `~/.config/seekr/config.toml` | login and settings |
| `~/.local/share/seekr/downloads.json` | the download list |
| `~/.local/share/seekr/buddies.json` | the buddy list |
| `~/.local/share/seekr/wishlist.json` | wishlist queries |
| `~/.cache/seekr/shares.json` | cached audio properties of shared files |
| `~/.local/state/seekr/seekr.log` | log of the last TUI session (`RUST_LOG=debug` for more) |

## Security

- The Soulseek login needs the plain password, so clients have to keep it. Nicotine+
  and slskd keep it unencrypted in their config files, and seekr does the same, with
  tighter permissions: `~/.config/seekr/config.toml` is written with mode `600`,
  inside a `700` directory, so only your user can read it. The file is replaced
  atomically.
- Encrypting the file would not add real protection, because the key would have to sit
  on the same disk. System keyring support (Secret Service) may come as an option.
- Credentials never appear in logs, and the config lives outside the source tree, so
  it cannot end up in a commit of this repository.
- If you keep `~/.config` in a dotfiles repository, make sure `seekr/` is not
  committed.

## Roadmap

- [x] Login, peer connections (direct and indirect), search, downloads with resume
- [x] TUI with folder view, format filter and transfer list
- [x] Sharing your music library and serving uploads
- [x] Distributed search network (as a child node; relaying searches to children is next)
- [x] Browsing a user's shares
- [x] Wishlist
- [ ] Private messages
- [x] Automatic port mapping (UPnP)
- [ ] AUR package

## Development

```sh
cargo test                                   # unit and loopback-network tests
cargo clippy --all-targets -- -D warnings
scripts/fetch-protocol-doc.sh                # local copy of the protocol docs
```

The workspace has three crates:

| Crate | Purpose |
|---|---|
| `crates/proto` | Soulseek message encoding and decoding, without I/O. Byte-for-byte tests against the spec. |
| `crates/net` | tokio networking: server session, peer connections, searches, transfers, and an actor that ties them together. |
| `crates/seekr` | The binary: ratatui TUI and CLI. |

## Acknowledgements

- The [Nicotine+ protocol documentation](https://nicotine-plus.org/doc/SLSKPROTOCOL.html),
  the reference used to implement the protocol.
- [soulseek-rs](https://github.com/michel/soulseek-rs), another Rust client, consulted
  for comparison. seekr's code is written from scratch.

## License

[MIT](LICENSE). Please share back what you download, and respect copyright where you live.
