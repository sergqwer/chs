use anyhow::{anyhow, bail, Result};
use chs::config::{self, AfterResume, Config};
use chs::i18n::{fill, Strings};
use chs::session::Session;
use chs::{index, launch, platform, search, tui};
use clap::Parser;
use std::io::IsTerminal;

/// Browse, fuzzy-search and resume your recent terminal AI agent sessions: Claude Code, Codex,
/// Gemini CLI, Copilot, Cursor, OpenCode, Pi, Qwen, Kimi, DeepSeek Harness, Aider and more.
///
/// Start typing to search; arrows to pick; Enter resumes the session in its own folder.
#[derive(Parser)]
#[command(name = "chs", version, about)]
struct Cli {
    /// Start with this search.
    query: Vec<String>,
    /// Print the sessions instead of opening the list (with a query: the best matches).
    #[arg(short, long)]
    list: bool,
    /// With --list: print JSON.
    #[arg(long)]
    json: bool,
    /// With --list: how many to print.
    #[arg(short = 'n', long)]
    limit: Option<usize>,
    /// Include non-interactive sessions (claude -p, codex exec, subagents, ...).
    #[arg(short, long)]
    all: bool,
    /// Only this source (a [[sources]] name from the config).
    #[arg(short, long)]
    source: Option<String>,
    /// Print where the config file is and exit.
    #[arg(long)]
    config_path: bool,
    /// Write the default config file (add --force to overwrite an existing one).
    #[arg(long)]
    init_config: bool,
    #[arg(long, requires = "init_config")]
    force: bool,
    /// Print every UI string as a language file to translate, and exit.
    #[arg(long)]
    dump_lang: bool,
    /// Forget the index cache and re-read every session file.
    #[arg(long)]
    reindex: bool,
    /// Leave PATH and autostart alone for this run.
    #[arg(long)]
    no_integration: bool,
    /// Remove what chs added to PATH (and the Windows autostart entry), then exit.
    #[arg(long)]
    uninstall: bool,
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("chs: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    if cli.dump_lang {
        print!("{}", Strings::dump());
        return Ok(());
    }
    let path = config::config_path();
    if cli.config_path {
        println!("{}", path.display());
        return Ok(());
    }
    if cli.init_config {
        if config::write_default(&path, cli.force)? {
            println!("{}", path.display());
            return Ok(());
        }
        bail!("{} already exists (add --force to overwrite it)", path.display());
    }

    let (cfg, created) = config::load()?;
    let t = Strings::load(&cfg.general.language_file).unwrap_or_else(|e| {
        eprintln!("chs: {e:#}");
        Strings::default()
    });
    if created {
        eprintln!("{}", fill(&t.config_created, &[("path", &path.to_string_lossy())]));
    }
    if cli.uninstall {
        for note in uninstall(&cfg, &t) {
            eprintln!("chs: {note}");
        }
        return Ok(());
    }
    if !cli.no_integration && std::env::var_os("CHS_NO_INTEGRATION").is_none() {
        for note in platform::sync(&cfg, &t) {
            eprintln!("{note}");
        }
        #[cfg(windows)]
        platform::windows::start_tray_if_wanted(&cfg);
    }
    if cli.reindex {
        index::clear_cache(&cfg);
    }
    if let Some(s) = &cli.source {
        if cfg.source(s).is_none() {
            let names: Vec<&str> = cfg.sources.iter().map(|s| s.name.as_str()).collect();
            bail!("no source named {s:?} (configured: {})", names.join(", "));
        }
    }

    let query = cli.query.join(" ");
    if cli.list || !std::io::stdout().is_terminal() {
        return list(&cfg, &cli, &query);
    }

    loop {
        let opts = tui::Options { query: query.clone(), show_headless: cli.all, source: cli.source.clone() };
        match tui::run(&cfg, &t, opts)? {
            tui::Outcome::Quit => return Ok(()),
            tui::Outcome::Print(s) => {
                println!("{}", launch::display(&launch::prepare(&cfg, &t, &s)?));
                return Ok(());
            }
            tui::Outcome::Resume(s) => {
                let mut l = launch::prepare(&cfg, &t, &s)?;
                launch::make_cwd(&mut l, &t);
                if let Some(n) = &l.note {
                    eprintln!("chs: {n}");
                }
                let code = launch::run(&l, cfg.general.after_resume).map_err(|e| {
                    anyhow!(fill(&t.launch_failed, &[("cmd", &l.argv.join(" ")), ("err", &e.to_string())]))
                })?;
                if cfg.general.after_resume == AfterResume::Exit {
                    std::process::exit(code);
                }
            }
        }
    }
}

