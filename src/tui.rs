//! The interactive list. Typing goes straight into the search; actions live on
//! special keys and Ctrl/Alt combinations (see `[keys]` in the config).

use crate::config::{Border, Column, Config, FolderStyle, PreviewPosition};
use crate::i18n::{fill, Strings};
use crate::index;
use crate::keys::{Action, KeyMap};
use crate::launch;
use crate::search::{Query, Searcher};
use crate::session::{one_line, truncate_chars, Session};
use anyhow::Result;
use chrono::{Local, TimeZone};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame, TerminalOptions, Viewport};
use std::str::FromStr;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub enum Outcome {
    Quit,
    Resume(Box<Session>),
    Print(Box<Session>),
}

pub struct Options {
    pub query: String,
    pub show_headless: bool,
    pub source: Option<String>,
}

pub fn run(cfg: &Config, t: &Strings, opts: Options) -> Result<Outcome> {
    let keymap = KeyMap::new(&cfg.keys)?;
    let inline = cfg.display.height > 0;
    let mut terminal = if inline {
        ratatui::try_init_with_options(TerminalOptions { viewport: Viewport::Inline(cfg.display.height) })?
    } else {
        ratatui::try_init()?
    };
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let mut app = App::new(cfg, t, keymap, opts);
    let result = app.event_loop(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    if inline {
        let _ = terminal.clear();
    }
    ratatui::restore();
    result
}

struct Row {
    session: usize,
    prompt: Option<usize>,
}

struct Theme {
    selected: Style,
    matched: Style,
    date: Style,
    folder: Style,
    title: Style,
    headless: Style,
    border: Style,
    query: Style,
    help: Style,
    sources: Vec<Color>,
}

fn color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("reset") {
        None
    } else {
        Color::from_str(s).ok()
    }
}

fn fg(s: &str) -> Style {
    color(s).map(|c| Style::default().fg(c)).unwrap_or_default()
}

impl Theme {
    fn new(cfg: &Config) -> Theme {
        let th = &cfg.theme;
        let mut selected = Style::default().add_modifier(Modifier::BOLD);
        if let Some(c) = color(&th.selected_bg) {
            selected = selected.bg(c);
        }
        if let Some(c) = color(&th.selected_fg) {
            selected = selected.fg(c);
        }
        Theme {
            selected,
            matched: fg(&th.match_fg).add_modifier(Modifier::BOLD),
            date: fg(&th.date_fg),
            folder: fg(&th.folder_fg),
            title: fg(&th.title_fg),
            headless: fg(&th.headless_fg),
            border: fg(&th.border_fg),
            query: fg(&th.query_fg),
            help: fg(&th.help_fg),
            sources: cfg.sources.iter().map(|s| color(&s.color).unwrap_or(Color::Cyan)).collect(),
        }
    }
}

struct App<'a> {
    cfg: &'a Config,
    t: &'a Strings,
    keys: KeyMap,
    theme: Theme,
    sessions: Vec<Session>,
    titles: Vec<String>,
    /// Indices into `sessions` that the current filters allow, newest first.
    pool: Vec<usize>,
    rows: Vec<Row>,
    list: ListState,
    searcher: Searcher,
    parsed: Query,
    query: String,
    /// Cursor position in chars.
    cursor: usize,
    show_preview: bool,
    show_headless: bool,
    source_filter: Option<usize>,
    loaded: bool,
    message: Option<String>,
    list_height: usize,
}

