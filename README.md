<div align="center">

# crabseek

<img src="showcase.gif" alt="crabseek searching and downloading an album" width="1000" />

**A fast, keyboard-driven [Soulseek](https://www.slsknet.org/) client for the terminal, written in Rust.**

[![CI](https://github.com/aronkv/crabseek/actions/workflows/ci.yml/badge.svg)](https://github.com/aronkv/crabseek/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/aronkv/crabseek/graph/badge.svg)](https://codecov.io/gh/aronkv/crabseek)
![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)

</div>

crabseek speaks the Soulseek protocol natively. There is no daemon, web UI or Python runtime:
you type `crabseek`, search, and download whole albums in a few keystrokes.

## Features

- **Live search:** results stream in, grouped by folder. Users with a free upload slot
  and fast connections come first, and the cursor stays put while new results arrive.
- **Format filter:** cycle through all formats, FLAC, lossless, MP3 320, MP3 and M4A/AAC
  with `f`. Folder downloads take only matching audio, plus cover art and lyrics.
- **One-key downloads:** `d` on a file or a whole folder. Interrupted downloads resume
  from `.part` files, the download list survives restarts, and name clashes never
  overwrite anything.
- **Transfer view:** live progress, speed, queue position and failure reasons, with
  retry and cancel.
- **Quality at a glance:** kbps for every file and folder (estimated and marked `~` when
  the peer does not send it), sample rate and bit depth for lossless files, duration
  and size.
- **Browse users:** `b` on any result opens that user's whole share as a folder tree.
- **Buddies:** see whether users are online, away or offline, with share size and speed.
- **Private messages:** chat on the Chat tab; offline messages arrive when you log in,
  and history is kept between runs.
- **Wishlist:** saved searches keep running in the background (one every 12 minutes, as
  the server allows) and the tab counts new files.
- **Sharing:** your music folders (default `~/Music`) are searchable and browsable by
  other users, with uploads spread fairly over a configurable number of slots.
  crabseek joins the distributed search network on its own.
- **Connectivity:** UPnP port forwarding, with a warning on double NAT. Direct and
  indirect peer connections are raced, so peers behind NAT still work.
- **Background mode and desktop notifications**, both optional and off by default.
- **Vim-style keys** everywhere, `?` for help on any tab, and a scriptable CLI.

## Installation

### Quick install (x86_64 Linux)

```sh
curl -fsSL https://raw.githubusercontent.com/aronkv/crabseek/main/scripts/get.sh | sh
```

This downloads the latest prebuilt release, checks its SHA-256 sum and installs it to
`~/.local/bin`. It needs glibc 2.35+ (current Arch, Fedora, Debian 12, Ubuntu 22.04 and
newer). You can read [`scripts/get.sh`](scripts/get.sh) first.

### From source

You need Rust 1.88 or newer (on Arch/CachyOS: `sudo pacman -S rustup && rustup default stable`).

```sh
git clone https://github.com/aronkv/crabseek.git
cd crabseek
scripts/install.sh          # builds and installs to ~/.local/bin/crabseek
```

If your shell cannot find `crabseek`, add `~/.local/bin` to your `PATH`; the script
prints how. To install elsewhere: `PREFIX=/usr/local sudo -E scripts/install.sh`.
Alternatively: `cargo install --git https://github.com/aronkv/crabseek crabseek`.

### AUR

Not published yet. The package is ready in [`packaging/aur`](packaging/aur) and goes up
once AUR account registration reopens.

### Uninstall

```sh
# quick install
curl -fsSL https://raw.githubusercontent.com/aronkv/crabseek/main/scripts/get.sh | sh -s -- --uninstall
# from source (add -y to skip the question)
scripts/uninstall.sh
```

This removes the program and everything crabseek stored:

| Removed | Path |
|---|---|
| the program | `~/.local/bin/crabseek` (or `$PREFIX/bin/crabseek`) |
| login and settings | `~/.config/crabseek/` |
| download list, buddies, wishlist, chats | `~/.local/share/crabseek/` |
| share cache | `~/.cache/crabseek/` |
| logs | `~/.local/state/crabseek/` |

Your downloaded music is never touched. The saved password is deleted too, and
Soulseek has no password reset, so note it down if you want to keep the account. After
`cargo install`, run `cargo uninstall crabseek` and then the script for the rest.

## First run

Start `crabseek`. The first time, it asks for a Soulseek username and password and
writes the config once the server accepts them. After that it logs in straight away.

- **Have an account?** Log in with it.
- **New to Soulseek?** Pick any free name. The server creates the account on first
  login, so double-check the spelling.

To forget the saved credentials, run `crabseek logout`. See [Security](#security).

### Let peers reach you

Soulseek is peer-to-peer, so things work best when others can connect to your listen
port (TCP 2234 by default):

- **Router:** crabseek opens the port with UPnP and renews it every 30 minutes. The
  Settings tab shows the result, as does `crabseek portmap`. Without UPnP, forward the
  port by hand.
- **Double NAT:** if Settings says the router "sits behind another NAT", the ISP's modem
  needs a forward to your router, or bridge mode.
- **Local firewall:** allow the port, e.g. `sudo ufw allow 2234/tcp`.

Without this you can still download from peers that are reachable themselves.

### Background mode

Turn on **Settings → Background mode**. From the next start, `q` (or closing the
terminal) only detaches, and crabseek keeps sharing, downloading, running the wishlist
and receiving messages. `crabseek` attaches again; `Q` inside or `crabseek stop` quits
for good.

To start it at login: `systemctl --user enable --now crabseek` (the install script puts
the unit in place). While it runs, CLI commands that log in (`search`, `download`, ...)
refuse to start, since a second login would push it off the server.

## Usage

Press `?` on any tab for an explanation of that tab and all its keys. The essentials:

| Key | Action |
|---|---|
| `s`, `/` | search (`Esc` back to results, `↑`/`↓` for earlier searches) |
| `1`–`8`, `Tab` | Search, Downloads, Uploads, Browse, Chat, Buddies, Wishlist, Settings |
| `j`/`k`, `g`/`G`, `Ctrl-d`/`Ctrl-u` | move, first/last, half page |
| `Enter`, `Space`, `l`/`h` | open or close a folder |
| `d` | download the file or whole folder |
| `f` / `F` | next or previous format filter |
| `b` | browse the selected user's shares |
| `m` / `A` | message the selected user / add them as a buddy |
| `w` | add the current search to the wishlist |
| `c` / `r` / `x` | cancel / retry / remove a transfer |
| `q`, `Ctrl-c` | quit (asks while transfers run) |

Text boxes edit like a shell (`Ctrl-w`, `Ctrl-u`, `Ctrl-←`/`Ctrl-→`, ...).

### Command line

```sh
crabseek                                    # the TUI
crabseek search "artist album" --full-paths [--wishlist]
crabseek download <user> '<remote\path\to\file.flac>'
crabseek userinfo <user>                    # test a peer connection
crabseek browse <user>                      # list a user's shared folders
crabseek message <user> "text"              # send a private message, print replies
crabseek portmap                            # test automatic port forwarding (UPnP)
crabseek shares ["query"] [--dir PATH]      # what you share, and what a search would find
crabseek stop                               # quit crabseek running in the background
crabseek logout                             # forget saved credentials
crabseek config-path                        # where the config lives
```

## Configuration

The download folder, the listen port and the shared folders can be changed in the
**Settings** tab (`8`); changes apply and are saved immediately. Everything lives in
`~/.config/crabseek/config.toml`, and every key except the credentials is optional:

```toml
username = "..."                        # written by the login screen
password = "..."
download_dir = "~/Downloads/crabseek"   # default
shared_dirs = ["~/Music"]               # default
upload_slots = 2                        # default
listen_port = 2234                      # default
upnp = true                             # default: open the port on the router
notifications = false                   # default: no desktop notifications
background = false                      # default: q quits instead of detaching
server = "server.slsknet.org:2242"      # default
```

| File | Contents |
|---|---|
| `~/.config/crabseek/config.toml` | login and settings |
| `~/.local/share/crabseek/downloads.json` | the download list |
| `~/.local/share/crabseek/buddies.json` | the buddy list |
| `~/.local/share/crabseek/wishlist.json` | wishlist queries |
| `~/.local/share/crabseek/chats.json` | private message history (mode 600) |
| `~/.cache/crabseek/shares.json` | cached audio properties of shared files |
| `~/.local/state/crabseek/crabseek.log` | log of the last TUI session (`RUST_LOG=debug` for more) |

## Security

- Soulseek logins need the plain password, so it is stored unencrypted (as in Nicotine+
  and slskd) in `~/.config/crabseek/config.toml`, mode `600` inside a `700` directory,
  written atomically. Encrypting it would not help while the key sits on the same disk;
  system keyring support may come later.
- Credentials never appear in logs.
- If you keep `~/.config` in a dotfiles repository, leave `crabseek/` out.

## Roadmap

- [x] Login, peer connections (direct and indirect), search, downloads with resume
- [x] TUI with folder view, format filter and transfer list
- [x] Sharing your music library and serving uploads
- [x] Distributed search network (as a child node; relaying searches to children is next)
- [x] Browsing a user's shares
- [x] Wishlist
- [x] Private messages
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
| `crates/crabseek` | The binary: ratatui TUI and CLI. |

## Acknowledgements

- The [Nicotine+ protocol documentation](https://nicotine-plus.org/doc/SLSKPROTOCOL.html),
  the reference used to implement the protocol.
- [soulseek-rs](https://github.com/michel/soulseek-rs), another Rust client, consulted
  for comparison. crabseek's code is written from scratch.

## License

[MIT](LICENSE). Please share back what you download, and respect copyright where you live.
