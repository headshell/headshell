//! Human-readable output formatting.
//!
//! **No business logic here** — only writing the reports the core returns to
//! the terminal. Every number is printed as it came from the core.

use headshell_core::artwork::{ArtworkReport, ArtworkSource, ArtworkStatus};
use headshell_core::config::FolderSource;
use headshell_core::library::SearchHit;
use headshell_core::lyrics::{LyricsFit, LyricsReport, LyricsSource, LyricsStatus};
use headshell_core::playback::{PlanReason, PrefetchReport, PrefetchStatus};
use headshell_core::plugin::catalog::UpdateOutcome;
use headshell_core::recommend::{
    DismissReport, Reason, RecommendReport, RecommendResetReport, SectionKind,
};
use headshell_core::session::{
    ImportReport, LookupMode, MusicFoldersReport, OnlineReport, OnlineSource, PlayFrom, PlayReport,
    PluginCatalogReport, PluginConsentReport, PluginIndexReport, PluginInstallReport,
    PluginListReport, PluginRemoveReport, PluginUpdateReport, ProviderListReport, ProviderRoles,
    ProviderTestReport, ResolveReport, ScanReport, SearchReport, SecretListReport,
    SecretWriteReport, ServerAddReport, ServerListReport, ServerRemoveReport, SleeveResponse,
    StatsResponse, VideoReport,
};
use headshell_core::stats::StatsReport;
use headshell_core::suggest::SuggestReport;
use headshell_core::video::VideoSource;

