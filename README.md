<p align="center"><img src="assets/icon.png" width="96" alt="chs icon"></p>

<h1 align="center">chs</h1>

<p align="center"><b>C</b>onversation <b>h</b>istory <b>s</b>earch: browse, fuzzy-search and resume your recent
<b>Claude Code</b>, <b>Codex CLI</b> and <b>claudellm</b> sessions from one list.</p>

```
❯ rust parser                                                    6/49  all
╭───────────────────────────────────────────────────╮╭ preview ───────────────────────╮
│▶ claude    2026-11-08 02:52 log-parser  Rewrite t… ││claude                          │
│  claude    2026-11-08 03:05 api-server  Fix the r… ││Rewrite the log parser in Rust  │
│  claudellm 2026-09-24 01:40 web-shop    Find bugs… ││folder: /home/me/log-parser     │
│  codex     2026-09-21 11:02 dotfiles    Review th… ││first prompts                   │
│                                                    ││› the parser chokes on CRLF …   │
╰───────────────────────────────────────────────────╯╰────────────────────────────────╯
↑↓ select · Enter resume · Tab source · Ctrl+T preview · Ctrl+A headless · Esc quit
```

- **One list for every agent.** Claude Code, Codex and any Claude Code run with its own config dir
  (claudellm, work/personal profiles…) - each source has its own color.
- **Just type.** The 20 most recent sessions show at once; any letter you type searches the 200 most
  recent, fuzzy and typo-tolerant, in titles, folders and every prompt you wrote. The 20 best matches win.
- **Enter resumes** the session in its own folder (`claude --resume <id>`, `codex resume <id>`, …).
- **No daemon.** An incremental on-disk index makes a start cost one `stat` per file (≈10 ms, ≈12 MB),
  and on Linux/macOS chs *replaces itself* with the resumed session, so it holds no memory while you work.
- **Windows tray** (optional), PATH registration and autostart, all switchable in the config.
- **Everything is configurable**: columns, colors, keys, sources, resume commands, search weights, UI language.

## Install

Download an archive from [Releases](https://github.com/sergqwer/chs/releases), unpack it anywhere and run
`chs` once - by its full path. That first run puts it on your PATH (see below); from then on `chs` works
from any terminal.

Or build it (Rust 1.85+):

```sh
git clone https://github.com/sergqwer/chs && cd chs
cargo build --release
./target/release/chs          # first run links itself into /usr/local/bin
```

Windows from Linux: `cargo build --release --target x86_64-pc-windows-gnu` (needs `mingw-w64`).

### What the first run sets up

| | Linux / macOS | Windows |
|---|---|---|
| `chs` on PATH | symlink in `/usr/local/bin` (uses `sudo -n` if needed, falls back to `~/.local/bin`) | install folder added to the *user* PATH (`HKCU\Environment`), no admin rights |
| Tray | - | `chs-tray.exe` started, and registered to start with Windows (`HKCU\…\Run`, full path) |

Every start re-syncs this with the config, so switching an option **off removes what it added**:
`[linux] add_to_path`, `[windows] add_to_path`, `tray_enabled`, `tray_autostart`, `start_tray_with_cli`.
`chs --uninstall` removes it all at once; `chs --no-integration` (or `CHS_NO_INTEGRATION=1`) leaves the
system untouched for one run.

## Use

```
chs                  open the list
chs nginx boring     open it with a search already typed
chs -a               include non-interactive sessions (claude -p, codex exec, subagents)
chs -s codex         only one source
chs -l [query]       print instead of opening the list (-n N, --json); also when stdout is a pipe
chs --reindex        forget the index and re-read every session
```

| Key | Action |
|---|---|
| *any text* | search (words are ANDed; fzf syntax: `'exact` `^prefix` `suffix$` `!exclude`) |
| `↑` `↓` `PgUp` `PgDn` `Ctrl+Home/End` | move |
| `Enter` | resume the selected session |
| `Ctrl+O` / `F4` | print the resume command instead of running it |
| `Tab` / `Shift+Tab` | cycle source filter: all → claude → claudellm → codex |
| `Ctrl+A` / `F3` | show / hide non-interactive sessions |
| `Ctrl+T` / `F2` | toggle the preview |
| `Ctrl+R` / `F5` | re-index |
| `Ctrl+U`, `Ctrl+W` | clear the search, delete a word |
| `Esc` | clear the search, then quit |

All of them can be rebound in `[keys]`.

### Windows tray

Left click opens chs in a terminal (Windows Terminal, the classic console or a command of your own -
`[windows] terminal`). The menu has *Edit config*, *Open config folder*, *Start with Windows* (writes
`tray_autostart` into the config) and *Quit*. The tray loads nothing: it sleeps in the message loop until
you click, reads the config fresh each time, and launches `chs.exe` by its full path.

## Configuration

`chs --config-path` prints where it is: `~/.config/chs/config.toml` (Linux/macOS),
`%APPDATA%\chs\config.toml` (Windows). It is created on first run with **every option and a comment
explaining it** - see [`config.default.toml`](config.default.toml). Keys you delete fall back to the
defaults, so the file can also hold only what you change.

Sources are a list, so any number of Claude Code / Codex homes can be shown:

```toml
[[sources]]
name = "work"                 # label in the list
format = "claude"             # "claude" or "codex"
path = "~/.claude-work"       # the folder holding projects/ (claude) or sessions/ (codex)
color = "#61AFEF"
include_headless = false
resume_command = ["claude", "--resume", "{id}"]   # {id} {cwd} {file}; runs in the session's folder
```

A session counts as *non-interactive* when its `entrypoint` (Claude) or `source` (Codex) is in
`headless_markers` (defaults: `sdk-cli sdk-ts sdk-py` / `exec subagent mcp`). They are hidden unless
`include_headless = true` or you press `Ctrl+A`.

## Translations

The UI is English. To translate it, no rebuild needed:

```sh
chs --dump-lang > ~/.config/chs/uk.toml   # every string, with English values
# translate the values (keep the {placeholders})
```

and set `language_file = "~/.config/chs/uk.toml"` in `[general]`. Missing keys stay English.

## How it works

- **Claude Code** (and claudellm etc.): `<home>/projects/<folder>/<session>.jsonl`. Title = your
  `/rename`, else Claude's auto title, else the first prompt. Slash-command envelopes, hook output and
  tool results are not counted as prompts; subagent transcripts are skipped.
- **Codex**: `<home>/sessions/YYYY/MM/DD/rollout-*.jsonl`, prompts from user-message events (old and new
  formats), injected context blocks filtered out.
- Lines are pre-filtered with a substring search before any JSON parsing, so multi-megabyte tool outputs
  are skipped almost for free. Files are append-only, so a grown file is read only from where the last
  run stopped. The index lives in `~/.cache/chs/index.json` (`%LOCALAPPDATA%\chs` on Windows).
- Search: [nucleo](https://github.com/helix-editor/nucleo) fuzzy matching per word, scored against each
  field separately; each score is normalised by the word's perfect score and has to stay within about one
  word of text, so letters scattered across a long prompt do not count as a match.

## License

MIT