impl<'a> App<'a> {
    fn new(cfg: &'a Config, t: &'a Strings, keys: KeyMap, opts: Options) -> App<'a> {
        let mut searcher = Searcher::new(&cfg.search);
        let parsed = searcher.parse(&opts.query);
        let source_filter = opts.source.and_then(|n| cfg.sources.iter().position(|s| s.name == n));
        App {
            cfg,
            t,
            keys,
            theme: Theme::new(cfg),
            sessions: Vec::new(),
            titles: Vec::new(),
            pool: Vec::new(),
            rows: Vec::new(),
            list: ListState::default(),
            searcher,
            parsed,
            cursor: opts.query.chars().count(),
            query: opts.query,
            show_preview: cfg.display.preview,
            show_headless: opts.show_headless,
            source_filter,
            loaded: false,
            message: None,
            list_height: 20,
        }
    }

    fn load(&mut self, ignore_cache: bool) {
        match index::load(self.cfg, ignore_cache) {
            Ok(l) => {
                self.titles = l.sessions.iter().map(Session::title).collect();
                self.sessions = l.sessions;
            }
            Err(e) => self.message = Some(format!("{e:#}")),
        }
        self.loaded = true;
        self.refilter();
    }

    fn refilter(&mut self) {
        let cfg = self.cfg;
        self.pool = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let Some(si) = cfg.sources.iter().position(|c| c.name == s.source) else { return false };
                self.source_filter.is_none_or(|f| f == si)
                    && (!s.headless || self.show_headless || cfg.sources[si].include_headless)
            })
            .map(|(i, _)| i)
            .take(cfg.general.search_pool)
            .collect();
        self.research();
    }

    fn research(&mut self) {
        self.parsed = self.searcher.parse(&self.query);
        self.rows = if self.parsed.is_empty() {
            self.pool.iter().take(self.cfg.general.recent_count).map(|&i| Row { session: i, prompt: None }).collect()
        } else {
            let refs: Vec<(&Session, &str)> =
                self.pool.iter().map(|&i| (&self.sessions[i], self.titles[i].as_str())).collect();
            self.searcher
                .search(&self.parsed, &refs, self.cfg.general.result_count)
                .into_iter()
                .map(|h| Row { session: self.pool[h.idx], prompt: h.prompt })
                .collect()
        };
        self.list.select(if self.rows.is_empty() { None } else { Some(0) });
        *self.list.offset_mut() = 0;
    }

    fn selected(&self) -> Option<&Session> {
        self.list.selected().and_then(|i| self.rows.get(i)).map(|r| &self.sessions[r.session])
    }

    fn event_loop(&mut self, term: &mut DefaultTerminal) -> Result<Outcome> {
        term.draw(|f| self.draw(f))?;
        self.load(false);
        loop {
            term.draw(|f| self.draw(f))?;
            match event::read()? {
                // Windows reports key releases too; acting on them would type twice.
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    if let Some(out) = self.on_key(k)? {
                        return Ok(out);
                    }
                }
                Event::Paste(s) => {
                    for c in s.chars().filter(|c| !c.is_control()) {
                        self.insert(c);
                    }
                    self.research();
                }
                _ => {}
            }
        }
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.query.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(self.query.len())
    }

    fn move_sel(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        let cur = self.list.selected().unwrap_or(0) as isize;
        self.list.select(Some((cur + delta).clamp(0, n - 1) as usize));
    }

    fn on_key(&mut self, k: event::KeyEvent) -> Result<Option<Outcome>> {
        self.message = None;
        let page = self.list_height.max(1) as isize;
        if let Some(action) = self.keys.action(&k) {
            match action {
                Action::Up => self.move_sel(-1),
                Action::Down => self.move_sel(1),
                Action::PageUp => self.move_sel(-page),
                Action::PageDown => self.move_sel(page),
                Action::First => self.move_sel(isize::MIN / 2),
                Action::Last => self.move_sel(isize::MAX / 2),
                Action::Resume => {
                    if let Some(s) = self.selected() {
                        return Ok(Some(Outcome::Resume(Box::new(s.clone()))));
                    }
                }
                Action::PrintCommand => {
                    if let Some(s) = self.selected() {
                        return Ok(Some(Outcome::Print(Box::new(s.clone()))));
                    }
                }
                Action::Quit => {
                    if k.code == KeyCode::Esc && self.cfg.general.esc_clears_query && !self.query.is_empty() {
                        self.query.clear();
                        self.cursor = 0;
                        self.research();
                    } else {
                        return Ok(Some(Outcome::Quit));
                    }
                }
                Action::ClearQuery => {
                    self.query.clear();
                    self.cursor = 0;
                    self.research();
                }
                Action::DeleteWord => {
                    let chars: Vec<char> = self.query.chars().collect();
                    let mut i = self.cursor;
                    while i > 0 && chars[i - 1].is_whitespace() {
                        i -= 1;
                    }
                    while i > 0 && !chars[i - 1].is_whitespace() {
                        i -= 1;
                    }
                    let (a, b) = (self.byte_at(i), self.byte_at(self.cursor));
                    self.query.replace_range(a..b, "");
                    self.cursor = i;
                    self.research();
                }
                Action::TogglePreview => self.show_preview = !self.show_preview,
                Action::ToggleHeadless => {
                    self.show_headless = !self.show_headless;
                    self.refilter();
                }
                Action::NextSource | Action::PrevSource => {
                    let n = self.cfg.sources.len();
                    // None (all) -> 0 -> 1 -> ... -> n-1 -> None
                    let pos = self.source_filter.map(|i| i + 1).unwrap_or(0);
                    let next = if action == Action::NextSource { (pos + 1) % (n + 1) } else { (pos + n) % (n + 1) };
                    self.source_filter = if next == 0 { None } else { Some(next - 1) };
                    self.refilter();
                }
                Action::Reindex => {
                    self.loaded = false;
                    self.load(true);
                }
                Action::CursorLeft => self.cursor = self.cursor.saturating_sub(1),
                Action::CursorRight => self.cursor = (self.cursor + 1).min(self.query.chars().count()),
                Action::CursorHome => self.cursor = 0,
                Action::CursorEnd => self.cursor = self.query.chars().count(),
            }
            return Ok(None);
        }
        match k.code {
            KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.insert(c);
                self.research();
            }
            // AltGr arrives as Ctrl+Alt on Windows: it types characters (e.g. @ { }).
            KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.insert(c);
                self.research();
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let (a, b) = (self.byte_at(self.cursor - 1), self.byte_at(self.cursor));
                self.query.replace_range(a..b, "");
                self.cursor -= 1;
                self.research();
            }
            KeyCode::Delete if self.cursor < self.query.chars().count() => {
                let (a, b) = (self.byte_at(self.cursor), self.byte_at(self.cursor + 1));
                self.query.replace_range(a..b, "");
                self.research();
            }
            _ => {}
        }
        Ok(None)
    }

    // ------------------------------------------------------------------ drawing

    fn block(&self) -> Block<'static> {
        let (borders, ty) = match self.cfg.display.border {
            Border::Rounded => (Borders::ALL, BorderType::Rounded),
            Border::Plain => (Borders::ALL, BorderType::Plain),
            Border::Double => (Borders::ALL, BorderType::Double),
            Border::Thick => (Borders::ALL, BorderType::Thick),
            Border::None => (Borders::NONE, BorderType::Plain),
        };
        Block::default().borders(borders).border_type(ty).border_style(self.theme.border)
    }

    fn draw(&mut self, f: &mut Frame) {
        let d = &self.cfg.display;
        let area = f.area();
        let mut constraints = vec![Constraint::Length(1), Constraint::Min(3)];
        if d.show_help {
            constraints.push(Constraint::Length(1));
        }
        let parts = Layout::vertical(constraints).split(area);
        self.draw_query(f, parts[0]);
        if d.show_help {
            let help = self.message.clone().unwrap_or_else(|| self.t.help.clone());
            let style = if self.message.is_some() { self.theme.matched } else { self.theme.help };
            f.render_widget(Paragraph::new(Span::styled(fit(&help, parts[2].width as usize), style)), parts[2]);
        }

        let body = parts[1];
        let preview_on = self.show_preview && self.selected().is_some();
        if !preview_on {
            self.draw_list(f, body);
            return;
        }
        let right = match d.preview_position {
            PreviewPosition::Right => true,
            PreviewPosition::Bottom => false,
            PreviewPosition::Auto => body.width >= d.preview_auto_width,
        };
        let size = d.preview_size.clamp(10, 90);
        let split = Layout::default()
            .direction(if right { Direction::Horizontal } else { Direction::Vertical })
            .constraints([Constraint::Percentage(100 - size), Constraint::Percentage(size)])
            .split(body);
        self.draw_list(f, split[0]);
        self.draw_preview(f, split[1]);
    }

    fn draw_query(&self, f: &mut Frame, area: Rect) {
        let th = &self.theme;
        let sym = &self.cfg.theme.prompt_symbol;
        let mut spans = vec![Span::styled(sym.clone(), th.query.add_modifier(Modifier::BOLD))];
        if self.query.is_empty() {
            spans.push(Span::styled(self.t.search_placeholder.clone(), th.help));
        } else {
            spans.push(Span::styled(self.query.clone(), th.query));
        }
        // Right side: counter and active filters.
        let mut right =
            fill(&self.t.counter, &[("shown", &self.rows.len().to_string()), ("total", &self.pool.len().to_string())]);
        right.push_str("  ");
        let src_style = match self.source_filter {
            Some(i) => {
                right.push_str(&self.cfg.sources[i].name);
                Style::default().fg(th.sources[i]).add_modifier(Modifier::BOLD)
            }
            None => {
                right.push_str(&self.t.all_sources);
                th.help
            }
        };
        if self.show_headless {
            right.push(' ');
            right.push_str(&self.t.headless_on);
        }
        let left_w: usize = spans.iter().map(|s| s.content.width()).sum();
        let pad = (area.width as usize).saturating_sub(left_w + right.width() + 1);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(right, src_style));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        let before: usize = self.query.chars().take(self.cursor).map(|c| c.width().unwrap_or(0)).sum();
        let x = area.x + (sym.width() + before).min(area.width.saturating_sub(1) as usize) as u16;
        f.set_cursor_position((x, area.y));
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        let block = self.block();
        let inner = block.inner(area);
        self.list_height = inner.height as usize;
        if self.rows.is_empty() {
            let msg = if !self.loaded {
                self.t.indexing.clone()
            } else if self.sessions.is_empty() {
                fill(&self.t.no_sessions, &[("config", &crate::config::config_path().to_string_lossy())])
            } else {
                self.t.no_matches.clone()
            };
            f.render_widget(
                Paragraph::new(Span::styled(msg, self.theme.help)).wrap(Wrap { trim: true }).block(block),
                area,
            );
            return;
        }
        let d = &self.cfg.display;
        let width = inner.width as usize;
        let sym_w = d.highlight_symbol.width();
        let date_w = self.date_width();
        let now = Local::now().timestamp_millis();
        let mut items = Vec::with_capacity(self.rows.len());
        let rows: Vec<(usize, Option<usize>)> = self.rows.iter().map(|r| (r.session, r.prompt)).collect();
        for (si, _) in rows {
            let s = &self.sessions[si];
            let src_idx = self.cfg.sources.iter().position(|c| c.name == s.source).unwrap_or(0);
            let mut used = sym_w;
            let mut spans: Vec<Span> = Vec::new();
            for (ci, col) in d.columns.iter().enumerate() {
                let last = ci + 1 == d.columns.len();
                let sep = if last { 0 } else { 1 };
                let avail = width.saturating_sub(used);
                if avail == 0 {
                    break;
                }
                match col {
                    Column::Source => {
                        let w = (d.source_width as usize).min(avail);
                        let style = Style::default().fg(self.theme.sources[src_idx]).add_modifier(Modifier::BOLD);
                        spans.push(Span::styled(pad(&fit(&s.source, w), w), style));
                        used += w;
                    }
                    Column::Date => {
                        let w = date_w.min(avail);
                        spans
                            .push(Span::styled(pad(&fit(&self.format_date(s.updated_ms, now), w), w), self.theme.date));
                        used += w;
                    }
                    Column::Folder => {
                        let w = (d.folder_width as usize).min(avail);
                        spans.push(Span::styled(pad(&fit(&folder(&s.cwd, d.folder_style), w), w), self.theme.folder));
                        used += w;
                    }
                    Column::Title => {
                        let w = if last { avail } else { avail.min(60) };
                        let mut title = self.titles[si].clone();
                        let mut style = self.theme.title;
                        if s.headless && d.show_headless_tag {
                            title = format!("{} {}", d.headless_tag, title);
                            style = self.theme.headless;
                        }
                        let hl = if self.parsed.is_empty() {
                            Vec::new()
                        } else {
                            self.searcher.highlight(&self.parsed, &title)
                        };
                        spans.extend(highlighted(&title, &hl, w, style, self.theme.matched));
                        used += w;
                    }
                }
                if sep == 1 && width > used {
                    spans.push(Span::raw(" "));
                    used += 1;
                }
            }
            items.push(ListItem::new(Line::from(spans)));
        }
        let list = List::new(items)
            .block(block)
            .highlight_style(self.theme.selected)
            .highlight_symbol(Line::from(d.highlight_symbol.clone()))
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
        f.render_stateful_widget(list, area, &mut self.list);
    }

    fn date_width(&self) -> usize {
        let sample =
            Local.with_ymd_and_hms(2026, 12, 28, 23, 59, 59).single().map(|d| d.timestamp_millis()).unwrap_or(0);
        self.format_date(sample, sample + 365 * 86_400_000).width().max(4)
    }

    fn format_date(&self, ms: i64, now: i64) -> String {
        let d = &self.cfg.display;
        if d.relative_dates {
            let age = (now - ms).max(0) / 1000;
            let n = |x: i64| x.to_string();
            if age < 60 {
                return self.t.just_now.clone();
            } else if age < 3600 {
                return fill(&self.t.minutes_ago, &[("n", &n(age / 60))]);
            } else if age < 86_400 {
                return fill(&self.t.hours_ago, &[("n", &n(age / 3600))]);
            } else if age < 7 * 86_400 {
                return fill(&self.t.days_ago, &[("n", &n(age / 86_400))]);
            }
        }
        format_ts(ms, &d.date_format)
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let Some(idx) = self.list.selected() else { return };
        let Some(row) = self.rows.get(idx) else { return };
        let (si, hit_prompt) = (row.session, row.prompt);
        let s = &self.sessions[si];
        let d = &self.cfg.display;
        let th = &self.theme;
        let t = self.t;
        let label = |k: &str| Span::styled(format!("{k}: "), th.help);
        let src_idx = self.cfg.sources.iter().position(|c| c.name == s.source).unwrap_or(0);
        let mut lines: Vec<Line> = Vec::new();

        let mut head =
            vec![Span::styled(s.source.clone(), Style::default().fg(th.sources[src_idx]).add_modifier(Modifier::BOLD))];
        if s.headless {
            head.push(Span::styled(format!("  {} {}", d.headless_tag, t.headless_label), th.headless));
        }
        lines.push(Line::from(head));
        lines.push(Line::from(Span::styled(self.titles[si].clone(), Style::default().add_modifier(Modifier::BOLD))));
        lines.push(Line::default());
        let mut folder_line = vec![label(&t.folder), Span::styled(s.cwd.clone(), th.folder)];
        if let Some(b) = &s.branch {
            folder_line.push(Span::styled(format!("  ({} {b})", t.branch), th.help));
        }
        lines.push(Line::from(folder_line));
        if s.started_ms > 0 {
            lines.push(Line::from(vec![label(&t.started), Span::raw(format_ts(s.started_ms, &d.date_format))]));
        }
        lines.push(Line::from(vec![
            label(&t.updated),
            Span::raw(format_ts(s.updated_ms, &d.date_format)),
            Span::styled(format!("   {} {}", s.prompt_count, t.prompts), th.help),
        ]));
        lines.push(Line::from(vec![label(&t.session_id), Span::raw(s.id.clone())]));

        let max = d.preview_prompt_chars.max(20);
        let bullet =
            |p: &str| Line::from(vec![Span::styled("› ", th.help), Span::raw(truncate_chars(&one_line(p), max))]);
        let n = s.prompts.len();
        let first = d.preview_first_prompts.min(n);
        if first > 0 {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(t.first_prompts.clone(), th.help.add_modifier(Modifier::BOLD))));
            for p in &s.prompts[..first] {
                lines.push(bullet(p));
            }
        }
        let last_from = n.saturating_sub(d.preview_last_prompts).max(first);
        if last_from < n {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(t.last_prompts.clone(), th.help.add_modifier(Modifier::BOLD))));
            if last_from > first || s.prompt_count as usize > n {
                let hidden = (s.prompt_count as usize).saturating_sub(first + (n - last_from));
                if hidden > 0 {
                    lines.push(Line::from(Span::styled(fill(&t.more_prompts, &[("n", &hidden.to_string())]), th.help)));
                }
            }
            for p in &s.prompts[last_from..] {
                lines.push(bullet(p));
            }
        }
        if let Some(pi) = hit_prompt {
            if let Some(p) = s.prompts.get(pi) {
                let text = one_line(p);
                let hl = self.searcher.highlight(&self.parsed, &text);
                let (snippet, shl) = window(&text, &hl, max);
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(t.best_match.clone(), th.matched)));
                let mut spans = vec![Span::styled("› ", th.help)];
                spans.extend(highlighted(&snippet, &shl, usize::MAX, Style::default(), th.matched));
                lines.push(Line::from(spans));
            }
        }
        if let Ok(l) = launch::prepare(self.cfg, t, s) {
            lines.push(Line::default());
            lines.push(Line::from(vec![label(&t.resume_with), Span::styled(launch::display(&l), th.help)]));
        }
        let block = self.block().title(Span::styled(format!(" {} ", t.preview_title), th.help));
        f.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }).block(block), area);
    }
}