/// Turns milliseconds into a readable duration like `12h 3m`.
fn duration(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// A count with its noun: `1 play`, `2 plays`.
///
/// English puts the noun in the plural after every count but one; the
/// Turkish this output was first written in did not (D-073).
fn count<N>(n: N, noun: &str) -> String
where
    N: std::fmt::Display + PartialEq + From<u8>,
{
    if n == N::from(1) {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `play`/`plays` for a table column, padded so the columns stay aligned.
fn plays(n: usize) -> &'static str {
    if n == 1 { "play " } else { "plays" }
}

/// The import report.
pub fn import(report: &ImportReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let i = &report.import;
    let _ = writeln!(out, "imported: {} ({})", i.source, i.export);
    let _ = writeln!(out, "  matched files : {}", i.files_matched);
    let _ = writeln!(out, "  raw records   : {}", i.records_total);
    let _ = writeln!(out, "  listens       : {}", i.listens);
    if i.skipped_total() > 0 {
        let detail = i
            .skipped
            .iter()
            .map(|(reason, count)| format!("{reason} {count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "  skipped       : {} ({detail})", i.skipped_total());
    }
    let _ = writeln!(out, "  with ISRC     : {}", i.with_isrc);

    let d = &report.identity;
    let _ = writeln!(
        out,
        "\nidentity: isrc {} · mbid {} · fuzzy {} · fingerprint {} · local {} (authoritative {:.1}%)",
        d.by_isrc,
        d.by_mbid,
        d.by_fuzzy,
        d.by_fingerprint,
        d.by_local_key,
        d.authoritative_ratio() * 100.0
    );

    let w = &report.write;
    let _ = writeln!(
        out,
        "library: new {} · duplicate {} · new tracks {}",
        w.inserted, w.duplicates, w.new_tracks
    );
    out
}

/// The statistics report.
pub fn stats(response: &StatsResponse) -> String {
    use std::fmt::Write as _;
    let r: &StatsReport = &response.report;
    let mut out = String::new();

    let scope = r
        .query
        .year
        .map_or_else(|| "all time".to_owned(), |y| y.to_string());
    let _ = writeln!(out, "period: {scope}");
    let _ = writeln!(
        out,
        "{} · {:.1} hours · {} · {}",
        count(r.plays, "play"),
        r.total_hours(),
        count(r.unique_tracks, "track"),
        count(r.unique_artists, "artist")
    );
    let _ = writeln!(
        out,
        "{} in scope, {} skipped, {} out of scope, {} without an identity",
        count(r.listens_in_scope, "record"),
        count(r.skipped_short, "short play"),
        count(r.out_of_scope, "record"),
        count(r.without_canonical_id, "record")
    );

    if !r.top_artists.is_empty() {
        let _ = writeln!(out, "\ntop artists");
        for (rank, artist) in r.top_artists.iter().enumerate() {
            let _ = writeln!(
                out,
                "  {:>2}. {:<32} {:>5} {}  {:>8}  {}",
                rank + 1,
                truncate(&artist.artist, 32),
                artist.plays,
                plays(artist.plays),
                duration(artist.ms_played),
                count(artist.unique_tracks, "track")
            );
        }
    }

    if !r.top_tracks.is_empty() {
        let _ = writeln!(out, "\ntop tracks");
        for (rank, track) in r.top_tracks.iter().enumerate() {
            let _ = writeln!(
                out,
                "  {:>2}. {:<44} {:>5} {}  {:>8}",
                rank + 1,
                truncate(&format!("{} - {}", track.artist, track.title), 44),
                track.plays,
                plays(track.plays),
                duration(track.ms_played)
            );
        }
    }

    if !r.top_albums.is_empty() {
        let _ = writeln!(out, "\ntop albums");
        for (rank, album) in r.top_albums.iter().enumerate() {
            let _ = writeln!(
                out,
                "  {:>2}. {:<44} {:>5} {}",
                rank + 1,
                truncate(&format!("{} - {}", album.artist, album.album), 44),
                album.plays,
                plays(album.plays)
            );
        }
    }

    if r.by_year.len() > 1 {
        let _ = writeln!(out, "\nby year");
        for year in &r.by_year {
            let _ = writeln!(
                out,
                "  {}  {:>6} {}  {:>8}",
                year.year,
                year.plays,
                plays(year.plays),
                duration(year.ms_played)
            );
        }
    }
    out
}

/// A single-track resolution.
pub fn resolve(report: &ResolveReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let res = &report.resolution;
    let _ = writeln!(out, "query   : {} - {}", report.artist, report.title);
    let _ = writeln!(out, "identity: {}", res.canonical_id);
    let _ = writeln!(
        out,
        "method  : {} (confidence {:.1}%)",
        res.method,
        res.confidence * 100.0
    );
    if let Some(candidate) = &res.matched {
        let _ = writeln!(
            out,
            "match   : {} - {} [{}]",
            candidate.artist, candidate.title, candidate.mbid
        );
        if let Some(note) = &candidate.disambiguation {
            let _ = writeln!(out, "note    : {note}");
        }
    } else {
        let _ = writeln!(out, "match   : none — {}", no_match_reason(res));
    }
    out
}

/// Why a resolution has no match (K9): no candidate came close, candidates
/// could not be told apart (D-046), or the source ranks more as high than
/// were read (D-081). No candidate is chosen in any of them — a tie used to
/// be shown as "a deterministic but arbitrary choice", which D-046 ended.
fn no_match_reason(res: &headshell_core::identity::Resolution) -> String {
    let at_least = if res.unread_equals { "at least " } else { "" };
    if res.tied_candidates > 1 {
        format!(
            "{at_least}{} candidates could not be told apart; no authority claimed",
            res.tied_candidates
        )
    } else if res.unread_equals {
        "the source ranks more candidates as high than were read; none is claimed".to_owned()
    } else {
        "no candidate came close enough".to_owned()
    }
}

/// The covers of a query's tracks (D-076): one line per track, the counts
/// last. "Not found" and "not looked up because offline" are different lines
/// and different counts (K9).
pub fn artwork(report: &ArtworkReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "covers · {} · {}",
        report.subject,
        if report.online {
            "online: MusicBrainz and the Cover Art Archive were asked"
        } else {
            "offline: only where the tracks live (--online also asks MusicBrainz and the Cover Art Archive)"
        }
    );
    for item in &report.items {
        let (mark, label, detail) = match &item.status {
            ArtworkStatus::Found { source } => (
                "✓",
                match source {
                    ArtworkSource::Embedded => "embedded",
                    ArtworkSource::Folder => "folder",
                    ArtworkSource::Provider => "provider",
                    ArtworkSource::CoverArtArchive => "archive",
                },
                None,
            ),
            ArtworkStatus::NotFound { detail } => ("–", "not found", Some(detail.as_str())),
            ArtworkStatus::NotCheckedOffline => ("·", "offline", None),
            ArtworkStatus::Failed { chain } => ("✗", "failed", Some(chain.as_str())),
            ArtworkStatus::Pending => ("…", "pending", None),
        };
        let album = item
            .album
            .as_deref()
            .map(|album| format!("  [{}]", truncate(album, 30)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  {mark} {label:<9} {:>2}. {} — {}{album}",
            item.index + 1,
            truncate(&item.artist, 28),
            truncate(&item.title, 40),
        );
        if let Some(detail) = detail {
            for line in detail.lines() {
                let _ = writeln!(out, "                  {line}");
            }
        }
        for note in &item.notes {
            let _ = writeln!(out, "                  note: {note}");
        }
    }
    let summary = &report.summary;
    let _ = writeln!(
        out,
        "{} · {} found ({} embedded, {} folder, {} provider, {} archive) · {} not found · {} not looked up (offline) · {} failed",
        count(summary.tracks, "track"),
        summary.found(),
        summary.embedded,
        summary.folder,
        summary.provider,
        summary.cover_art_archive,
        summary.not_found,
        summary.not_checked_offline,
        summary.failed,
    );
    for path in &report.written {
        let _ = writeln!(out, "written: {}", path.display());
    }
    out
}

/// The lyrics of a query's tracks, or of a file (D-078).
pub fn lyrics(report: &LyricsReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "lyrics · {}", report.subject);
    for item in &report.items {
        let from = |source: &LyricsSource| match source {
            LyricsSource::Sidecar => "the .lrc beside the file".to_owned(),
            LyricsSource::Embedded => "the file's tags".to_owned(),
            LyricsSource::Provider => item.provider.to_string(),
            LyricsSource::Lrclib => "LRCLIB".to_owned(),
        };
        let (mark, label, detail) = match &item.status {
            LyricsStatus::Found {
                source,
                synced,
                fit,
                delta_ms,
            } => {
                let how = match (fit, delta_ms) {
                    (LyricsFit::Own, _) => format!("from {}", from(source)),
                    (LyricsFit::Length, Some(delta)) => {
                        format!("from LRCLIB, its version {}", length_difference(*delta))
                    }
                    (LyricsFit::Closest, Some(delta)) => format!(
                        "from LRCLIB's nearest version, {} — the timing may be off; --shift moves it",
                        length_difference(*delta)
                    ),
                    _ => format!("from {}, the recording's length unknown", from(source)),
                };
                let (mark, label) = match (synced, fit) {
                    (true, LyricsFit::Closest) => ("≈", "nearest"),
                    (true, _) => ("✓", "synced"),
                    (false, _) => ("✓", "unsynced"),
                };
                (mark, label, Some(how))
            }
            LyricsStatus::Instrumental { source } => (
                "♪",
                "no words",
                Some(format!("instrumental, says {}", from(source))),
            ),
            LyricsStatus::NotFound { detail } => ("–", "not found", Some(detail.clone())),
            LyricsStatus::NotRequested => ("·", "not asked", None),
            LyricsStatus::Failed { chain } => ("✗", "failed", Some(chain.clone())),
            LyricsStatus::Pending => ("…", "pending", None),
        };
        let album = item
            .album
            .as_deref()
            .map(|album| format!("  [{}]", truncate(album, 30)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  {mark} {label:<9} {:>2}. {} — {}{album}",
            item.index + 1,
            truncate(&item.artist, 28),
            truncate(&item.title, 40),
        );
        if let Some(detail) = detail {
            for line in detail.lines() {
                let _ = writeln!(out, "                  {line}");
            }
        }
        if item.offset_ms != 0 {
            let _ = writeln!(
                out,
                "                  shifted {}",
                shift_text(item.offset_ms)
            );
        }
        for note in &item.notes {
            let _ = writeln!(out, "                  note: {note}");
        }
        if let Some(lyrics) = &item.lyrics {
            for line in &lyrics.lines {
                let stamp = line
                    .start_ms
                    .map(|ms| {
                        format!(
                            "[{:02}:{:02}.{:02}] ",
                            ms / 60_000,
                            ms / 1_000 % 60,
                            ms % 1_000 / 10
                        )
                    })
                    .unwrap_or_default();
                let _ = writeln!(out, "      {stamp}{}", line.text);
            }
            if let Some(attribution) = &lyrics.attribution {
                let _ = writeln!(out, "      — {attribution}");
            }
        }
    }
    let summary = &report.summary;
    let _ = writeln!(
        out,
        "{} · {} found ({} synced, {} of them only the nearest version) · {} instrumental · {} not found · {} failed",
        count(summary.tracks, "track"),
        summary.found(),
        summary.synced,
        summary.closest,
        summary.instrumental,
        summary.not_found,
        summary.failed,
    );
    out
}

/// A version's length against the recording's: `1.2 s longer`.
fn length_difference(delta_ms: i64) -> String {
    let magnitude = delta_ms.unsigned_abs();
    let seconds = format!("{}.{} s", magnitude / 1_000, magnitude % 1_000 / 100);
    match delta_ms.signum() {
        1 => format!("{seconds} longer"),
        -1 => format!("{seconds} shorter"),
        _ => "of the same length".to_owned(),
    }
}

/// A shift as the user reads it: `0.5 s sooner`.
fn shift_text(offset_ms: i64) -> String {
    let magnitude = offset_ms.unsigned_abs();
    let seconds = format!("{}.{} s", magnitude / 1_000, magnitude % 1_000 / 100);
    if offset_ms > 0 {
        format!("{seconds} sooner")
    } else {
        format!("{seconds} later")
    }
}

/// Search results.
pub fn search(report: &SearchReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if report.hits.is_empty() {
        let _ = writeln!(out, "no results for {:?}", report.query);
        return out;
    }
    for hit in &report.hits {
        let SearchHit {
            artist,
            title,
            album,
            play_count,
            ms_played,
            ..
        } = hit;
        let _ = writeln!(
            out,
            "{:<44} {:>4} {}  {:>8}  {}",
            truncate(&format!("{artist} - {title}"), 44),
            play_count,
            plays(*play_count),
            duration(*ms_played),
            album.as_deref().unwrap_or("")
        );
    }
    out
}

/// The Sleeve card output.
pub fn sleeve(response: &SleeveResponse) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let d = &response.data;
    let period = d
        .year
        .map_or_else(|| "all time".to_owned(), |y| y.to_string());
    let _ = writeln!(out, "period: {period}");
    let _ = writeln!(
        out,
        "{} × {}  {}  {}  {}",
        response.size.width,
        response.size.height,
        count(d.plays, "play"),
        count(d.unique_tracks, "track"),
        count(d.unique_artists, "artist")
    );
    if d.plays > 0 {
        let (value, unit) = if d.total_ms_played >= 3_600_000 {
            (d.total_ms_played / 3_600_000, "hour")
        } else {
            (d.total_ms_played / 60_000, "minute")
        };
        let _ = writeln!(out, "{} listened", count(value, unit));
        if let Some(artist) = &d.top_artist {
            let _ = writeln!(
                out,
                "top: {} ({})",
                artist.artist,
                count(artist.plays, "play")
            );
        }
    } else {
        let _ = writeln!(out, "no listens yet");
    }
    if let Some(written) = &response.written {
        let _ = writeln!(
            out,
            "written: {} ({} bytes, {})",
            written.path.display(),
            written.bytes,
            written.kind
        );
    }
    out
}

/// The provider list.
pub fn provider_list(report: &ProviderListReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if report.providers.is_empty() {
        let _ = writeln!(out, "no registered providers");
        return out;
    }
    for info in &report.providers {
        // The one the play box asks first is marked (D-080).
        let mark = if report.play_from.provider.as_ref() == Some(&info.id) {
            '*'
        } else {
            ' '
        };
        // `as_str`: the id's `Display` writes the string as it is and ignores
        // the column width.
        let _ = writeln!(
            out,
            "{mark} {:<10} {:<22} {}",
            info.id.as_str(),
            truncate(&info.display_name, 22),
            info.capabilities
        );
    }
    out.push_str(&play_from(&report.play_from));
    if !report.roles.music.follows_main {
        out.push_str(&role_line("main", &report.roles.main));
    }
    out.push_str(&role_line("video", &report.roles.video));
    out
}

/// Who plays each role (D-086): `provider use --for` and `provider follow`.
pub fn roles(roles: &ProviderRoles) -> String {
    let mut out = String::new();
    out.push_str(&role_line("main", &roles.main));
    out.push_str(&role_line("music", &roles.music));
    out.push_str(&role_line("video", &roles.video));
    out
}

/// One role's line: who, and why it is that one.
fn role_line(role: &str, from: &PlayFrom) -> String {
    let mut out = match (&from.provider, from.follows_main, from.chosen) {
        (None, _, _) if role == "video" => {
            "video from: none — no registered provider shows videos\n".to_owned()
        }
        (None, _, _) => format!("{role} from: none — no registered provider can play\n"),
        (Some(id), false, true) => format!("{role} from: {id} (chosen)\n"),
        (Some(id), false, false) => format!(
            "{role} from: {id} (nothing chosen yet: the first that can play — `headshell provider use <name>`)\n"
        ),
        (Some(id), true, _) => format!(
            "{role} from: {id} (follows the main one — `headshell provider use <name> --for {role}` to choose apart)\n"
        ),
    };
    if let Some(missing) = &from.missing {
        out.push_str(&format!(
            "  the chosen {missing} is not registered or cannot do this now — the one above instead\n"
        ));
    }
    out
}

/// The videos found for a track (D-086).
pub fn video(report: &VideoReport) -> String {
    use std::fmt::Write as _;
    let plan = &report.plan;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} — video from {}{}",
        plan.track.display_name(),
        plan.provider,
        if plan.own { " (the track's own)" } else { "" }
    );
    if plan.candidates.is_empty() {
        let _ = writeln!(out, "  no video found");
    }
    for (index, candidate) in plan.candidates.iter().enumerate() {
        let source = match &candidate.source {
            VideoSource::Youtube { video_id } => format!("youtube {video_id}"),
            VideoSource::LocalFile { path } => format!("file {}", path.display()),
        };
        let _ = writeln!(
            out,
            "  {}. {} — {source} ({:.2})",
            index + 1,
            truncate(&candidate.track.display_name(), 48),
            candidate.score
        );
    }
    if plan.refused > 0 {
        let _ = writeln!(out, "  {} not this song", count(plan.refused, "row"));
    }
    if plan.without_video > 0 {
        let _ = writeln!(
            out,
            "  {} without a video",
            count(plan.without_video, "row")
        );
    }
    out
}

/// Where the play box searches first (D-080) — and why, when it is not the
/// user's choice.
pub fn play_from(from: &PlayFrom) -> String {
    let mut out = match (&from.provider, from.chosen) {
        (Some(id), true) => format!("play from: {id} (chosen)\n"),
        (Some(id), false) => format!(
            "play from: {id} (nothing chosen yet: the first that can play — `headshell provider use <name>`)\n"
        ),
        (None, _) => "play from: none — no registered provider can play\n".to_owned(),
    };
    if let Some(missing) = &from.missing {
        out.push_str(&format!(
            "  the chosen {missing} is not registered or cannot play now — searching from the one above\n"
        ));
    }
    out
}

/// The desktop's online choice (D-082): what a desktop started from here
/// goes by and who decided it — and that the CLI is not bound by it.
pub fn online(report: &OnlineReport) -> String {
    let choice = &report.online;
    let switch = |mode: LookupMode| match mode {
        LookupMode::Online => "on",
        LookupMode::Offline => "off",
    };
    let why = match choice.source {
        OnlineSource::Variable => "HEADSHELL_ONLINE in this shell decides, over the saved choice",
        OnlineSource::Saved => "the saved choice",
        OnlineSource::Default => "nothing chosen yet — `headshell online on`",
    };
    let saved = choice.saved.map_or("nothing", switch);
    let variable = match choice.variable {
        Some(LookupMode::Online) => "HEADSHELL_ONLINE=1",
        Some(LookupMode::Offline) => "HEADSHELL_ONLINE=0",
        None => "HEADSHELL_ONLINE not set",
    };
    format!(
        "desktop : online {} ({why})\n\
         saved   : {saved}\n\
         shell   : {variable}\n\
         the CLI : online only with --online, run by run\n\
         online asks MusicBrainz (identities, covers) and the Cover Art Archive\n",
        switch(choice.mode)
    )
}

/// The music folders and who decided them (D-085); after a change, when
/// its songs come in.
pub fn music_folders(report: &MusicFoldersReport, changing: bool) -> String {
    use std::fmt::Write as _;
    let folders = &report.folders;
    let why = match folders.source {
        FolderSource::Variable => "HEADSHELL_MUSIC_DIRS decides this run",
        FolderSource::Saved => "chosen here",
        FolderSource::Default => "nothing chosen — the system's music folder",
    };
    let mut out = format!("music folders ({why}):\n");
    if folders.dirs.is_empty() {
        out.push_str("  none\n");
    }
    for dir in &folders.dirs {
        let _ = writeln!(out, "  {}", dir.display());
    }
    if folders.source == FolderSource::Variable {
        let chosen = folders.saved.as_ref().unwrap_or(&folders.system);
        let list: Vec<String> = chosen.iter().map(|dir| dir.display().to_string()).collect();
        let _ = writeln!(
            out,
            "without the variable: {}",
            if list.is_empty() {
                "none".to_owned()
            } else {
                list.join(", ")
            }
        );
    }
    if changing {
        out.push_str(if report.changed {
            "the songs change at the next scan: headshell provider scan\n"
        } else {
            "already on the list — nothing changed\n"
        });
    }
    out
}

/// What the play box offers for a text (D-080): the chosen provider's
/// section, or every other one's, named.
pub fn suggest(report: &SuggestReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let first = report
        .play_from
        .provider
        .as_ref()
        .map_or_else(|| "no provider".to_owned(), ToString::to_string);
    if report
        .sections
        .first()
        .is_some_and(|section| section.fallback)
    {
        let _ = writeln!(out, "nothing in {first} for {:?}", report.query);
    }
    for section in &report.sections {
        if section.fallback {
            let _ = writeln!(out, "results from {}:", section.display_name);
        } else {
            let _ = writeln!(out, "{}:", section.display_name);
        }
        for item in &section.tracks {
            let _ = writeln!(
                out,
                "  track   {} · {}",
                item.track.title, item.track.artist
            );
        }
        for artist in &section.artists {
            let _ = writeln!(out, "  artist  {}", artist.artist);
        }
        for item in &section.more {
            let _ = writeln!(
                out,
                "  more    {} · {}",
                item.track.title, item.track.artist
            );
        }
    }
    if report.sections.is_empty() && !report.query.trim().is_empty() {
        let _ = writeln!(out, "nothing found for {:?} anywhere", report.query);
    }
    // "Not found" and "could not ask" are different answers (K9).
    for failure in &report.failures {
        let first_line = failure
            .chain
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("STEP:"))
            .unwrap_or(&failure.chain);
        let _ = writeln!(
            out,
            "could not ask {} — STEP: {} — {}",
            failure.display_name, failure.stage, first_line
        );
    }
    out
}

