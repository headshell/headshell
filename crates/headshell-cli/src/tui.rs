//! The terminal player interface (PLAN §1.7).
//!
//! **No business logic here.** This file only maps keys to core calls, draws
//! the [`PlaybackAnchor`] and refreshes the screen. The queue, repeat,
//! shuffle, scrobbling — all of it is inside `headshell-core`.
//!
//! The TUI is not the GUI's prototype; it is the proof that the core is fully
//! usable. The core must still be able to offer everything deleted from here.
//!
//! ## Why the position is not polled
//!
//! The core does not shower notifications (D-015). The TUI calls
//! `anchor.position_at(now)` at its own drawing rate and fills in the time in
//! between itself — the GUI and mobile will use the same formula, which is why
//! the formula is in the core.

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph};

use headshell_core::playback::{
    LiveSession, PlayState, PlaybackAnchor, Player, QueueItem, RepeatMode,
};
use headshell_core::session::PlayOptions;
use headshell_core::suggest::SuggestReport;

/// How often the screen is refreshed. Independent of the audio pipeline: only
/// drawing.
const TICK: Duration = Duration::from_millis(200);

/// How long the search line waits after the last key before asking a
/// provider that goes over the network (D-080) — the desktop's pause.
const SEARCH_PAUSE: Duration = Duration::from_millis(300);

/// Tests that the terminal can really be taken **before** the TUI opens.
///
/// `--tui` needs two resources: a terminal and an audio output. The terminal
/// is tested for free, the audio output opens a hardware resource — which is
/// why this is the order. In the reverse order, in an environment without a
/// terminal the user saw a sound card error even though they had typed
/// `--tui`; the wrong diagnosis, the wrong stage.
///
/// The test is done with `enable_raw_mode` itself, not with a separate
/// `is_terminal` criterion: if the two criteria drifted apart, "passed the
/// test, failed when opening" would be born. Raw mode is given back right
/// away; the screen is not changed.
///
/// # Errors
/// If the terminal cannot be put into raw mode.
pub fn require_terminal() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    terminal::disable_raw_mode()
}

/// Puts the terminal into raw mode and gives it back on exit **in every
/// case**.
///
/// Restoring with `Drop`: even on a panic the user's terminal must not be left
/// broken. This is safer than calling `restore()` by hand.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// What the key the user pressed means.
///
/// The key → action mapping is here, the action → core call below. Keeping
/// them apart brings changing the key layout down to a single place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Quit,
    TogglePause,
    Next,
    Previous,
    SelectionUp,
    SelectionDown,
    PlaySelected,
    ToggleShuffle,
    CycleRepeat,
    /// Opens the search line (D-080).
    Search,
}

fn action_for(key: KeyEvent) -> Option<Action> {
    // Ctrl+C always exits: the behaviour expected in a terminal.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Char(' ') | KeyCode::Char('p') => Some(Action::TogglePause),
        KeyCode::Char('n') | KeyCode::Right => Some(Action::Next),
        KeyCode::Char('b') | KeyCode::Left => Some(Action::Previous),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::SelectionUp),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::SelectionDown),
        KeyCode::Enter => Some(Action::PlaySelected),
        KeyCode::Char('s') => Some(Action::ToggleShuffle),
        KeyCode::Char('r') => Some(Action::CycleRepeat),
        KeyCode::Char('/') => Some(Action::Search),
        _ => None,
    }
}

// ————————————————————————————————————— the search line (D-080)
//
// What the TUI has of the desktop's play box: suggestions while typing, from
// the provider chosen to play from — and when it has nothing, from the
// others, named. A row is played at once or added to the queue. The ranking,
// the order the providers are asked in and the queue are the core's; this is
// keys and drawing.

/// What a key does while the search line is open: letters are text there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchAction {
    Quit,
    Close,
    Type(char),
    Erase,
    Up,
    Down,
    /// Play the chosen row at once.
    Play,
    /// Add the chosen row to the queue; the search line stays open.
    Add,
}