pub fn format_ts(ms: i64, fmt: &str) -> String {
    use std::fmt::Write;
    let Some(dt) = Local.timestamp_millis_opt(ms).single() else { return String::new() };
    // chrono reports a bad format string as a fmt::Error; to_string() would panic.
    let mut out = String::new();
    if write!(out, "{}", dt.format(fmt)).is_err() {
        out.clear();
        let _ = write!(out, "{}", dt.format("%Y-%m-%d %H:%M"));
    }
    out
}

pub fn folder(cwd: &str, style: FolderStyle) -> String {
    match style {
        FolderStyle::Full => cwd.to_string(),
        FolderStyle::Name => {
            let trimmed = cwd.trim_end_matches(['/', '\\']);
            trimmed.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or(trimmed).to_string()
        }
        FolderStyle::Tilde => {
            if let Some(home) = dirs::home_dir() {
                let h = home.to_string_lossy();
                if let Some(rest) = cwd.strip_prefix(h.as_ref()) {
                    return format!("~{rest}");
                }
            }
            cwd.to_string()
        }
    }
}

/// Cut `s` to `w` display columns, ending in "…" when cut.
pub fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w - 1 {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

fn pad(s: &str, w: usize) -> String {
    let n = w.saturating_sub(s.width());
    format!("{s}{}", " ".repeat(n))
}

/// Spans of `text` (cut to `w` columns) with the chars at `hl` emphasised.
fn highlighted(text: &str, hl: &[usize], w: usize, base: Style, mark: Style) -> Vec<Span<'static>> {
    let text = if w == usize::MAX { text.to_string() } else { fit(text, w) };
    let mut spans = Vec::new();
    let mut cur = String::new();
    let mut cur_hl = false;
    let mut k = 0;
    for (i, c) in text.chars().enumerate() {
        while k < hl.len() && hl[k] < i {
            k += 1;
        }
        let is = k < hl.len() && hl[k] == i;
        if is != cur_hl && !cur.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut cur), if cur_hl { base.patch(mark) } else { base }));
        }
        cur_hl = is;
        cur.push(c);
    }
    if !cur.is_empty() {
        spans.push(Span::styled(cur, if cur_hl { base.patch(mark) } else { base }));
    }
    spans
}