/// Songs recommended from one source (D-087): each section with its
/// songs, why each is there, what went in, and what could not be asked.
pub fn recommend(report: &RecommendReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "for you, from {}:", report.display_name);
    if !report.top_picks.is_empty() {
        let _ = writeln!(out, "\ntop picks");
        for item in &report.top_picks {
            let _ = writeln!(
                out,
                "  {} · {}",
                item.item.track.title, item.item.track.artist
            );
        }
    }
    for section in &report.sections {
        let title = match &section.kind {
            SectionKind::BecauseYouListened { artist, title } => {
                format!("because you listened to {title} · {artist}")
            }
            SectionKind::Genre { genre } => format!("more {genre}"),
            SectionKind::FromArtistsYouLove => "from artists you love".to_owned(),
        };
        let _ = writeln!(out, "\n{title}");
        for item in &section.items {
            let why = item
                .reasons
                .first()
                .map_or("", |reason| reason_text(*reason));
            let _ = writeln!(
                out,
                "  {} · {}{}",
                item.item.track.title,
                item.item.track.artist,
                if why.is_empty() {
                    String::new()
                } else {
                    format!("  ({why})")
                }
            );
        }
    }
    let summary = &report.summary;
    if report.is_empty() {
        let _ = writeln!(
            out,
            "{}",
            if summary.listens_considered == 0 {
                "nothing yet — play or import a few songs first"
            } else {
                "nothing new to offer from this source right now"
            }
        );
    }
    let _ = writeln!(
        out,
        "\nread from {} listens ({} songs, {} with a genre){}",
        summary.listens_considered,
        summary.songs_in_taste,
        summary.songs_with_genre,
        if report.since_ms.is_some() {
            ", since you last reset recommendations"
        } else {
            ""
        }
    );
    if summary.left_out_dismissed + summary.left_out_recent > 0 {
        let _ = writeln!(
            out,
            "left out: {} marked not for me, {} played this week",
            summary.left_out_dismissed, summary.left_out_recent
        );
    }
    if summary.genres_learned_online > 0 {
        let _ = writeln!(
            out,
            "MusicBrainz named the genres of {} songs",
            summary.genres_learned_online
        );
    }
    // "Nothing to offer" and "could not ask" are different answers (K9).
    for failure in &report.failures {
        let first_line = failure
            .chain
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("STEP:"))
            .unwrap_or(&failure.chain);
        let _ = writeln!(
            out,
            "could not ask {} — STEP: {} — {}",
            failure.display_name, failure.stage, first_line
        );
    }
    out
}