fn search_key(key: KeyEvent) -> Option<SearchAction> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return (key.code == KeyCode::Char('c')).then_some(SearchAction::Quit);
    }
    match key.code {
        KeyCode::Esc => Some(SearchAction::Close),
        KeyCode::Enter => Some(SearchAction::Play),
        KeyCode::Tab => Some(SearchAction::Add),
        KeyCode::Up => Some(SearchAction::Up),
        KeyCode::Down => Some(SearchAction::Down),
        KeyCode::Backspace => Some(SearchAction::Erase),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(SearchAction::Type(c))
        }
        _ => None,
    }
}

/// A line of the suggestions, in the order drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// "results from …": the chosen provider had nothing.
    Heading(String),
    Track(QueueItem),
    Artist(String),
    /// A provider that could not be asked (K9).
    Failure(String),
}

impl Row {
    fn pickable(&self) -> bool {
        matches!(self, Self::Track(_) | Self::Artist(_))
    }
}

/// The core's answer as lines: the chosen provider's section as it is, the
/// others under their names, the failures last.
fn rows(report: &SuggestReport) -> Vec<Row> {
    let mut rows = Vec::new();
    for section in &report.sections {
        if section.fallback {
            rows.push(Row::Heading(format!(
                "results from {}:",
                section.display_name
            )));
        }
        rows.extend(section.tracks.iter().cloned().map(Row::Track));
        rows.extend(
            section
                .artists
                .iter()
                .map(|artist| Row::Artist(artist.artist.clone())),
        );
        rows.extend(section.more.iter().cloned().map(Row::Track));
    }
    for failure in &report.failures {
        rows.push(Row::Failure(format!(
            "could not ask {} — STEP: {}",
            failure.display_name, failure.stage
        )));
    }
    rows
}

/// The open search line.
struct Search {
    text: String,
    /// The text the drawn rows answer.
    answered: String,
    rows: Vec<Row>,
    /// Which pickable row is chosen (an index among the pickable ones).
    chosen: usize,
    /// Where the play box searches first, for the line's title.
    from: String,
    /// The chosen provider answers from the catalog: asked at every key.
    instant: bool,
    changed: Instant,
    /// What the last Tab did, said once.
    note: Option<String>,
}

impl Search {
    fn open(from: String, instant: bool) -> Self {
        Self {
            text: String::new(),
            answered: String::new(),
            rows: Vec::new(),
            chosen: 0,
            from,
            instant,
            changed: Instant::now(),
            note: None,
        }
    }

    fn type_char(&mut self, c: char) {
        self.text.push(c);
        self.changed = Instant::now();
    }

    fn erase(&mut self) {
        self.text.pop();
        self.changed = Instant::now();
    }

    /// Is it time to ask: the text changed, and either the answer is at
    /// once or typing has paused.
    fn due(&self, now: Instant) -> bool {
        self.text != self.answered
            && (self.instant || now.saturating_duration_since(self.changed) >= SEARCH_PAUSE)
    }

    fn take(&mut self, text: String, report: &SuggestReport) {
        self.rows = rows(report);
        self.answered = text;
        let count = self.pickable().len();
        self.chosen = self.chosen.min(count.saturating_sub(1));
    }

    fn pickable(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.pickable())
            .map(|(index, _)| index)
            .collect()
    }

    fn step(&mut self, down: bool) {
        let count = self.pickable().len();
        if count == 0 {
            return;
        }
        self.chosen = if down {
            (self.chosen + 1).min(count - 1)
        } else {
            self.chosen.saturating_sub(1)
        };
    }

    /// The chosen row, if there is one to choose.
    fn chosen_row(&self) -> Option<&Row> {
        self.pickable()
            .get(self.chosen)
            .and_then(|&index| self.rows.get(index))
    }
}

