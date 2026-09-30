<div align="center">

# seekr

**A fast, keyboard-driven [Soulseek](https://www.slsknet.org/) client for the terminal, written in Rust.**

[![CI](https://github.com/DarkAaronfox/seekr/actions/workflows/ci.yml/badge.svg)](https://github.com/DarkAaronfox/seekr/actions/workflows/ci.yml)
![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)

</div>

```
  1 Search    2 Transfers (3)                                     ↓ 8.8 MB/s  me ● online
┌ Search ───────────────────────────────────────────────────────────────────────────────┐
│boards of canada                                                                       │
└───────────────────────────────────────────────────────────────────────────────────────┘
┌ "boards of canada": 659 users, 49925 files (10s) · [FLAC] 412 folders ────────────────┐
│▾ Boards of Canada\Geogaddi  (24)      FLAC 16/44.1   512.3 MB  alice    free 13.3 MB/s│
│    01 - Ready Lets Go.flac            44.1kHz/16bit    4.4 MB                         │
│    02 - Music Is Math.flac            44.1kHz/16bit   33.6 MB                         │
│▸ Boards of Canada\Tomorrow's Harvest  FLAC 24/44.1   643.5 MB  bob      free 12.1 MB/s│
│▸ Music Has the Right to Children (19) FLAC 16/44.1   367.1 MB  carol    queue 2       │
└───────────────────────────────────────────────────────────────────────────────────────┘
 j/k move · Enter open folder · d download · f/F format filter · / search · Tab transfers
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
- **One-key downloads:** `d` on a file or a whole folder. Interrupted downloads resume
  from where they stopped (`.part` files), and name clashes never overwrite anything.
- **Transfer view:** live progress, speed, queue position and failure reasons, with
  retry and cancel. The download list survives restarts, and unfinished downloads
  continue where they stopped.
- **Sharing:** your music folders (default `~/Music`) can be searched, browsed and
  downloaded by other users. Audio properties are read once and cached, and uploads
  are spread fairly over a configurable number of slots.
- **Quality at a glance:** bitrate, sample rate and bit depth, duration and size for
  every file and folder.
- **Solid networking:** direct and firewall-piercing (indirect) peer connections are
  raced against each other, so peers behind NAT still work.
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
To remove seekr, run `scripts/uninstall.sh`. It deletes the binary, your login and
settings, the download list, the cache and the logs after one confirmation (`-y`
skips it). Downloaded music is never touched.

Alternatively: `cargo install --git https://github.com/DarkAaronfox/seekr seekr`.

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

Soulseek is peer-to-peer. Downloads work best when other users can connect to you on
your listen port (TCP **2234** by default):

- **Router:** forward TCP 2234 to your computer.
- **Local firewall:** allow the port, e.g. `sudo ufw allow 2234/tcp`.

Without this, you can still download from peers that are reachable themselves.

## Usage

| Where | Key | Action |
|---|---|---|
| everywhere | `/` | focus the search box |
| | `Tab`, `Alt-1`–`4`, `F1`–`F4` | switch between Search, Downloads, Uploads and Settings |
| lists | `10j`, `10k`, `10↑` … | vim-style counts: move 10 rows (the count shows bottom left) |
| | `5G` | jump to row 5 |
| | `q` | quit (asks again while downloads are running) |
| search box | `Enter` | search |
| | `Esc` | back to the results |
| | `Ctrl-u` | clear |
| results | `j`/`k`, `↓`/`↑`, `PgUp`/`PgDn`, `g`/`G` | move |
| | `Enter`, `Space` | open or close a folder |
| | `l`/`h` | expand or collapse |
| | `d` | download the file or the whole folder |
| | `f` / `F` | next or previous format filter |
| downloads | `c` | cancel |
| | `r` | retry a failed download |
| | `x` | clear finished downloads |
| uploads | `c` | cancel |
| | `x` | clear finished uploads |
| settings | `Enter` | edit the selected folder (`Tab` completes the path) |
| | `a` / `x` | add or remove a shared folder |

### Command line

```sh
seekr                                   # the TUI
seekr search "artist album" --full-paths
seekr download <user> '<remote\path\to\file.flac>'
seekr userinfo <user>                   # test a peer connection
seekr shares ["query"]                  # what you share, and what a search would find
seekr logout                            # forget saved credentials
seekr config-path                       # where the config lives
```

## Configuration

The download folder and the shared folders can be changed in the **Settings** tab
(`3`); changes are saved immediately. Everything lives in
`~/.config/seekr/config.toml`, and every key except the credentials is optional:

```toml
username = "..."                        # written by the login screen
password = "..."
download_dir = "~/Downloads/seekr"      # default
shared_dirs = ["~/Music"]               # default
upload_slots = 2                        # default
listen_port = 2234                      # default
server = "server.slsknet.org:2242"      # default
```

| File | Contents |
|---|---|
| `~/.config/seekr/config.toml` | login and settings |
| `~/.local/share/seekr/downloads.json` | the download list |
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
- [ ] Distributed search network
- [ ] Browsing a user's shares, private messages, wishlist
- [ ] Automatic port mapping (UPnP / NAT-PMP)
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