const fn reason_text(reason: Reason) -> &'static str {
    match reason {
        Reason::ListenedTogether => "played alongside it",
        Reason::SimilarAtSource => "the source says it's alike",
        Reason::SharedGenre => "your genres",
        Reason::ArtistYouLove => "an artist you love",
        Reason::NotHeardYet => "new to you",
    }
}

/// A song marked "not for me".
pub fn recommend_dismiss(report: &DismissReport) -> String {
    if report.newly_marked {
        format!(
            "{} · {} won't be recommended again until you reset recommendations\n",
            report.title, report.artist
        )
    } else {
        format!("{} · {} was marked already\n", report.title, report.artist)
    }
}

/// Recommendations started over.
pub fn recommend_reset(report: &RecommendResetReport) -> String {
    format!(
        "recommendations start over from now\n\
         forgotten: {} not-for-me marks, {} genres learned online\n\
         your history and stats are untouched\n",
        report.forgot.dismissed, report.forgot.learned_genres
    )
}

/// The plugin list.
pub fn plugin_list(report: &PluginListReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if report.plugins.is_empty() {
        let _ = writeln!(out, "no plugins installed");
        return out;
    }
    for entry in &report.plugins {
        let _ = writeln!(
            out,
            "{:<14} {:<20} {}",
            entry.name,
            truncate(entry.display_name.as_deref().unwrap_or("—"), 20),
            entry.status_text()
        );
        if !entry.permissions.is_empty() {
            let _ = writeln!(out, "{:<14} requests: {}", "", entry.permissions.describe());
        }
        // The artifacts the engine will install go **on a separate line** (D-055):
        // the engine does the downloading, not the plugin, so they do not mix with
        // the plugin's permission list. If they did, the user would read "this
        // plugin connects there", when it is the engine that connects.
        for requirement in &entry.requires {
            let _ = writeln!(
                out,
                "{:<14} engine  : {} {} ({}) — {}",
                "",
                requirement.name,
                requirement.version,
                requirement.platform,
                requirement.state.describe()
            );
        }
    }
    let s = &report.summary;
    let _ = writeln!(
        out,
        "\n{}: {} ready, {} awaiting install, {} awaiting consent, {} disabled, {} incompatible, {} broken",
        count(s.discovered, "plugin"),
        s.ready,
        s.needs_install,
        s.awaiting_approval,
        s.disabled,
        s.incompatible,
        s.broken
    );
    let _ = writeln!(out, "{}", enforcement_notice(report.permissions_enforced));
    out
}