/// Plays the chosen row at once, or adds it to the queue (D-080). With no
/// row the typed text's first match is taken, as Enter in the play box does.
/// Returns what was done, to be said.
async fn pick(
    live: &mut LiveSession,
    search: &Search,
    play: bool,
) -> headshell_core::Result<Option<String>> {
    let registry = live.player().providers().clone();
    let limit = PlayOptions::new(String::new()).limit;
    let (items, what) = match search.chosen_row() {
        Some(Row::Track(item)) => (vec![item.clone()], item.track.display_name()),
        Some(Row::Artist(name)) => (
            live.session()
                .queue_from_search(&registry, name, true, limit)
                .await?,
            format!("every track by {name}"),
        ),
        _ if search.text.trim().is_empty() => return Ok(None),
        _ => {
            let items = live
                .session()
                .queue_from_search(&registry, &search.text, false, limit)
                .await?;
            let what = items
                .first()
                .map_or_else(String::new, |item| item.track.display_name());
            (items, what)
        }
    };
    if play {
        let player = live.session().player_from_items(&registry, items).await?;
        live.replace_player(player);
        Ok(Some(format!("playing {what}")))
    } else {
        live.player_mut().enqueue(items).await?;
        Ok(Some(format!("added to the queue: {what}")))
    }
}

/// Runs the TUI; it lasts until the user quits or the queue ends.
///
/// On return the accumulated listen records are written to the library
/// (§1.6) and how many were written is returned.
///
/// # Errors
/// If the terminal cannot be set up or the core returns an error.
pub async fn run(live: &mut LiveSession) -> anyhow::Result<usize> {
    let mut guard = TerminalGuard::enter()?;
    let mut selection = ListState::default();
    selection.select(Some(live.player().queue().position()));
    let mut last_error: Option<String> = None;
    let mut recorded = 0usize;
    let mut search: Option<Search> = None;
    let mut search_list = ListState::default();

    let result = loop {
        // — The search line asks once its text has rested (D-080).
        if let Some(open) = search.as_mut()
            && open.due(Instant::now())
        {
            let text = open.text.clone();
            let registry = live.player().providers().clone();
            match live.session().suggest(&registry, &text).await {
                Ok(report) => open.take(text, &report),
                Err(err) => {
                    // Not asked again until the text changes.
                    open.answered = text;
                    last_error = Some(err.chain_text());
                }
            }
        }
        if let Some(open) = search.as_ref() {
            search_list.select(open.pickable().get(open.chosen).copied());
        }

        // — Draw.
        let player = live.player();
        let anchor = player.anchor();
        // The queue view comes from the core in one piece: play order,
        // position, repeat and shuffle from the same moment. The GUI gets the
        // same type.
        let queue = player.queue().view();
        let queue_items: Vec<String> = queue
            .items
            .iter()
            .map(|item| item.track.display_name())
            .collect();
        let title = player
            .current_track()
            .map(|track| track.display_name())
            .unwrap_or_else(|| "—".to_owned());

        guard.terminal.draw(|frame| {
            draw(
                frame,
                &View {
                    anchor: &anchor,
                    title: &title,
                    queue: &queue_items,
                    playing: queue.position,
                    repeat: queue.repeat,
                    shuffle: queue.shuffle,
                    error: last_error.as_deref(),
                    search: search.as_ref(),
                },
                &mut selection,
                &mut search_list,
            );
        })?;

        // — Read a key (with a timeout: the screen must still refresh).
        let deadline = Instant::now() + TICK;
        let mut quit = false;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !event::poll(remaining)? {
                break;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            // On Windows releasing a key produces an event too; take only presses.
            if key.kind != KeyEventKind::Press {
                continue;
            }
            // While the search line is open, keys are its own: a letter is text.
            if let Some(open) = search.as_mut() {
                let Some(action) = search_key(key) else {
                    continue;
                };
                match action {
                    SearchAction::Quit => {
                        quit = true;
                        break;
                    }
                    SearchAction::Close => search = None,
                    SearchAction::Type(c) => open.type_char(c),
                    SearchAction::Erase => open.erase(),
                    SearchAction::Up => open.step(false),
                    SearchAction::Down => open.step(true),
                    SearchAction::Play | SearchAction::Add => {
                        let play = action == SearchAction::Play;
                        match pick(live, open, play).await {
                            Ok(done) => {
                                last_error = None;
                                if play && done.is_some() {
                                    search = None;
                                } else {
                                    open.note = done;
                                }
                                selection.select(Some(live.player().queue().position()));
                            }
                            Err(err) => last_error = Some(err.chain_text()),
                        }
                    }
                }
                // Draw what the key did, and ask, before the next key.
                break;
            }
            let Some(action) = action_for(key) else {
                continue;
            };
            if action == Action::Quit {
                quit = true;
                break;
            }
            if action == Action::Search {
                let registry = live.player().providers().clone();
                match live.session().play_from(&registry) {
                    Ok(from) => {
                        let name = from
                            .provider
                            .as_ref()
                            .and_then(|id| registry.get(id))
                            .map_or_else(
                                || "nowhere".to_owned(),
                                |provider| provider.info().display_name,
                            );
                        search = Some(Search::open(name, from.instant));
                    }
                    Err(err) => last_error = Some(err.chain_text()),
                }
                break;
            }
            if let Err(err) =
                apply(live.player_mut(), action, &mut selection, queue_items.len()).await
            {
                // An error does not bring the TUI down: it is shown to the user and the
                // loop carries on.
                last_error = Some(err.chain_text());
            } else {
                last_error = None;
            }
        }
        if quit {
            break Ok(());
        }

        // — One round: if the track ended, move on to the next; write the
        // accumulated listens.
        match live.tick().await {
            Ok(report) => {
                recorded += report.listens_recorded;
                // If the store could not write, it does not stay silent: the records
                // were held and will be retried next round, but the user should know.
                //
                // It is only written **if** there is an error: an unconditional
                // assignment would wipe out, on the next round, the error of the key
                // the user just pressed.
                if let Some(text) = report.store_error {
                    last_error = Some(format!(
                        "{text}\n  → {} listens held back",
                        report.listens_pending
                    ));
                }
                if report.finished {
                    break Ok(());
                }
            }
            Err(err) => last_error = Some(err.chain_text()),
        }
    };
    let result: io::Result<()> = result;
    result?;

    let summary = live.shutdown()?;
    Ok(recorded + summary.inserted)
}

