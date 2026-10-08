<div align="center">

# crabseek

<img src="showcase.gif" alt="crabseek searching and downloading an album" width="1000" />

**A fast, keyboard-driven [Soulseek](https://www.slsknet.org/) client for the terminal, written in Rust.**

[![CI](https://github.com/aronkv/crabseek/actions/workflows/ci.yml/badge.svg)](https://github.com/aronkv/crabseek/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/aronkv/crabseek/graph/badge.svg)](https://codecov.io/gh/aronkv/crabseek)
![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)

</div>

A Soulseek client that runs in your terminal. No daemon, no web UI: run `crabseek`,
search, and download whole albums in a few keystrokes.

## Features

- Live search, grouped by folder, with a format filter (FLAC, MP3 320, ...)
- One-key downloads of files or whole folders, with resume
- Browse users' shares, buddies, private messages and a wishlist
- Shares your `~/Music` with other users
- Handles port forwarding (UPnP) and NAT for you
- Vim-style keys, and `?` shows help on every tab

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/aronkv/crabseek/main/scripts/get.sh | sh
```

This installs a prebuilt binary to `~/.local/bin` (x86_64 Linux). To build from source
instead (Rust 1.88+):

```sh
cargo install --git https://github.com/aronkv/crabseek crabseek
```

## Use

Run `crabseek` and log in with your Soulseek account. If you don't have one, pick any
free username and password, and the account is created on first login.

| Key | Action |
|---|---|
| `/` | search |
| `Enter` | open a folder |
| `d` | download |
| `f` | change the format filter |
| `1`–`8` | switch tabs |
| `?` | help for the current tab |
| `q` | quit |

Settings (download folder, shared folders, port) are on the Settings tab (`8`) and are
saved to `~/.config/crabseek/config.toml`. Run `crabseek --help` for the command line
tools.

## License

[MIT](LICENSE). Please share back what you download.