/// The result of the consent command.
pub fn plugin_consent(report: &PluginConsentReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "plugin     : {}", report.name);
    let _ = writeln!(out, "command    : {}", report.action);
    let _ = writeln!(out, "permissions: {}", report.permissions.describe());
    // The artifacts the engine will download go on a separate line (D-055): the
    // engine downloads them, not the plugin, so they do not mix with the
    // plugin's permission list.
    for requirement in &report.requires {
        match requirement.asset_for(&report.platform) {
            Some(asset) => {
                let _ = writeln!(
                    out,
                    "engine     : {} {} ({}) ← {}",
                    requirement.name, requirement.version, report.platform, asset.url
                );
                let _ = writeln!(out, "{:<11}  sha256 {}", "", asset.sha256);
            }
            None => {
                let _ = writeln!(
                    out,
                    "engine     : {} {} — no release for this platform ({})",
                    requirement.name, requirement.version, report.platform
                );
            }
        }
    }
    if !report.requires.is_empty() {
        let _ = writeln!(
            out,
            "{:<11}  to install: `headshell plugin install {}`",
            "", report.name
        );
    }
    let _ = writeln!(out, "status     : {}", report.status.describe());
    let _ = writeln!(out, "{}", enforcement_notice(report.permissions_enforced));
    out
}