/// Turns an action into a core call.
///
/// Note: no decision is made here. The answer to "pause or resume" is in
/// `Player::toggle_pause`; the TUI only passes it on.
async fn apply(
    player: &mut Player,
    action: Action,
    selection: &mut ListState,
    queue_len: usize,
) -> headshell_core::Result<()> {
    match action {
        // Quitting and the search line are handled in the calling loop; they
        // never get here.
        Action::Quit | Action::Search => {}
        Action::TogglePause => player.toggle_pause(),
        Action::Next => {
            player.next().await?;
            selection.select(Some(player.queue().position()));
        }
        Action::Previous => {
            player.previous().await?;
            selection.select(Some(player.queue().position()));
        }
        Action::SelectionUp => {
            let current = selection.selected().unwrap_or(0);
            selection.select(Some(current.saturating_sub(1)));
        }
        Action::SelectionDown => {
            let current = selection.selected().unwrap_or(0);
            selection.select(Some((current + 1).min(queue_len.saturating_sub(1))));
        }
        Action::PlaySelected => {
            if let Some(index) = selection.selected() {
                player.jump_to(index).await?;
            }
        }
        Action::ToggleShuffle => {
            let shuffle = player.queue().shuffle();
            player.queue_mut().set_shuffle(!shuffle);
            selection.select(Some(player.queue().position()));
        }
        Action::CycleRepeat => {
            let next = match player.queue().repeat() {
                RepeatMode::Off => RepeatMode::All,
                RepeatMode::All => RepeatMode::One,
                RepeatMode::One => RepeatMode::Off,
            };
            player.queue_mut().set_repeat(next);
        }
    }
    Ok(())
}

/// Everything needed for drawing — a read-only view gathered in one place.
struct View<'a> {
    anchor: &'a PlaybackAnchor,
    title: &'a str,
    queue: &'a [String],
    playing: usize,
    repeat: RepeatMode,
    shuffle: bool,
    error: Option<&'a str>,
    search: Option<&'a Search>,
}

fn draw(
    frame: &mut ratatui::Frame<'_>,
    view: &View<'_>,
    selection: &mut ListState,
    search_list: &mut ListState,
) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3), // playing track
        Constraint::Length(3), // progress
        Constraint::Min(3),    // queue
        Constraint::Length(if view.error.is_some() { 3 } else { 1 }),
    ])
    .split(area);

    draw_now_playing(frame, chunks[0], view);
    draw_progress(frame, chunks[1], view.anchor);
    match view.search {
        Some(search) => draw_search(frame, chunks[2], search, search_list),
        None => draw_queue(frame, chunks[2], view, selection),
    }
    draw_footer(frame, chunks[3], view);
}