fn uninstall(cfg: &Config, t: &Strings) -> Vec<String> {
    #[cfg(windows)]
    {
        let _ = (cfg, t);
        platform::windows::uninstall()
    }
    #[cfg(unix)]
    {
        let mut c = cfg.clone();
        c.linux.add_to_path = false;
        platform::sync(&c, t)
    }
}

/// Non-interactive output, for scripts and for `chs | grep`.
fn list(cfg: &Config, cli: &Cli, query: &str) -> Result<()> {
    let loaded = index::load(cfg, false)?;
    let source = cli.source.as_ref().and_then(|n| cfg.sources.iter().position(|s| s.name == *n));
    let idx = index::pool(cfg, &loaded.sessions, source, cli.all);
    let pool: Vec<&Session> = idx.iter().map(|&i| &loaded.sessions[i]).collect();
    let titles: Vec<&str> = idx.iter().map(|&i| loaded.titles[i].as_str()).collect();
    let picked: Vec<usize> = if query.trim().is_empty() {
        (0..pool.len().min(cli.limit.unwrap_or(cfg.general.recent_count))).collect()
    } else {
        let mut searcher = search::Searcher::new(&cfg.search);
        let q = searcher.parse(query);
        let refs: Vec<(&Session, &str)> = pool.iter().zip(&titles).map(|(s, t)| (*s, *t)).collect();
        searcher.search(&q, &refs, cli.limit.unwrap_or(cfg.general.result_count)).into_iter().map(|h| h.idx).collect()
    };

    let t = Strings::default();
    if cli.json {
        let items: Vec<serde_json::Value> = picked
            .iter()
            .map(|&i| {
                let s = pool[i];
                let resume = launch::prepare(cfg, &t, s).map(|l| launch::display(&l)).unwrap_or_default();
                serde_json::json!({
                    "source": s.source, "id": s.id, "title": titles[i], "cwd": s.cwd, "cwd_missing": s.cwd_missing,
                    "updated": tui::format_ts(s.updated_ms, "%Y-%m-%dT%H:%M:%S%:z"),
                    "headless": s.headless, "prompts": s.prompt_count,
                    "file": s.file, "resume": resume,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    let color = std::io::stdout().is_terminal();
    let d = &cfg.display;
    for &i in &picked {
        let s = pool[i];
        let src = tui::fit(&s.source, d.source_width as usize);
        let src = format!("{src:<w$}", w = d.source_width as usize);
        let src = match tui::color(&cfg.sources[s.src].color).and_then(ansi_fg).filter(|_| color) {
            Some(fg) => format!("\x1b[1;{fg}m{src}\x1b[0m"),
            None => src,
        };
        let fw = d.folder_width as usize;
        let folder = format!("{:<fw$}", tui::fit(&tui::folder_label(cfg, s), fw));
        // Colored after padding: escape codes would count as width.
        let folder = match tui::color(&cfg.theme.missing_fg).and_then(ansi_fg).filter(|_| color && s.cwd_missing) {
            Some(fg) => format!("\x1b[{fg}m{folder}\x1b[0m"),
            None => folder,
        };
        println!(
            "{src} {}  {folder}  {}{}",
            tui::format_ts(s.updated_ms, &d.date_format),
            if s.headless { format!("{} ", d.headless_tag) } else { String::new() },
            tui::fit(titles[i], 160),
        );
    }
    Ok(())
}

/// The SGR parameters for a foreground color, the same colors the TUI accepts.
fn ansi_fg(c: ratatui::style::Color) -> Option<String> {
    use ratatui::style::Color::*;
    let basic = |n: u8| Some(n.to_string());
    match c {
        Rgb(r, g, b) => Some(format!("38;2;{r};{g};{b}")),
        Indexed(i) => Some(format!("38;5;{i}")),
        Black => basic(30),
        Red => basic(31),
        Green => basic(32),
        Yellow => basic(33),
        Blue => basic(34),
        Magenta => basic(35),
        Cyan => basic(36),
        Gray => basic(37),
        DarkGray => basic(90),
        LightRed => basic(91),
        LightGreen => basic(92),
        LightYellow => basic(93),
        LightBlue => basic(94),
        LightMagenta => basic(95),
        LightCyan => basic(96),
        White => basic(97),
        Reset => None,
    }
}