/// The result of the install command (D-055, D-071).
pub fn plugin_install(report: &PluginInstallReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let name = &report.report.plugin;
    let _ = writeln!(out, "plugin     : {name}");
    match &report.fetched {
        Some(fetched) => {
            let _ = writeln!(
                out,
                "catalog    : {} downloaded ← {}",
                fetched.version, fetched.index
            );
            for file in &fetched.files {
                let _ = writeln!(
                    out,
                    "{:<11}  {:<12} sha256 {} verified",
                    "",
                    file.path,
                    short(&file.sha256)
                );
            }
        }
        // "The catalog was not read" and "it was not in the catalog" are separate
        // answers (K9).
        None => {
            let _ = writeln!(
                out,
                "catalog    : not read — the plugin was already on disk"
            );
        }
    }
    let _ = writeln!(out, "platform   : {}", report.platform);

    if report.declared == 0 {
        // "It asks for nothing" and "I did not look" are separate answers (K9).
        let _ = writeln!(out, "artifact   : none — this plugin asks for nothing");
    }
    for (name, outcome) in &report.report.outcomes {
        let _ = writeln!(out, "artifact   : {name} — {}", outcome.describe());
    }
    let _ = writeln!(out, "permissions: {}", report.permissions.describe());
    let _ = writeln!(
        out,
        "consent    : {}",
        report.consent.describe().replace("<name>", name)
    );
    let _ = writeln!(
        out,
        "\nstatus     : {}",
        if !report.ready {
            "incomplete — the plugin will not load like this"
        } else if report.consent.is_approved() {
            "ready — the plugin can run"
        } else {
            "installed — it runs once approved"
        }
    );
    out
}

/// The catalog: what can be installed, what is installed, what can be updated
/// (D-071).
pub fn plugin_catalog(report: &PluginCatalogReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "catalog: {}\n", report.index);
    if report.plugins.is_empty() {
        let _ = writeln!(out, "the catalog is empty");
    }
    for plugin in &report.plugins {
        let state = match &plugin.problem {
            Some(problem) => format!("CANNOT BE INSTALLED — {problem}"),
            None => plugin.installed.describe(),
        };
        let _ = writeln!(
            out,
            "{:<14} {:<20} {:<9} {}",
            plugin.name,
            truncate(plugin.display_name.as_deref().unwrap_or("—"), 20),
            plugin.version.as_deref().unwrap_or("—"),
            state
        );
        if let Some(description) = &plugin.description {
            let _ = writeln!(out, "{:<14} {}", "", truncate(description, 100));
        }
        if plugin.problem.is_none() {
            let _ = writeln!(
                out,
                "{:<14} requests: {}",
                "",
                plugin.permissions.describe()
            );
            // The tools the engine will install go on a line separate from the network
            // permission (D-055).
            for requirement in &plugin.requires {
                let line = if requirement.asset_for(&report.platform).is_some() {
                    format!(
                        "{} {} ({})",
                        requirement.name, requirement.version, report.platform
                    )
                } else {
                    format!(
                        "{} {} — no release for this platform ({})",
                        requirement.name, requirement.version, report.platform
                    )
                };
                let _ = writeln!(out, "{:<14} engine  : {line}", "");
            }
        }
    }
    for name in &report.delisted {
        let _ = writeln!(
            out,
            "\n{name}: pulled from the catalog but installed on this machine — to remove it \
             `headshell plugin remove {name}`"
        );
    }
    let s = &report.summary;
    let _ = writeln!(
        out,
        "\n{}: {} installable, {} installed, {} with updates, {} cannot be installed",
        count(s.listed, "plugin"),
        s.installable,
        s.installed,
        s.updates,
        s.problems
    );
    let _ = writeln!(
        out,
        "to install `headshell plugin install <name>` · to update `headshell plugin update`"
    );
    out
}

/// The update result (D-071).
pub fn plugin_update(report: &PluginUpdateReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "catalog: {}\n", report.index);
    if report.plugins.is_empty() {
        let _ = writeln!(
            out,
            "no plugins installed from the catalog — to see what there is: `headshell plugin catalog`"
        );
        return out;
    }
    for plugin in &report.plugins {
        let _ = writeln!(out, "{:<14} {}", plugin.name, plugin.outcome.describe());
        if let UpdateOutcome::Updated {
            permissions_added,
            tools_changed,
            ..
        } = &plugin.outcome
        {
            if !permissions_added.is_empty() {
                let _ = writeln!(
                    out,
                    "{:<14} asks for new permissions: {} — `headshell plugin approve {}`",
                    "",
                    permissions_added.describe(),
                    plugin.name
                );
            }
            // A tool change does not ask for consent (D-071) but it is said: a separate
            // program must not change silently.
            for change in tools_changed {
                let _ = writeln!(
                    out,
                    "{:<14} tool changed: {} (no consent asked)",
                    "",
                    change.describe()
                );
            }
        }
        for (name, outcome) in &plugin.tools {
            let _ = writeln!(out, "{:<14} artifact: {name} — {}", "", outcome.describe());
        }
        if let Some(error) = &plugin.tools_error {
            let _ = writeln!(out, "{:<14} COULD NOT INSTALL THE TOOLS — {error}", "");
        }
        if let Some(consent) = &plugin.consent
            && !consent.is_approved()
        {
            let _ = writeln!(
                out,
                "{:<14} consent: {}",
                "",
                consent.describe().replace("<name>", &plugin.name)
            );
        }
    }
    let s = &report.summary;
    let _ = writeln!(
        out,
        "\n{}: {} updated, {} up to date, {} skipped, {} failed",
        count(s.checked, "plugin"),
        s.updated,
        s.current,
        s.skipped,
        s.failed
    );
    out
}