/// A window of about `max` chars around the first highlighted char.
fn window(text: &str, hl: &[usize], max: usize) -> (String, Vec<usize>) {
    let chars: Vec<char> = text.chars().collect();
    let first = hl.first().copied().unwrap_or(0);
    let start = first.saturating_sub(max / 4);
    let end = (start + max).min(chars.len());
    let mut s: String = chars[start..end].iter().collect();
    let mut shift = start as isize;
    if start > 0 {
        s.insert(0, '…');
        shift -= 1;
    }
    if end < chars.len() {
        s.push('…');
    }
    let hl = hl.iter().filter(|&&i| i >= start && i < end).map(|&i| (i as isize - shift) as usize).collect();
    (s, hl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_and_folder() {
        assert_eq!(fit("hello world", 6), "hello…");
        assert_eq!(fit("короткий", 20), "короткий");
        assert_eq!(folder("/home/u/proj/", FolderStyle::Name), "proj");
        assert_eq!(folder("C:\\Users\\u\\chs", FolderStyle::Name), "chs");
    }

    #[test]
    fn bad_date_format_does_not_panic() {
        assert!(!format_ts(1_700_000_000_000, "%Q bogus").is_empty());
    }

    #[test]
    fn window_shifts_highlights() {
        let text = "a".repeat(100) + "match" + &"b".repeat(100);
        let hl: Vec<usize> = (100..105).collect();
        let (s, h) = window(&text, &hl, 40);
        let chars: Vec<char> = s.chars().collect();
        let got: String = h.iter().map(|&i| chars[i]).collect();
        assert_eq!(got, "match");
    }
}
