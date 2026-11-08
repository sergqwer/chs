use anyhow::{anyhow, bail, Result};
use chs::config::{self, AfterResume, Config};
use chs::i18n::{fill, Strings};
use chs::session::Session;
use chs::{index, launch, platform, search, tui};
use clap::Parser;
use std::io::IsTerminal;

/// Browse, fuzzy-search and resume your recent Claude Code, Codex and claudellm sessions.
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
    /// Include non-interactive sessions (claude -p, codex exec, subagents).
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
                let l = launch::prepare(&cfg, &t, &s)?;
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
    let pool: Vec<&Session> = loaded
        .sessions
        .iter()
        .filter(|s| cli.source.as_ref().is_none_or(|n| *n == s.source))
        .filter(|s| !s.headless || cli.all || cfg.source(&s.source).is_some_and(|c| c.include_headless))
        .take(cfg.general.search_pool)
        .collect();
    let titles: Vec<String> = pool.iter().map(|s| s.title()).collect();
    let picked: Vec<usize> = if query.trim().is_empty() {
        (0..pool.len().min(cli.limit.unwrap_or(cfg.general.recent_count))).collect()
    } else {
        let mut searcher = search::Searcher::new(&cfg.search);
        let q = searcher.parse(query);
        let refs: Vec<(&Session, &str)> = pool.iter().zip(&titles).map(|(s, t)| (*s, t.as_str())).collect();
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
                    "source": s.source, "id": s.id, "title": titles[i], "cwd": s.cwd,
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
        let src = match (color, cfg.source(&s.source).and_then(|c| hex(&c.color))) {
            (true, Some((r, g, b))) => format!("\x1b[1;38;2;{r};{g};{b}m{src}\x1b[0m"),
            _ => src,
        };
        let folder = tui::fit(&tui::folder(&s.cwd, d.folder_style), d.folder_width as usize);
        println!(
            "{src} {}  {folder:<fw$}  {}{}",
            tui::format_ts(s.updated_ms, &d.date_format),
            if s.headless { format!("{} ", d.headless_tag) } else { String::new() },
            tui::fit(&titles[i], 160),
            fw = d.folder_width as usize
        );
    }
    Ok(())
}

fn hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((p(0)?, p(2)?, p(4)?))
}