/// The removal result (D-071).
pub fn plugin_remove(report: &PluginRemoveReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "removed   : {} ({})",
        report.name,
        report.removed.path.display()
    );
    if let Some(target) = &report.removed.link_target {
        let _ = writeln!(
            out,
            "            it was a link; its target was not touched: {}",
            target.display()
        );
    }
    let _ = writeln!(
        out,
        "consent   : {}",
        if report.consent_forgotten {
            "forgotten — if reinstalled it is asked from scratch"
        } else {
            "there was no record"
        }
    );
    if !report.kept_secrets.is_empty() {
        let _ = writeln!(
            out,
            "secrets   : left in plugin:{}: {} — to delete them `headshell secret remove \
             plugin:{} <key>`",
            report.name,
            report.kept_secrets.join(", "),
            report.name
        );
    }
    let _ = writeln!(
        out,
        "note      : the tools the engine installed are shared between plugins; not deleted"
    );
    out
}

/// Producing the index (catalog maintenance, D-071).
pub fn plugin_index(report: &PluginIndexReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "index   : {} — {}",
        report.path.display(),
        if report.written {
            "written"
        } else if report.up_to_date {
            "already up to date"
        } else {
            "not written"
        }
    );
    let _ = writeln!(out, "template: {}", report.url_template);
    for plugin in &report.plugins {
        let _ = writeln!(out, "\n{} {}", plugin.name, plugin.version);
        for file in &plugin.files {
            let _ = writeln!(
                out,
                "  {:<12} sha256 {}  {}",
                file.path,
                short(&file.sha256),
                file.url
            );
        }
    }
    let _ = writeln!(out, "\n{}", count(report.plugins.len(), "plugin"));
    out
}

/// The first 12 digits of a hash — 64 digits are unreadable in a terminal.
fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

/// A note saying how much of the permissions is enforced (D-040 → D-069).
///
/// It shows up in every list and consent output: the user must not trust a
/// protection that does not exist, and must know the limits of the one that
/// does. In api 2 the plugin itself is confined; the tools the engine installs
/// (yt-dlp) are not.
fn enforcement_notice(enforced: bool) -> &'static str {
    if enforced {
        "note: permissions are enforced — the plugin can only connect to the hosts it declares \
         and cannot access the file system.\n\
         the tools the engine installs (like yt-dlp) are separate programs and are outside this boundary."
    } else {
        "note: permissions are not enforced — the declaration is a contract, not a firewall.\n\
         the plugin runs with all of your privileges."
    }
}

/// The key names in the secret store.
pub fn secret_list(report: &SecretListReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if report.namespaces.is_empty() {
        let _ = writeln!(out, "no secrets stored");
        return out;
    }
    for (namespace, keys) in &report.namespaces {
        let _ = writeln!(out, "{namespace}: {}", keys.join(", "));
    }
    let _ = writeln!(out, "\nvalues are not shown; only key names.");
    out
}

/// The result of writing/removing a secret.
pub fn secret_write(report: &SecretWriteReport) -> String {
    let verb = if report.action == "remove" {
        "removed"
    } else {
        "written"
    };
    format!("{} / {} {verb}\n", report.namespace, report.key)
}

/// A provider test.
pub fn provider_test(report: &ProviderTestReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "provider  : {}", report.info.id);
    let _ = writeln!(out, "name      : {}", report.info.display_name);
    let _ = writeln!(out, "capability: {}", report.info.capabilities);
    // We do not say "UNREACHABLE": `reachable == false` has two causes, and
    // one is "the server is up but refused the credentials". The heading picks
    // the word that covers both; the `note` below says which one (D-023).
    let _ = writeln!(
        out,
        "state     : {}",
        if report.health.reachable {
            "available"
        } else {
            "UNAVAILABLE"
        }
    );
    if let Some(count) = report.health.track_count {
        let _ = writeln!(out, "tracks    : {count}");
    }
    if let Some(detail) = &report.health.detail {
        let _ = writeln!(out, "note      : {detail}");
    }
    out
}

/// The scan summary.
pub fn scan(report: &ScanReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if report.dirs.is_empty() {
        let _ = writeln!(
            out,
            "no music directory found — set HEADSHELL_MUSIC_DIRS\n\
             (example: HEADSHELL_MUSIC_DIRS=~/Music headshell provider scan)"
        );
        return out;
    }
    if !report.scanned {
        // We **say** it was skipped: silently doing nothing would make the
        // user think we scanned.
        let _ = writeln!(out, "scan skipped ({})", report.reason);
        return out;
    }
    for dir in &report.dirs {
        let _ = writeln!(out, "scanned: {}", dir.display());
    }
    let s = &report.summary;
    let _ = writeln!(out, "  files seen    : {}", s.files_seen);
    let _ = writeln!(out, "  audio files   : {}", s.audio_files);
    let _ = writeln!(out, "  indexed       : {}", s.indexed);
    if s.unchanged > 0 {
        let _ = writeln!(out, "  unchanged     : {} (not read again)", s.unchanged);
    }
    if s.tag_fallback > 0 {
        let _ = writeln!(
            out,
            "  untagged      : {} (from the file name)",
            s.tag_fallback
        );
    }
    if s.failed > 0 {
        let _ = writeln!(out, "  unreadable    : {}", s.failed);
    }
    if s.unreadable_dirs > 0 {
        let _ = writeln!(out, "  skipped dirs  : {}", s.unreadable_dirs);
    }

    let w = &report.write;
    let _ = writeln!(
        out,
        "catalog: new {} · updated {} · dropped {}",
        w.inserted, w.updated, w.removed
    );
    out
}

