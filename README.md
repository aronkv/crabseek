<div align="center">
  <h1>crabseek</h1>

  <p>A Soulseek client for the terminal, written in Rust.</p>

  <p>
    <a href="https://github.com/aronkv/crabseek/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/aronkv/crabseek/ci.yml?branch=main&style=flat-square&label=CI" alt="CI status" /></a>
    <a href="https://github.com/aronkv/crabseek/releases"><img src="https://img.shields.io/github/v/release/aronkv/crabseek?style=flat-square" alt="Latest release" /></a>
    <a href="./LICENSE"><img src="https://img.shields.io/github/license/aronkv/crabseek?style=flat-square" alt="MIT license" /></a>
    <img src="https://img.shields.io/badge/Rust-1.88%2B-orange?logo=rust&logoColor=white&style=flat-square" alt="Rust 1.88+" />
  </p>

  <p>
    <a href="#features">Features</a> ·
    <a href="#install">Install</a> ·
    <a href="#usage">Usage</a> ·
    <a href="#command-line">Command line</a>
  </p>

  <img src="showcase.gif" alt="crabseek searching and downloading an album" width="800" />
</div>

crabseek speaks the [Soulseek](https://www.slsknet.org/) protocol natively, with no
daemon, web UI or runtime. Run `crabseek`, search, and download whole albums in a few
keystrokes.

## Features

| Capability | Highlights |
|------------|------------|
| **Search** | Live results grouped by folder, fastest users first, format filter (FLAC, MP3 320, ...) |
| **Downloads** | One key for a file or a whole folder, resume, retry, queue positions |
| **Social** | Browse shares, buddies, private messages, wishlist |
| **Sharing** | Shares `~/Music`, fair upload slots, distributed search network |
| **Networking** | UPnP port forwarding, NAT traversal |
| **Interface** | Vim-style keys, help on every tab, optional background mode and notifications |

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/aronkv/crabseek/main/scripts/get.sh | sh
```

Installs a prebuilt binary to `~/.local/bin` (x86_64 Linux). To build from source
(Rust 1.88+):

```sh
cargo install --git https://github.com/aronkv/crabseek crabseek
```

## Usage

Run `crabseek` and log in with your Soulseek account. New to Soulseek? Pick any free
username and password, and the account is created on first login.

| Key | Action |
|-----|--------|
| `/` | Search |
| `Enter` | Open a folder |
| `d` | Download |
| `f` | Change the format filter |
| `1`–`8` | Switch tabs |
| `?` | Help for the current tab |
| `q` | Quit |

Settings live on the Settings tab (`8`) and in `~/.config/crabseek/config.toml`.

## Command line

| Command | Does |
|---------|------|
| `crabseek search "query"` | Search and print results |
| `crabseek download <user> '<path>'` | Download one file |
| `crabseek browse <user>` | List a user's shares |
| `crabseek logout` | Forget saved credentials |

Run `crabseek --help` for everything else.

## License

[MIT](LICENSE). Please share back what you download.