/// The search line and its suggestions, where the queue was.
fn draw_search(frame: &mut ratatui::Frame<'_>, area: Rect, search: &Search, list: &mut ListState) {
    let parts = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(area);
    let accent = Style::default().fg(Color::Rgb(255, 180, 84));
    let dim = Style::default().fg(Color::DarkGray);
    let typed = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            search.text.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("▏", accent),
    ]);
    frame.render_widget(
        Paragraph::new(typed).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" play from {} ", search.from)),
        ),
        parts[0],
    );

    let items: Vec<ListItem<'_>> = search
        .rows
        .iter()
        .map(|row| match row {
            Row::Heading(text) => ListItem::new(Line::from(Span::styled(
                text.clone(),
                accent.add_modifier(Modifier::BOLD),
            ))),
            Row::Track(item) => ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    item.track.title.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" · {}", item.track.artist), dim),
            ])),
            Row::Artist(name) => ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::raw(name.clone()),
                Span::styled("  artist", dim),
            ])),
            Row::Failure(text) => ListItem::new(Line::from(Span::styled(
                text.clone(),
                Style::default().fg(Color::Red),
            ))),
        })
        .collect();
    let title = if search.text.trim().is_empty() {
        " type a track or an artist "
    } else if search.text != search.answered {
        " … "
    } else if search.rows.is_empty() {
        " nothing found "
    } else {
        " suggestions "
    };
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        parts[1],
        list,
    );
}

fn draw_now_playing(frame: &mut ratatui::Frame<'_>, area: Rect, view: &View<'_>) {
    let state = match view.anchor.state {
        PlayState::Playing => ("▶", Color::Green, "playing"),
        PlayState::Paused => ("⏸", Color::Yellow, "paused"),
        PlayState::Buffering => ("⋯", Color::Cyan, "waiting"),
        PlayState::Stopped => ("■", Color::DarkGray, "stopped"),
    };
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", state.0),
            Style::default().fg(state.1).add_modifier(Modifier::BOLD),
        ),
        Span::styled(view.title, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("  ({})", state.2),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" headshell ")),
        area,
    );
}

fn draw_progress(frame: &mut ratatui::Frame<'_>, area: Rect, anchor: &PlaybackAnchor) {
    // The position is **computed** from the anchor — the core is not asked
    // (D-015).
    let position = anchor.position_now();
    let (ratio, label) = match anchor.duration_ms {
        Some(duration) if duration > 0 => {
            #[expect(
                clippy::cast_precision_loss,
                reason = "progress bar ratio; the loss is invisible at ms scale"
            )]
            let ratio = (position as f64 / duration as f64).clamp(0.0, 1.0);
            (ratio, format!("{} / {}", clock(position), clock(duration)))
        }
        _ => (0.0, clock(position)),
    };

    frame.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL))
            .gauge_style(Style::default().fg(Color::Rgb(255, 180, 84)))
            .ratio(ratio)
            .label(label),
        area,
    );
}

fn draw_queue(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    view: &View<'_>,
    selection: &mut ListState,
) {
    let items: Vec<ListItem<'_>> = view
        .queue
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let marker = if index == view.playing { "▸ " } else { "  " };
            let style = if index == view.playing {
                Style::default()
                    .fg(Color::Rgb(255, 180, 84))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::styled(name.clone(), style),
            ]))
        })
        .collect();

    let title = format!(
        " queue ({}) · repeat: {} · shuffle: {} ",
        view.queue.len(),
        repeat_label(view.repeat),
        if view.shuffle { "on" } else { "off" }
    );
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        selection,
    );
}