/// The result of registering a server.
///
/// The token or key **is not printed**: the summary the core gives does not
/// carry them anyway, and there is nowhere here to read them back from.
pub fn server_add(report: &ServerAddReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let s = &report.server;
    let _ = writeln!(out, "registered: {} ({})", s.id, s.kind);
    let _ = writeln!(out, "address   : {}", s.url);
    let _ = writeln!(out, "user      : {}", s.username);
    let _ = writeln!(out, "auth      : {}", s.auth);
    let _ = writeln!(
        out,
        "verified  : {}",
        if report.verified {
            "connected to the server"
        } else {
            "skipped (--no-verify)"
        }
    );
    // Observations are not swallowed: weak entropy or a field that could not
    // be learned are things the user needs to see (K9).
    for note in &report.notes {
        let _ = writeln!(out, "note      : {note}");
    }
    let _ = writeln!(out, "\ntest it: headshell provider test {}", s.id);
    out
}

/// Deleting a server record.
pub fn server_remove(report: &ServerRemoveReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "removed: {} (records left: {})",
        report.id, report.remaining
    );
    out
}

/// The registered remote servers.
pub fn server_list(report: &ServerListReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if report.servers.is_empty() {
        let _ = writeln!(
            out,
            "no registered remote servers\n\
             (example: headshell provider add subsonic --url https://music.home --user you)"
        );
        return out;
    }
    for server in &report.servers {
        let _ = writeln!(
            out,
            "{:<12} {:<9} {:<34} {:<14} {}",
            server.id,
            server.kind,
            truncate(&server.url, 34),
            truncate(&server.username, 14),
            server.auth
        );
    }
    // "Where was it written?" is a diagnostic question; the user looks at it
    // while backing up the file or fixing it by hand.
    let _ = writeln!(out, "\nrecord file: {}", report.path.display());
    out
}

/// The play result.
pub fn play(report: &PlayReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} queued{}",
        count(report.queued.len(), "track"),
        if report.played { "" } else { " (not played)" }
    );
    for (index, item) in report.queued.iter().enumerate().take(10) {
        let _ = writeln!(
            out,
            "  {:>2}. {}",
            index + 1,
            truncate(&item.track.display_name(), 56)
        );
    }
    if report.queued.len() > 10 {
        let _ = writeln!(
            out,
            "  … and {} more",
            count(report.queued.len() - 10, "track")
        );
    }
    if report.video_only > 0 {
        let _ = writeln!(
            out,
            "{} only a video: {} in the app's window, not here",
            count(report.video_only, "track"),
            if report.played {
                "passed by — it plays"
            } else {
                "it plays"
            }
        );
    }
    if report.played {
        let _ = writeln!(out, "listens recorded: {}", report.listens_recorded);
    }
    out
}

/// Cuts text to the given width (by Unicode character count).
/// What `headshell prefetch` made ready, and how long each track took
/// (D-077).
pub fn prefetch(report: &PrefetchReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "read ahead · {}", report.subject);
    for item in &report.items {
        let reason = match item.reason {
            PlanReason::First => "first",
            PlanReason::Next => "next",
            PlanReason::AfterNext => "after next",
            PlanReason::Again => "again",
            PlanReason::Likely => "likely",
        };
        let (mark, detail) = match &item.status {
            PrefetchStatus::Ready {
                resolve_ms,
                head_bytes,
                total_bytes,
                no_head,
            } => {
                let head = match (head_bytes, total_bytes, no_head) {
                    (Some(head), Some(total), _) => {
                        format!("head {} of {}", size(*head), size(*total))
                    }
                    (_, _, Some(why)) => format!("no head: {why}"),
                    _ => "no head".to_owned(),
                };
                (
                    "✓",
                    format!("address {:.2} s · {head}", *resolve_ms as f64 / 1000.0),
                )
            }
            PrefetchStatus::Failed { chain } => ("✗", chain.clone()),
            PrefetchStatus::Waiting => ("·", "waiting".to_owned()),
            PrefetchStatus::Resolving => ("…", "resolving".to_owned()),
        };
        let _ = writeln!(
            out,
            "  {mark} {reason:<10} {:>2}. {} — {}",
            item.index + 1,
            truncate(&item.artist, 28),
            truncate(&item.title, 40),
        );
        if let Some(why) = &item.why {
            let _ = writeln!(out, "                  why: {why}");
        }
        for line in detail.lines() {
            let _ = writeln!(out, "                  {line}");
        }
    }
    let summary = &report.summary;
    let _ = writeln!(
        out,
        "{} · {} ready ({} with a head) · {} failed",
        count(summary.planned, "track"),
        summary.ready,
        summary.with_head,
        summary.failed,
    );
    out
}

/// A byte count as KB or MB.
fn size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "a size shown to one decimal; the loss is invisible there"
    )]
    let bytes = bytes as f64;
    if bytes < KB * KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{:.1} MB", bytes / (KB * KB))
    }
}

fn truncate(input: &str, width: usize) -> String {
    if input.chars().count() <= width {
        return input.to_owned();
    }
    let kept: String = input.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_take_the_singular_only_for_one() {
        assert_eq!(count(0_usize, "play"), "0 plays");
        assert_eq!(count(1_usize, "play"), "1 play");
        assert_eq!(count(2_u64, "hour"), "2 hours");
        assert_eq!(
            plays(1).len(),
            plays(2).len(),
            "the column must stay aligned"
        );
    }

    #[test]
    fn duration_switches_to_hours() {
        assert_eq!(duration(90_000), "1m");
        assert_eq!(duration(3_600_000), "1h 0m");
        assert_eq!(duration(5_400_000), "1h 30m");
    }

    #[test]
    fn a_length_difference_and_a_shift_read_as_words() {
        assert_eq!(length_difference(1_234), "1.2 s longer");
        assert_eq!(length_difference(-7_000), "7.0 s shorter");
        assert_eq!(length_difference(0), "of the same length");
        assert_eq!(shift_text(500), "0.5 s sooner");
        assert_eq!(shift_text(-1_500), "1.5 s later");
    }

    #[test]
    fn truncate_respects_unicode_boundaries() {
        assert_eq!(truncate("Şebnem", 10), "Şebnem");
        assert_eq!(truncate("Şebnem Ferah", 7), "Şebnem…");
    }
}