/// The **user-facing** name of the repeat mode.
///
/// `RepeatMode`'s `Display` prints `off`/`all`/`one`, and that is the right
/// place for it: that string is the wire value going to JSON and IPC. The text
/// written on screen is a separate thing (D-036, D-073) — tying the two into
/// one function would make it impossible to fix the interface text without
/// changing the wire format.
fn repeat_label(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "off",
        RepeatMode::All => "all",
        RepeatMode::One => "one",
    }
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, view: &View<'_>) {
    if let Some(error) = view.error {
        // The error does not cover the screen, but it is not hidden either (K9):
        // its first line is enough; all of it can be read with `headshell diag`.
        let first = error.lines().next().unwrap_or(error);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {first}  (details: headshell diag)"),
                Style::default().fg(Color::Red),
            )))
            .block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    }
    if let Some(search) = view.search {
        let mut spans = vec![Span::styled(
            " enter play · tab add to the queue · ↑↓ choose · esc back",
            Style::default().fg(Color::DarkGray),
        )];
        if let Some(note) = &search.note {
            spans.push(Span::styled(
                format!("   {note}"),
                Style::default().fg(Color::Green),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " space pause · n/b next/previous · ↑↓ select · enter play · / search · s shuffle · r repeat · q quit",
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

/// Turns milliseconds into the `3:07` format.
fn clock(ms: u64) -> String {
    let total_seconds = ms / 1000;
    format!("{}:{:02}", total_seconds / 60, total_seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire value stays as it is; the text written on screen is a separate
    /// string (D-036).
    #[test]
    fn the_repeat_label_is_turkish_while_the_wire_value_stays_english() {
        assert_eq!(repeat_label(RepeatMode::Off), "off");
        assert_eq!(repeat_label(RepeatMode::All), "all");
        assert_eq!(repeat_label(RepeatMode::One), "one");
        // The wire side must not change: JSON and IPC read it.
        assert_eq!(RepeatMode::All.as_str(), "all");
        assert_eq!(RepeatMode::One.to_string(), "one");
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn clock_pads_seconds() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(7_000), "0:07");
        assert_eq!(clock(187_000), "3:07");
        assert_eq!(clock(3_600_000), "60:00");
    }

    #[test]
    fn keys_map_to_the_expected_actions() {
        assert_eq!(
            action_for(key(KeyCode::Char(' '))),
            Some(Action::TogglePause)
        );
        assert_eq!(action_for(key(KeyCode::Char('q'))), Some(Action::Quit));
        assert_eq!(action_for(key(KeyCode::Esc)), Some(Action::Quit));
        assert_eq!(action_for(key(KeyCode::Char('n'))), Some(Action::Next));
        assert_eq!(action_for(key(KeyCode::Left)), Some(Action::Previous));
        assert_eq!(action_for(key(KeyCode::Enter)), Some(Action::PlaySelected));
        assert_eq!(
            action_for(key(KeyCode::Char('s'))),
            Some(Action::ToggleShuffle)
        );
        assert_eq!(
            action_for(key(KeyCode::Char('r'))),
            Some(Action::CycleRepeat)
        );
        assert_eq!(
            action_for(key(KeyCode::Char('z'))),
            None,
            "an unknown key must be ignored"
        );
    }

    #[test]
    fn ctrl_c_always_quits() {
        // The behaviour expected in a terminal, even though 'c' alone does nothing.
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action_for(ctrl_c), Some(Action::Quit));
        assert_eq!(action_for(key(KeyCode::Char('c'))), None);
    }

    #[test]
    fn slash_opens_the_search_line_and_there_letters_are_text() {
        assert_eq!(action_for(key(KeyCode::Char('/'))), Some(Action::Search));
        // `q` quits the player, but in the search line it is a letter.
        assert_eq!(
            search_key(key(KeyCode::Char('q'))),
            Some(SearchAction::Type('q'))
        );
        assert_eq!(
            search_key(key(KeyCode::Char(' '))),
            Some(SearchAction::Type(' '))
        );
        assert_eq!(search_key(key(KeyCode::Enter)), Some(SearchAction::Play));
        assert_eq!(search_key(key(KeyCode::Tab)), Some(SearchAction::Add));
        assert_eq!(search_key(key(KeyCode::Esc)), Some(SearchAction::Close));
        assert_eq!(
            search_key(key(KeyCode::Backspace)),
            Some(SearchAction::Erase)
        );
        assert_eq!(search_key(key(KeyCode::Down)), Some(SearchAction::Down));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(search_key(ctrl_c), Some(SearchAction::Quit));
    }

    fn report_with_fallback() -> SuggestReport {
        use headshell_core::ids::{ProviderId, ProviderTrackId};
        use headshell_core::model::TrackRef;
        use headshell_core::session::PlayFrom;
        use headshell_core::suggest::{ArtistSuggestion, SuggestFailure, SuggestSection};

        let track = |title: &str| QueueItem {
            id: ProviderTrackId::new(ProviderId::new("echo"), title),
            track: TrackRef::new("Ezhel", title),
        };
        SuggestReport {
            query: "gece".to_owned(),
            play_from: PlayFrom {
                provider: Some(ProviderId::new("local")),
                chosen: false,
                instant: true,
                missing: None,
            },
            sections: vec![SuggestSection {
                provider: ProviderId::new("echo"),
                display_name: "Echo (test)".to_owned(),
                fallback: true,
                tracks: vec![track("Geceler")],
                artists: vec![ArtistSuggestion {
                    artist: "Ezhel".to_owned(),
                }],
                more: vec![track("Gece Yarısı")],
            }],
            failures: vec![SuggestFailure {
                provider: ProviderId::new("srv"),
                display_name: "srv".to_owned(),
                stage: "NETWORK_REQUEST".to_owned(),
                chain: "STEP: NETWORK_REQUEST\n  → refused".to_owned(),
            }],
        }
    }

    #[test]
    fn suggestions_are_drawn_in_the_cores_order_with_the_fallback_named() {
        let lines = rows(&report_with_fallback());
        assert_eq!(
            lines[0],
            Row::Heading("results from Echo (test):".to_owned())
        );
        assert!(matches!(&lines[1], Row::Track(item) if item.track.title == "Geceler"));
        assert_eq!(lines[2], Row::Artist("Ezhel".to_owned()));
        assert!(matches!(&lines[3], Row::Track(item) if item.track.title == "Gece Yarısı"));
        assert_eq!(
            lines[4],
            Row::Failure("could not ask srv — STEP: NETWORK_REQUEST".to_owned())
        );
    }

    #[test]
    fn the_choice_skips_headings_and_stays_inside_the_list() {
        let mut search = Search::open("Local files".to_owned(), true);
        search.text = "gece".to_owned();
        search.take("gece".to_owned(), &report_with_fallback());
        assert!(
            matches!(search.chosen_row(), Some(Row::Track(item)) if item.track.title == "Geceler")
        );
        search.step(true);
        assert_eq!(search.chosen_row(), Some(&Row::Artist("Ezhel".to_owned())));
        search.step(true);
        search.step(true);
        assert!(
            matches!(search.chosen_row(), Some(Row::Track(item)) if item.track.title == "Gece Yarısı"),
            "the failure line cannot be chosen, and the choice stops at the last row"
        );
        search.step(false);
        search.step(false);
        search.step(false);
        assert!(
            matches!(search.chosen_row(), Some(Row::Track(item)) if item.track.title == "Geceler")
        );
    }

    #[test]
    fn a_provider_over_the_network_is_asked_after_a_pause_the_catalog_at_once() {
        let mut catalog = Search::open("Local files".to_owned(), true);
        catalog.type_char('g');
        assert!(
            catalog.due(catalog.changed),
            "the catalog is asked at every key"
        );

        let mut network = Search::open("YouTube Music".to_owned(), false);
        network.type_char('g');
        assert!(!network.due(network.changed + Duration::from_millis(100)));
        assert!(network.due(network.changed + SEARCH_PAUSE));
        // Once answered, the same text is not asked again.
        network.answered = "g".to_owned();
        assert!(!network.due(network.changed + SEARCH_PAUSE));
    }

    #[test]
    fn vim_keys_move_the_selection() {
        assert_eq!(
            action_for(key(KeyCode::Char('j'))),
            Some(Action::SelectionDown)
        );
        assert_eq!(
            action_for(key(KeyCode::Char('k'))),
            Some(Action::SelectionUp)
        );
    }
}
