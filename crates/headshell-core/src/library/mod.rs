//! The local library: SQLite + FTS5.
//!
//! Access to the store is behind the [`ListenStore`] trait; tests can use a
//! fake store, and the rest of the core never sees SQLite.

mod schema;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::identity::normalize::track_key;
use crate::identity::{Resolution, ResolveMethod};
use crate::ids::{CanonicalId, Isrc, ProviderId, ProviderTrackId};
use crate::model::{ExportKind, Listen, ListenSource, PlayRule, TrackRef};

/// A summary of writing listens.
///
/// Importing the same export twice must not inflate the numbers; how many
/// already existed shows up here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteSummary {
    /// The number of records offered for writing.
    pub offered: usize,
    /// The number of newly added listens.
    pub inserted: usize,
    /// Skipped because they already existed (same track + time + duration).
    pub duplicates: usize,
    /// The number of newly created track rows.
    pub new_tracks: usize,
}

impl WriteSummary {
    /// Copies the counters into the diagnostics recorder.
    pub fn record_into(&self, recorder: &mut crate::diag::Recorder) {
        let n = |v: usize| i64::try_from(v).unwrap_or(i64::MAX);
        recorder.set("library.offered", n(self.offered));
        recorder.set("library.inserted", n(self.inserted));
        recorder.set("library.duplicates", n(self.duplicates));
        recorder.set("library.new_tracks", n(self.new_tracks));
    }
}

/// A row returned by a search.
///
/// The count field deliberately carries the name `play_count`: plays that
/// pass [`PlayRule`]. The raw event count is not here but in
/// [`SearchOutcome::listen_events`] — so the user is not shown two different
/// "plays" (D-008).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub canonical_id: Option<CanonicalId>,
    /// The number of plays that pass [`PlayRule`] — the same calculation as
    /// `stats`.
    pub play_count: usize,
    /// The total duration of the counted plays only.
    pub ms_played: u64,
}

/// The full result of a search.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    /// The **raw** number of listen events of the matching tracks (no threshold
    /// applied).
    ///
    /// It does not reach the user surface; it is written to the `diag` counters.
    /// The difference from `play_count` is the answer to "how many plays fell
    /// below the threshold".
    pub listen_events: usize,
}

/// How a track, and its artist, were listened to in the history (D-077):
/// what reading the queue ahead weighs its third track with.
///
/// "Skips" are the listens that stopped before [`PlayRule`] counted them —
/// the same line `stats` draws, from the other side. The history keeps them
/// when an export carries them (Spotify's does); the app's own playback
/// writes only counted plays.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListeningProfile {
    /// Plays of the track that the rule counts.
    pub plays: u64,
    /// Listens of the track that stopped before it.
    pub skips: u64,
    /// Plays of the artist's tracks that the rule counts.
    pub artist_plays: u64,
    /// Listens of the artist's tracks that stopped before it.
    pub artist_skips: u64,
}

/// The listen store. SQLite is its only real implementation; tests use a
/// fake.
pub trait ListenStore {
    /// Writes listens; skips duplicates.
    ///
    /// # Errors
    /// On database errors.
    fn insert_listens(&mut self, listens: &[Listen]) -> Result<WriteSummary>;

    /// Reads all listens (in time order).
    ///
    /// # Errors
    /// On database errors.
    fn all_listens(&self) -> Result<Vec<Listen>>;

    /// Full-text search.
    ///
    /// `rule` decides which listens count as a "play"; `stats` uses the same rule
    /// (D-008).
    ///
    /// # Errors
    /// If the query is invalid, or on a database error.
    fn search(&self, query: &str, limit: usize, rule: PlayRule) -> Result<SearchOutcome>;

    /// Records a track's resolution result.
    ///
    /// # Errors
    /// On database errors.
    fn set_resolution(&mut self, norm_key: &str, resolution: &Resolution) -> Result<()>;
}

/// A track in a provider's catalog (a persistent index row).
///
/// Different from [`SearchHit`]: that one is "what you listened to", this one
/// "what you can play".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTrack {
    pub id: ProviderTrackId,
    pub track: TrackRef,
    /// Whether the metadata came from the tags (or was derived from the file
    /// name).
    pub from_tags: bool,
    /// The file's last modification time (ms) — if unchanged, it is not read
    /// again.
    pub mtime_ms: Option<i64>,
    /// The genres the file's tags name (D-087). Written to `track_genres`
    /// when the row is written; a row read back carries none — they are
    /// asked for by key ([`SqliteLibrary::genres_of`]).
    pub genres: Vec<String>,
}

/// A summary of a persistent catalog write.
///
/// K9: how many rows were added, how many updated and how many **removed**
/// (the file no longer exists) are shown separately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogWriteSummary {
    pub inserted: usize,
    pub updated: usize,
    /// Rows dropped from the catalog because they no longer exist in the source.
    pub removed: usize,
    pub unchanged: usize,
}

/// The persistent store of a provider's catalog.
///
/// A trait separate from `ListenStore`: the listening history and the
/// playable catalog have different lifetimes. History is never deleted; the
/// catalog mirrors the source.
pub trait CatalogStore {
    /// **Replaces** a provider's catalog entirely.
    ///
    /// Rows not in the given list are deleted — the catalog must be a mirror of
    /// the source, otherwise deleted files keep showing up in searches.
    ///
    /// # Errors
    /// On database errors.
    fn replace_catalog(
        &mut self,
        provider: &ProviderId,
        tracks: &[CatalogTrack],
    ) -> Result<CatalogWriteSummary>;

    /// Full-text search in the catalog.
    ///
    /// # Errors
    /// On database errors.
    fn search_catalog(&self, query: &str, limit: usize) -> Result<Vec<CatalogTrack>>;

    /// The number of tracks a provider has in the catalog.
    ///
    /// # Errors
    /// On database errors.
    fn catalog_len(&self, provider: &ProviderId) -> Result<usize>;

    /// A single catalog row, by provider id.
    ///
    /// # Errors
    /// On database errors.
    fn catalog_get(&self, id: &ProviderTrackId) -> Result<Option<CatalogTrack>>;

    /// A provider's known file stamps: `provider_ref → mtime_ms`.
    ///
    /// The scan reads this and does not re-read the tags of unchanged files — in
    /// a large library that is the expensive part of a scan.
    ///
    /// # Errors
    /// On database errors.
    fn catalog_stamps(
        &self,
        provider: &ProviderId,
    ) -> Result<std::collections::HashMap<String, i64>>;

    /// **When** this provider's catalog was last scanned (ms).
    ///
    /// `None` if never scanned — not zero: "I never looked" and "I looked in
    /// 1970" are different things, and the latter would make everything look
    /// stale (D-025).
    ///
    /// # Errors
    /// If the query fails.
    fn last_scanned_at_ms(&self, provider: &ProviderId) -> Result<Option<i64>>;
}

/// An SQLite-backed library.
pub struct SqliteLibrary {
    conn: rusqlite::Connection,
    path: PathBuf,
}

fn db_err(stage: Stage) -> impl Fn(rusqlite::Error) -> Error {
    move |source| Error::new(stage, ErrorKind::Database { source })
}

/// The SQLite counterpart of [`PlayRule`].
///
/// The rule's **definition** is in [`PlayRule::counts`]; this translates it
/// into SQL in one single place — no threshold is written into queries by
/// hand. The two drifting apart was exactly the bug in D-008, which is why
/// their agreement is locked in by the `sql_play_rule_agrees_with_rust` test.
///
/// The table aliases are fixed: `l` = `listens`, `t` = `tracks`.
fn play_predicate_sql(rule: PlayRule) -> String {
    format!(
        "(l.ms_played >= {min} \
          OR (t.duration_ms IS NOT NULL AND t.duration_ms > 0 \
              AND l.ms_played * 2 >= t.duration_ms))",
        min = rule.min_ms_played
    )
}

impl SqliteLibrary {
    /// Opens the database (creating it if missing) and updates the schema.
    ///
    /// # Errors
    /// If the file cannot be opened or a migration fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|source| crate::error::io_err(Stage::LibraryOpen, parent, source))?;
            }
        }
        let conn = rusqlite::Connection::open(&path).map_err(db_err(Stage::LibraryOpen))?;
        let mut library = Self { conn, path };
        library.configure()?;
        library.migrate()?;
        Ok(library)
    }

    /// A temporary in-memory library — for tests and `--dry-run`.
    ///
    /// # Errors
    /// If the schema cannot be set up.
    pub fn open_in_memory() -> Result<Self> {
        let conn = rusqlite::Connection::open_in_memory().map_err(db_err(Stage::LibraryOpen))?;
        let mut library = Self {
            conn,
            path: PathBuf::from(":memory:"),
        };
        library.configure()?;
        library.migrate()?;
        Ok(library)
    }

    /// The path of the database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How a track and its artist were listened to (D-077). A track the
    /// history has never seen is all zeros — no history, not an error.
    ///
    /// The artist is found by its part of the grouping key: every key that
    /// begins with `artist\u{1}`, a range the key's index answers.
    ///
    /// # Errors
    /// If the database cannot be read.
    pub fn listening_profile(
        &self,
        artist: &str,
        title: &str,
        rule: PlayRule,
    ) -> Result<ListeningProfile> {
        let play = play_predicate_sql(rule);
        let counts = |filter: &str, low: &str, high: Option<&str>| -> Result<(u64, u64)> {
            let sql = format!(
                "SELECT COALESCE(SUM(CASE WHEN {play} THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN {play} THEN 0 ELSE 1 END), 0)
                 FROM listens l JOIN tracks t ON t.id = l.track_id
                 WHERE {filter}"
            );
            let row = |row: &rusqlite::Row<'_>| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?));
            let (plays, skips) = match high {
                Some(high) => self.conn.query_row(&sql, [low, high], row),
                None => self.conn.query_row(&sql, [low], row),
            }
            .map_err(db_err(Stage::LibraryQuery))?;
            Ok((
                u64::try_from(plays).unwrap_or(0),
                u64::try_from(skips).unwrap_or(0),
            ))
        };
        let key = track_key(artist, title);
        let (plays, skips) = counts("t.norm_key = ?1", &key, None)?;
        let artist_key = crate::identity::normalize::normalize_artist(artist);
        let (artist_plays, artist_skips) = counts(
            "t.norm_key >= ?1 AND t.norm_key < ?2",
            &format!("{artist_key}\u{1}"),
            Some(&format!("{artist_key}\u{2}")),
        )?;
        Ok(ListeningProfile {
            plays,
            skips,
            artist_plays,
            artist_skips,
        })
    }

    fn configure(&self) -> Result<()> {
        self.conn
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err(Stage::LibraryOpen))?;
        self.conn
            .pragma_update(None, "foreign_keys", true)
            .map_err(db_err(Stage::LibraryOpen))?;
        Ok(())
    }

    fn migrate(&mut self) -> Result<()> {
        let current: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_err(Stage::LibraryOpen))?;
        let current = usize::try_from(current).unwrap_or(0);

        for (index, migration) in schema::MIGRATIONS.iter().enumerate().skip(current) {
            let version = index + 1;
            tracing::info!(version, "applying schema migration");
            self.conn
                .execute_batch(migration)
                .map_err(db_err(Stage::LibraryOpen))?;
            self.conn
                .pragma_update(None, "user_version", i64::try_from(version).unwrap_or(0))
                .map_err(db_err(Stage::LibraryOpen))?;
        }
        Ok(())
    }

    /// Finds or creates the track row; returns `(id, is_new)`.
    fn upsert_track(tx: &rusqlite::Transaction<'_>, track: &TrackRef) -> Result<(i64, bool)> {
        let key = track_key(&track.artist, &track.title);
        // SQLite integers are signed; we narrow the duration to i64.
        let duration_ms = track.duration_ms.and_then(|ms| i64::try_from(ms).ok());
        let existing: Option<i64> = tx
            .query_row("SELECT id FROM tracks WHERE norm_key = ?1", [&key], |row| {
                row.get(0)
            })
            .optional_row()?;

        if let Some(id) = existing {
            // If richer metadata arrived, fill in the empty fields; do not overwrite
            // what is there.
            tx.execute(
                "UPDATE tracks SET
                     album             = COALESCE(album, ?2),
                     duration_ms       = COALESCE(duration_ms, ?3),
                     isrc              = COALESCE(isrc, ?4),
                     provider          = COALESCE(provider, ?5),
                     provider_track_id = COALESCE(provider_track_id, ?6)
                 WHERE id = ?1",
                rusqlite::params![
                    id,
                    track.album,
                    duration_ms,
                    track.isrc.as_ref().map(Isrc::as_str),
                    track
                        .provider_track_id
                        .as_ref()
                        .map(|p| p.provider.as_str()),
                    track.provider_track_id.as_ref().map(|p| p.id.as_str()),
                ],
            )
            .map_err(db_err(Stage::LibraryWrite))?;
            return Ok((id, false));
        }

        tx.execute(
            "INSERT INTO tracks
                 (norm_key, artist, title, album, duration_ms, isrc, provider, provider_track_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                key,
                track.artist,
                track.title,
                track.album,
                duration_ms,
                track.isrc.as_ref().map(Isrc::as_str),
                track
                    .provider_track_id
                    .as_ref()
                    .map(|p| p.provider.as_str()),
                track.provider_track_id.as_ref().map(|p| p.id.as_str()),
            ],
        )
        .map_err(db_err(Stage::LibraryWrite))?;
        Ok((tx.last_insert_rowid(), true))
    }
}

/// Reduces `query_row`'s "no row" case to an `Option` without turning it
/// into an error.
trait OptionalRow<T> {
    fn optional_row(self) -> Result<Option<T>>;
}

impl<T> OptionalRow<T> for std::result::Result<T, rusqlite::Error> {
    fn optional_row(self) -> Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(source) => Err(Error::new(
                Stage::LibraryQuery,
                ErrorKind::Database { source },
            )),
        }
    }
}

impl ListenStore for SqliteLibrary {
    fn insert_listens(&mut self, listens: &[Listen]) -> Result<WriteSummary> {
        let tx = self
            .conn
            .transaction()
            .map_err(db_err(Stage::LibraryWrite))?;
        let mut summary = WriteSummary {
            offered: listens.len(),
            ..WriteSummary::default()
        };

        for listen in listens {
            let (track_id, is_new) = Self::upsert_track(&tx, &listen.track)?;
            if is_new {
                summary.new_tracks += 1;
            }
            let (source_kind, source_ref) = match &listen.source {
                ListenSource::Import { export } => ("import", export.as_str().to_owned()),
                ListenSource::Playback { provider } => ("playback", provider.as_str().to_owned()),
            };
            let changed = tx
                .execute(
                    "INSERT OR IGNORE INTO listens
                         (track_id, played_at, ms_played, source_kind, source_ref)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        track_id,
                        listen.played_at.as_millisecond(),
                        i64::try_from(listen.ms_played).unwrap_or(i64::MAX),
                        source_kind,
                        source_ref,
                    ],
                )
                .map_err(db_err(Stage::LibraryWrite))?;
            if changed == 0 {
                summary.duplicates += 1;
            } else {
                summary.inserted += 1;
            }
        }

        tx.commit().map_err(db_err(Stage::LibraryWrite))?;
        tracing::info!(
            inserted = summary.inserted,
            duplicates = summary.duplicates,
            "written to the library"
        );
        Ok(summary)
    }

    fn all_listens(&self) -> Result<Vec<Listen>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.artist, t.title, t.album, t.duration_ms, t.isrc,
                        t.provider, t.provider_track_id, t.canonical_id,
                        l.played_at, l.ms_played, l.source_kind, l.source_ref
                 FROM listens l JOIN tracks t ON t.id = l.track_id
                 ORDER BY l.played_at",
            )
            .map_err(db_err(Stage::LibraryQuery))?;

        let rows = stmt
            .query_map([], |row| {
                let artist: String = row.get(0)?;
                let title: String = row.get(1)?;
                let album: Option<String> = row.get(2)?;
                let duration_ms: Option<i64> = row.get(3)?;
                let isrc: Option<String> = row.get(4)?;
                let provider: Option<String> = row.get(5)?;
                let provider_track_id: Option<String> = row.get(6)?;
                let canonical_id: Option<String> = row.get(7)?;
                let played_at_ms: i64 = row.get(8)?;
                let ms_played: i64 = row.get(9)?;
                let source_kind: String = row.get(10)?;
                let source_ref: Option<String> = row.get(11)?;
                Ok((
                    artist,
                    title,
                    album,
                    duration_ms,
                    isrc,
                    provider,
                    provider_track_id,
                    canonical_id,
                    played_at_ms,
                    ms_played,
                    source_kind,
                    source_ref,
                ))
            })
            .map_err(db_err(Stage::LibraryQuery))?;

        let mut listens = Vec::new();
        for row in rows {
            let (
                artist,
                title,
                album,
                duration_ms,
                isrc,
                provider,
                provider_track_id,
                canonical_id,
                played_at_ms,
                ms_played,
                source_kind,
                source_ref,
            ) = row.map_err(db_err(Stage::LibraryQuery))?;

            let played_at = jiff::Timestamp::from_millisecond(played_at_ms).map_err(|err| {
                Error::new(
                    Stage::LibraryQuery,
                    ErrorKind::InvalidInput {
                        detail: format!(
                            "could not read a timestamp from the store ({played_at_ms}): {err}"
                        ),
                    },
                )
            })?;

            let track = TrackRef::new(artist, title)
                .with_album(album)
                .with_duration_ms(duration_ms.and_then(|ms| u64::try_from(ms).ok()))
                .with_isrc(isrc.as_deref().and_then(Isrc::parse))
                .with_provider_track_id(match (provider, provider_track_id) {
                    (Some(p), Some(id)) => Some(ProviderTrackId::new(ProviderId::new(p), id)),
                    _ => None,
                });

            listens.push(Listen {
                track,
                played_at,
                ms_played: u64::try_from(ms_played).unwrap_or(0),
                source: decode_source(&source_kind, source_ref.as_deref()),
                canonical_id: canonical_id.map(CanonicalId::from_stored),
            });
        }
        Ok(listens)
    }

    fn search(&self, query: &str, limit: usize, rule: PlayRule) -> Result<SearchOutcome> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(Error::new(
                Stage::LibraryQuery,
                ErrorKind::InvalidInput {
                    detail: "the search query is empty".to_owned(),
                },
            ));
        }

        let counted = play_predicate_sql(rule);
        let sql = format!(
            // bm25() cannot be called in an aggregate (GROUP BY) context. Without
            // MATERIALIZED, SQLite flattens the subquery into the outer query and gives
            // the same error, so the matches are computed separately first.
            "WITH matches AS MATERIALIZED (
                 SELECT rowid AS track_id, bm25(tracks_fts) AS score
                 FROM tracks_fts WHERE tracks_fts MATCH ?1
             )
             SELECT t.artist, t.title, t.album, t.canonical_id,
                    COALESCE(SUM(CASE WHEN {counted} THEN 1 ELSE 0 END), 0) AS play_count,
                    COALESCE(SUM(CASE WHEN {counted} THEN l.ms_played ELSE 0 END), 0),
                    COUNT(l.id) AS listen_events
             FROM matches m
             JOIN tracks t ON t.id = m.track_id
             LEFT JOIN listens l ON l.track_id = t.id
             GROUP BY t.id
             ORDER BY play_count DESC, listen_events DESC, MIN(m.score)
             LIMIT ?2"
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(db_err(Stage::LibraryQuery))?;

        let pattern = fts_pattern(trimmed);
        let rows = stmt
            .query_map(
                rusqlite::params![pattern, i64::try_from(limit).unwrap_or(i64::MAX)],
                |row| {
                    let hit = SearchHit {
                        artist: row.get(0)?,
                        title: row.get(1)?,
                        album: row.get(2)?,
                        canonical_id: row
                            .get::<_, Option<String>>(3)?
                            .map(CanonicalId::from_stored),
                        play_count: usize::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                        ms_played: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
                    };
                    let listen_events = usize::try_from(row.get::<_, i64>(6)?).unwrap_or(0);
                    Ok((hit, listen_events))
                },
            )
            .map_err(db_err(Stage::LibraryQuery))?;

        let mut outcome = SearchOutcome::default();
        for row in rows {
            let (hit, listen_events) = row.map_err(db_err(Stage::LibraryQuery))?;
            outcome.listen_events += listen_events;
            outcome.hits.push(hit);
        }
        Ok(outcome)
    }

    fn set_resolution(&mut self, norm_key: &str, resolution: &Resolution) -> Result<()> {
        self.conn
            .execute(
                "UPDATE tracks
                 SET canonical_id = ?2, resolve_method = ?3, resolve_confidence = ?4
                 WHERE norm_key = ?1",
                rusqlite::params![
                    norm_key,
                    resolution.canonical_id.as_str(),
                    resolution.method.as_str(),
                    resolution.confidence,
                ],
            )
            .map_err(db_err(Stage::LibraryWrite))?;
        Ok(())
    }
}

fn decode_source(kind: &str, reference: Option<&str>) -> ListenSource {
    match (kind, reference) {
        ("playback", Some(provider)) => ListenSource::Playback {
            provider: ProviderId::new(provider),
        },
        (_, Some("spotify_account")) => ListenSource::Import {
            export: ExportKind::SpotifyAccount,
        },
        (_, Some("apple_music")) => ListenSource::Import {
            export: ExportKind::AppleMusic,
        },
        (_, Some("google_takeout")) => ListenSource::Import {
            export: ExportKind::GoogleTakeout,
        },
        _ => ListenSource::Import {
            export: ExportKind::SpotifyExtended,
        },
    }
}

/// Turns the user's query into a pattern FTS5 understands.
///
/// FTS5 syntax contains operators (`AND`, `"`, `*`); handing it raw user
/// input directly is both an error and a security risk. Every word is quoted
/// and `*` is added for prefix search.
fn fts_pattern(query: &str) -> String {
    query
        .split_whitespace()
        .map(|word| {
            let escaped = word.replace('"', "");
            format!("\"{escaped}\"*")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl CatalogStore for SqliteLibrary {
    fn replace_catalog(
        &mut self,
        provider: &ProviderId,
        tracks: &[CatalogTrack],
    ) -> Result<CatalogWriteSummary> {
        let now = jiff::Timestamp::now().as_millisecond();
        let tx = self
            .conn
            .transaction()
            .map_err(db_err(Stage::LibraryWrite))?;
        let mut summary = CatalogWriteSummary::default();

        // References seen in this round; the rest will be deleted.
        let mut seen: std::collections::HashSet<&str> =
            std::collections::HashSet::with_capacity(tracks.len());

        for entry in tracks {
            seen.insert(entry.id.id.as_str());
            let key = track_key(&entry.track.artist, &entry.track.title);
            let duration_ms = entry
                .track
                .duration_ms
                .and_then(|ms| i64::try_from(ms).ok());

            let existing: Option<(i64, Option<i64>)> = tx
                .query_row(
                    "SELECT id, mtime_ms FROM provider_tracks
                     WHERE provider = ?1 AND provider_ref = ?2",
                    rusqlite::params![provider.as_str(), entry.id.id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional_row()?;

            match existing {
                // The file has not changed: leave it alone. Skips the expensive part of the
                // scan.
                Some((_, mtime)) if mtime.is_some() && mtime == entry.mtime_ms => {
                    summary.unchanged += 1;
                }
                Some((id, _)) => {
                    tx.execute(
                        "UPDATE provider_tracks SET
                             norm_key = ?2, artist = ?3, title = ?4, album = ?5,
                             duration_ms = ?6, isrc = ?7, from_tags = ?8,
                             mtime_ms = ?9, scanned_at = ?10
                         WHERE id = ?1",
                        rusqlite::params![
                            id,
                            key,
                            entry.track.artist,
                            entry.track.title,
                            entry.track.album,
                            duration_ms,
                            entry.track.isrc.as_ref().map(Isrc::as_str),
                            i64::from(entry.from_tags),
                            entry.mtime_ms,
                            now,
                        ],
                    )
                    .map_err(db_err(Stage::LibraryWrite))?;
                    write_genres(&tx, &key, &entry.genres, GenreSource::Tag)?;
                    summary.updated += 1;
                }
                None => {
                    tx.execute(
                        "INSERT INTO provider_tracks
                             (provider, provider_ref, norm_key, artist, title, album,
                              duration_ms, isrc, from_tags, mtime_ms, scanned_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        rusqlite::params![
                            provider.as_str(),
                            entry.id.id.as_str(),
                            key,
                            entry.track.artist,
                            entry.track.title,
                            entry.track.album,
                            duration_ms,
                            entry.track.isrc.as_ref().map(Isrc::as_str),
                            i64::from(entry.from_tags),
                            entry.mtime_ms,
                            now,
                        ],
                    )
                    .map_err(db_err(Stage::LibraryWrite))?;
                    write_genres(&tx, &key, &entry.genres, GenreSource::Tag)?;
                    summary.inserted += 1;
                }
            }
        }

        // Drop what no longer exists in the source: the catalog is the source's
        // mirror. The **history** of a deleted file is not deleted; it stays in the
        // `listens` table.
        let stale: Vec<(i64, String)> = {
            let mut stmt = tx
                .prepare("SELECT id, provider_ref FROM provider_tracks WHERE provider = ?1")
                .map_err(db_err(Stage::LibraryQuery))?;
            let rows = stmt
                .query_map([provider.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(db_err(Stage::LibraryQuery))?;
            let mut out = Vec::new();
            for row in rows {
                let (id, reference): (i64, String) = row.map_err(db_err(Stage::LibraryQuery))?;
                if !seen.contains(reference.as_str()) {
                    out.push((id, reference));
                }
            }
            out
        };
        for (id, reference) in stale {
            tx.execute("DELETE FROM provider_tracks WHERE id = ?1", [id])
                .map_err(db_err(Stage::LibraryWrite))?;
            tracing::debug!(reference = %reference, "dropped from the catalog");
            summary.removed += 1;
        }

        tx.commit().map_err(db_err(Stage::LibraryWrite))?;
        Ok(summary)
    }

    fn search_catalog(&self, query: &str, limit: usize) -> Result<Vec<CatalogTrack>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT p.provider, p.provider_ref, p.artist, p.title, p.album,
                        p.duration_ms, p.isrc, p.from_tags, p.mtime_ms
                 FROM provider_tracks_fts f
                 JOIN provider_tracks p ON p.id = f.rowid
                 WHERE provider_tracks_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(db_err(Stage::LibraryQuery))?;

        let rows = stmt
            .query_map(
                rusqlite::params![fts_pattern(query), i64::try_from(limit).unwrap_or(i64::MAX)],
                catalog_row,
            )
            .map_err(db_err(Stage::LibraryQuery))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err(Stage::LibraryQuery))?);
        }
        Ok(out)
    }

    fn catalog_len(&self, provider: &ProviderId) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM provider_tracks WHERE provider = ?1",
                [provider.as_str()],
                |row| row.get(0),
            )
            .map_err(db_err(Stage::LibraryQuery))?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    fn catalog_get(&self, id: &ProviderTrackId) -> Result<Option<CatalogTrack>> {
        self.conn
            .query_row(
                "SELECT provider, provider_ref, artist, title, album,
                        duration_ms, isrc, from_tags, mtime_ms
                 FROM provider_tracks
                 WHERE provider = ?1 AND provider_ref = ?2",
                rusqlite::params![id.provider.as_str(), id.id.as_str()],
                catalog_row,
            )
            .optional_row()
    }

    fn catalog_stamps(
        &self,
        provider: &ProviderId,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT provider_ref, mtime_ms FROM provider_tracks
                 WHERE provider = ?1 AND mtime_ms IS NOT NULL",
            )
            .map_err(db_err(Stage::LibraryQuery))?;
        let rows = stmt
            .query_map([provider.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(db_err(Stage::LibraryQuery))?;

        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (reference, mtime) = row.map_err(db_err(Stage::LibraryQuery))?;
            out.insert(reference, mtime);
        }
        Ok(out)
    }

    fn last_scanned_at_ms(&self, provider: &ProviderId) -> Result<Option<i64>> {
        // `MAX`, not `MIN`: we want the time of the last scan. If the catalog is
        // empty the aggregate returns `NULL`, and that means "never scanned".
        self.conn
            .query_row(
                "SELECT MAX(scanned_at) FROM provider_tracks WHERE provider = ?1",
                [provider.as_str()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(db_err(Stage::LibraryQuery))
    }
}

/// Reads a catalog row. The column order must match the queries exactly.
fn catalog_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CatalogTrack> {
    let provider: String = row.get(0)?;
    let reference: String = row.get(1)?;
    let artist: String = row.get(2)?;
    let title: String = row.get(3)?;
    let album: Option<String> = row.get(4)?;
    let duration_ms: Option<i64> = row.get(5)?;
    let isrc: Option<String> = row.get(6)?;
    let from_tags: i64 = row.get(7)?;
    let mtime_ms: Option<i64> = row.get(8)?;

    Ok(CatalogTrack {
        id: ProviderTrackId::new(ProviderId::new(provider), reference),
        track: TrackRef::new(artist, title)
            .with_album(album)
            .with_duration_ms(duration_ms.and_then(|ms| u64::try_from(ms).ok()))
            .with_isrc(isrc.as_deref().and_then(Isrc::parse)),
        from_tags: from_tags != 0,
        mtime_ms,
        genres: Vec::new(),
    })
}

/// Where a genre was read (D-087). Resetting recommendations drops what was
/// learned — [`GenreSource::MusicBrainz`] — and keeps what the files and the
/// servers say about themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenreSource {
    /// The file's own tags.
    Tag,
    /// A Subsonic or Jellyfin server's answer.
    Server,
    /// A plugin's answer.
    Plugin,
    /// Looked up at MusicBrainz, online.
    MusicBrainz,
}

impl GenreSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::Server => "server",
            Self::Plugin => "plugin",
            Self::MusicBrainz => "musicbrainz",
        }
    }
}

/// Replaces what one source says a song's genres are. The values are folded
/// first ([`crate::recommend::genre`]); an empty list clears that source's
/// genres for the song — a retagged file that names none names none.
fn write_genres(
    tx: &rusqlite::Transaction<'_>,
    norm_key: &str,
    raw: &[String],
    source: GenreSource,
) -> Result<usize> {
    tx.execute(
        "DELETE FROM track_genres WHERE norm_key = ?1 AND source = ?2",
        rusqlite::params![norm_key, source.as_str()],
    )
    .map_err(db_err(Stage::LibraryWrite))?;
    let genres = crate::recommend::genre::normalize_genres(raw);
    if genres.is_empty() && source == GenreSource::MusicBrainz {
        // "Asked, and it names none": an empty marker, so the same song is
        // not asked again every visit. Readers skip it.
        tx.execute(
            "INSERT OR IGNORE INTO track_genres (norm_key, genre, source) VALUES (?1, '', ?2)",
            rusqlite::params![norm_key, source.as_str()],
        )
        .map_err(db_err(Stage::LibraryWrite))?;
    }
    for genre in &genres {
        tx.execute(
            "INSERT OR IGNORE INTO track_genres (norm_key, genre, source) VALUES (?1, ?2, ?3)",
            rusqlite::params![norm_key, genre, source.as_str()],
        )
        .map_err(db_err(Stage::LibraryWrite))?;
    }
    Ok(genres.len())
}

/// What resetting recommendations removed (D-087).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecommendForgetSummary {
    /// "Not for me" marks taken off.
    pub dismissed: usize,
    /// Genres learned from MusicBrainz dropped.
    pub learned_genres: usize,
}

/// What recommendations know about the songs (D-087): the genres and the
/// "not for me" marks. Kept apart from the listening history — resetting it
/// never touches a listen.
impl SqliteLibrary {
    /// Writes what `source` says the genres of these songs are, each by its
    /// artist and title. Returns how many songs were given at least one.
    ///
    /// # Errors
    /// If the database cannot be written.
    pub fn remember_genres(
        &mut self,
        songs: &[(String, String, Vec<String>)],
        source: GenreSource,
    ) -> Result<usize> {
        let tx = self
            .conn
            .transaction()
            .map_err(db_err(Stage::LibraryWrite))?;
        let mut written = 0;
        for (artist, title, genres) in songs {
            if write_genres(&tx, &track_key(artist, title), genres, source)? > 0 {
                written += 1;
            }
        }
        tx.commit().map_err(db_err(Stage::LibraryWrite))?;
        Ok(written)
    }

    /// Every song's genres, by grouping key, every source merged.
    ///
    /// # Errors
    /// If the database cannot be read.
    pub fn all_genres(&self) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT norm_key, genre FROM track_genres WHERE genre <> '' ORDER BY norm_key, genre",
            )
            .map_err(db_err(Stage::LibraryQuery))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_err(Stage::LibraryQuery))?;
        let mut out: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for row in rows {
            let (key, genre) = row.map_err(db_err(Stage::LibraryQuery))?;
            let list = out.entry(key).or_default();
            if !list.contains(&genre) {
                list.push(genre);
            }
        }
        Ok(out)
    }

    /// One song's genres, every source merged.
    ///
    /// # Errors
    /// If the database cannot be read.
    pub fn genres_of(&self, artist: &str, title: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT genre FROM track_genres
                 WHERE norm_key = ?1 AND genre <> '' ORDER BY genre",
            )
            .map_err(db_err(Stage::LibraryQuery))?;
        let rows = stmt
            .query_map([track_key(artist, title)], |row| row.get::<_, String>(0))
            .map_err(db_err(Stage::LibraryQuery))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err(Stage::LibraryQuery))?);
        }
        Ok(out)
    }

    /// The songs one source has said anything about — genres or, for
    /// MusicBrainz, "none" — by key.
    ///
    /// # Errors
    /// If the database cannot be read.
    pub fn genre_keys_from(
        &self,
        source: GenreSource,
    ) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT norm_key FROM track_genres WHERE source = ?1")
            .map_err(db_err(Stage::LibraryQuery))?;
        let rows = stmt
            .query_map([source.as_str()], |row| row.get::<_, String>(0))
            .map_err(db_err(Stage::LibraryQuery))?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row.map_err(db_err(Stage::LibraryQuery))?);
        }
        Ok(out)
    }

    /// The canonical identity of every resolved song in the history, by key.
    ///
    /// # Errors
    /// If the database cannot be read.
    pub fn canonical_ids(&self) -> Result<std::collections::HashMap<String, CanonicalId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT norm_key, canonical_id FROM tracks WHERE canonical_id IS NOT NULL")
            .map_err(db_err(Stage::LibraryQuery))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_err(Stage::LibraryQuery))?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (key, id) = row.map_err(db_err(Stage::LibraryQuery))?;
            out.insert(key, CanonicalId::from_stored(id));
        }
        Ok(out)
    }

    /// A provider's whole catalog (D-087): what recommendations pick from
    /// for the local files. In the catalog's own order.
    ///
    /// # Errors
    /// If the database cannot be read.
    pub fn catalog_of(&self, provider: &ProviderId) -> Result<Vec<CatalogTrack>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT provider, provider_ref, artist, title, album,
                        duration_ms, isrc, from_tags, mtime_ms
                 FROM provider_tracks
                 WHERE provider = ?1
                 ORDER BY id",
            )
            .map_err(db_err(Stage::LibraryQuery))?;
        let rows = stmt
            .query_map([provider.as_str()], catalog_row)
            .map_err(db_err(Stage::LibraryQuery))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err(Stage::LibraryQuery))?);
        }
        Ok(out)
    }

    /// Marks a song "not for me": recommendations leave it out until they
    /// are reset. Returns `false` when it was marked already.
    ///
    /// # Errors
    /// If the database cannot be written.
    pub fn dismiss(&mut self, artist: &str, title: &str) -> Result<bool> {
        let now = jiff::Timestamp::now().as_millisecond();
        let changed = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO recommend_dismissed (norm_key, artist, title, at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![track_key(artist, title), artist, title, now],
            )
            .map_err(db_err(Stage::LibraryWrite))?;
        Ok(changed > 0)
    }

    /// The grouping keys of the songs marked "not for me".
    ///
    /// # Errors
    /// If the database cannot be read.
    pub fn dismissed_keys(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT norm_key FROM recommend_dismissed")
            .map_err(db_err(Stage::LibraryQuery))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db_err(Stage::LibraryQuery))?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row.map_err(db_err(Stage::LibraryQuery))?);
        }
        Ok(out)
    }

    /// Forgets what recommendations learned: every "not for me" mark and
    /// every genre looked up at MusicBrainz. What the files and servers say
    /// stays; so does every listen.
    ///
    /// # Errors
    /// If the database cannot be written.
    pub fn forget_recommendations(&mut self) -> Result<RecommendForgetSummary> {
        let tx = self
            .conn
            .transaction()
            .map_err(db_err(Stage::LibraryWrite))?;
        let dismissed = tx
            .execute("DELETE FROM recommend_dismissed", [])
            .map_err(db_err(Stage::LibraryWrite))?;
        let learned_genres = tx
            .execute(
                "DELETE FROM track_genres WHERE source = ?1",
                [GenreSource::MusicBrainz.as_str()],
            )
            .map_err(db_err(Stage::LibraryWrite))?;
        tx.commit().map_err(db_err(Stage::LibraryWrite))?;
        Ok(RecommendForgetSummary {
            dismissed,
            learned_genres,
        })
    }
}

/// Reads a `ResolveMethod` back from text — for the reporting layer.
#[must_use]
pub fn parse_resolve_method(raw: &str) -> Option<ResolveMethod> {
    match raw {
        "isrc" => Some(ResolveMethod::Isrc),
        "mbid" => Some(ResolveMethod::Mbid),
        "fuzzy" => Some(ResolveMethod::Fuzzy),
        "fingerprint" => Some(ResolveMethod::Fingerprint),
        "local_key" => Some(ResolveMethod::LocalKey),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ExportKind;

    fn listen(artist: &str, title: &str, ts: &str, ms: u64) -> Listen {
        Listen {
            track: TrackRef::new(artist, title).with_album(Some("Pablo Honey".to_owned())),
            played_at: ts.parse().expect("test timestamp"),
            ms_played: ms,
            source: ListenSource::Import {
                export: ExportKind::SpotifyExtended,
            },
            canonical_id: None,
        }
    }

    #[test]
    fn schema_applies_and_fts5_is_available() {
        let library = SqliteLibrary::open_in_memory().expect("the in-memory library must open");
        let version: i64 = library
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("the version must be readable");
        assert_eq!(usize::try_from(version).unwrap(), schema::MIGRATIONS.len());
    }

    /// D-077: a track's plays and skips, and its artist's, by the same rule
    /// `stats` counts with — the artist found by its part of the key, not by
    /// a prefix of its name.
    #[test]
    fn a_listening_profile_counts_plays_and_skips_by_the_play_rule() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        library
            .insert_listens(&[
                listen("Radiohead", "Creep", "2023-01-01T00:00:00Z", 200_000),
                listen("Radiohead", "Creep", "2023-01-02T00:00:00Z", 190_000),
                listen("Radiohead", "Creep", "2023-01-03T00:00:00Z", 4_000),
                listen("Radiohead", "Karma Police", "2023-01-04T00:00:00Z", 3_000),
                listen("Radiohead", "Nude", "2023-01-05T00:00:00Z", 250_000),
                listen("Radioheads", "Creep", "2023-01-06T00:00:00Z", 250_000),
            ])
            .unwrap();

        let profile = library
            .listening_profile("radiohead", "Creep (Remastered 2011)", PlayRule::default())
            .unwrap();
        assert_eq!(
            profile,
            ListeningProfile {
                plays: 2,
                skips: 1,
                artist_plays: 3,
                artist_skips: 2,
            }
        );
        assert_eq!(
            library
                .listening_profile("Nobody", "Nothing", PlayRule::default())
                .unwrap(),
            ListeningProfile::default(),
            "a track never heard is no history, not an error"
        );
    }

    #[test]
    fn reimport_does_not_double_count() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let batch = vec![
            listen("Radiohead", "Creep", "2023-01-01T00:00:00Z", 200_000),
            listen("Radiohead", "Karma Police", "2023-01-01T00:05:00Z", 260_000),
        ];

        let first = library.insert_listens(&batch).unwrap();
        assert_eq!(first.inserted, 2);
        assert_eq!(first.new_tracks, 2);
        assert_eq!(first.duplicates, 0);

        let second = library.insert_listens(&batch).unwrap();
        assert_eq!(second.inserted, 0);
        assert_eq!(second.duplicates, 2);
        assert_eq!(second.new_tracks, 0);
        assert_eq!(library.all_listens().unwrap().len(), 2);
    }

    #[test]
    fn listens_round_trip_through_storage() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let original = listen("Portishead", "Roads", "2024-03-05T10:11:12Z", 303_000);
        library
            .insert_listens(std::slice::from_ref(&original))
            .unwrap();

        let loaded = library.all_listens().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].track.artist, "Portishead");
        assert_eq!(loaded[0].track.album.as_deref(), Some("Pablo Honey"));
        assert_eq!(loaded[0].played_at, original.played_at);
        assert_eq!(loaded[0].ms_played, 303_000);
    }

    #[test]
    fn full_text_search_finds_prefixes_and_counts_plays() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        library
            .insert_listens(&[
                listen("Radiohead", "Creep", "2023-01-01T00:00:00Z", 200_000),
                listen("Radiohead", "Creep", "2023-01-02T00:00:00Z", 200_000),
                listen("Portishead", "Roads", "2023-01-03T00:00:00Z", 300_000),
            ])
            .unwrap();

        let rule = PlayRule::default();
        let found = library.search("radio", 10, rule).unwrap();
        assert_eq!(
            found.hits.len(),
            1,
            "prefix search must find Radiohead: {found:?}"
        );
        assert_eq!(found.hits[0].play_count, 2);
        assert_eq!(found.hits[0].ms_played, 400_000);
        assert_eq!(found.listen_events, 2);

        assert_eq!(library.search("creep", 10, rule).unwrap().hits.len(), 1);
        assert!(
            library
                .search("nonexistenttrack", 10, rule)
                .unwrap()
                .hits
                .is_empty()
        );
    }

    #[test]
    fn search_counts_the_same_plays_as_stats() {
        // D-008: `search` counted raw events and `stats` thresholded plays; the
        // same data gave two different "play" counts. Now there is one rule.
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let batch = [
            listen("Radiohead", "Creep", "2023-01-01T00:00:00Z", 200_000),
            listen("Radiohead", "Creep", "2023-01-02T00:00:00Z", 200_000),
            // Below the threshold: must not count as a "play" on either surface.
            listen("Radiohead", "Creep", "2023-01-03T00:00:00Z", 4_000),
        ];
        library.insert_listens(&batch).unwrap();

        let rule = PlayRule::default();
        let found = library.search("creep", 10, rule).unwrap();
        let stats = crate::stats::compute(
            &library.all_listens().unwrap(),
            crate::stats::StatsQuery::default(),
        );

        assert_eq!(found.hits[0].play_count, stats.plays);
        assert_eq!(found.hits[0].ms_played, stats.total_ms_played);
        assert_eq!(
            found.listen_events, stats.listens_in_scope,
            "the raw event count must match too"
        );
        assert_eq!(found.listen_events - found.hits[0].play_count, 1);
    }

    #[test]
    fn sql_play_rule_agrees_with_rust() {
        // If `play_predicate_sql` and `PlayRule::counts` drift apart, D-008 comes
        // back. The edge cases are compared one by one, through real SQLite.
        let rule = PlayRule::default();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sql = format!(
            "SELECT {} FROM (SELECT ?1 AS ms_played) l, (SELECT ?2 AS duration_ms) t",
            play_predicate_sql(rule)
        );

        let cases: &[(u64, Option<u64>)] = &[
            (0, None),
            (0, Some(0)),
            (1, Some(0)),
            (29_999, None),
            (30_000, None),
            (30_001, None),
            (25_000, Some(40_000)),
            (25_000, Some(50_000)),
            (25_000, Some(50_001)),
            (25_000, Some(240_000)),
            (200_000, Some(238_000)),
        ];

        for &(ms_played, duration_ms) in cases {
            let sql_says: bool = conn
                .query_row(
                    &sql,
                    rusqlite::params![
                        i64::try_from(ms_played).unwrap(),
                        duration_ms.map(|d| i64::try_from(d).unwrap()),
                    ],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                sql_says,
                rule.counts(ms_played, duration_ms),
                "ms_played={ms_played}, duration_ms={duration_ms:?}"
            );
        }
    }

    #[test]
    fn fts_special_characters_do_not_break_the_query() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        library
            .insert_listens(&[listen(
                "AC/DC",
                "Back in Black",
                "2023-01-01T00:00:00Z",
                255_000,
            )])
            .unwrap();

        // Quotes and FTS5 operators in raw input must not cause a syntax error.
        for raw in ["\"AC OR DC", "back*", "NOT black", "(", "a\"b"] {
            library
                .search(raw, 10, PlayRule::default())
                .unwrap_or_else(|err| panic!("the {raw:?} query blew up: {}", err.chain_text()));
        }

        let rule = PlayRule::default();
        assert_eq!(library.search("AC/DC", 10, rule).unwrap().hits.len(), 1);
        assert_eq!(
            library.search("back black", 10, rule).unwrap().hits.len(),
            1
        );
    }

    #[test]
    fn empty_search_is_an_error_not_an_empty_list() {
        let library = SqliteLibrary::open_in_memory().unwrap();
        let err = library.search("   ", 10, PlayRule::default()).unwrap_err();
        assert_eq!(err.stage(), Stage::LibraryQuery);
    }

    #[test]
    fn resolution_is_persisted_and_read_back() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        library
            .insert_listens(&[listen(
                "Radiohead",
                "Creep",
                "2023-01-01T00:00:00Z",
                200_000,
            )])
            .unwrap();

        let resolution = Resolution {
            canonical_id: CanonicalId::from_local_key("radiohead\u{1}creep"),
            method: ResolveMethod::LocalKey,
            confidence: 0.2,
            matched: None,
            tied_candidates: 1,
            unread_equals: false,
        };
        library
            .set_resolution(&track_key("Radiohead", "Creep"), &resolution)
            .unwrap();

        let loaded = library.all_listens().unwrap();
        assert_eq!(loaded[0].canonical_id, Some(resolution.canonical_id));
    }

    // — The persistent catalog (Phase 1.2) —

    fn local_id(reference: &str) -> ProviderTrackId {
        ProviderTrackId::new(ProviderId::new("local"), reference)
    }

    fn catalog_entry(reference: &str, artist: &str, title: &str, mtime: i64) -> CatalogTrack {
        CatalogTrack {
            id: local_id(reference),
            track: TrackRef::new(artist, title).with_album(Some("Album".to_owned())),
            from_tags: true,
            mtime_ms: Some(mtime),
            genres: Vec::new(),
        }
    }

    #[test]
    fn catalog_survives_reopening_the_database() {
        // This was the whole point: the index is on disk, not in memory.
        let dir = crate::test_support::TempDir::new("catalog");
        let db = dir.join("library.db");
        let provider = ProviderId::new("local");

        {
            let mut library = SqliteLibrary::open(&db).unwrap();
            library
                .replace_catalog(
                    &provider,
                    &[catalog_entry("/m/a.flac", "Radiohead", "Creep", 100)],
                )
                .unwrap();
        }

        // Like a new process: open the database from scratch.
        let library = SqliteLibrary::open(&db).unwrap();
        assert_eq!(library.catalog_len(&provider).unwrap(), 1);
        let hits = library.search_catalog("radiohead", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].track.title, "Creep");
    }

    #[test]
    fn replacing_the_catalog_drops_rows_that_vanished() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let provider = ProviderId::new("local");

        let write = library
            .replace_catalog(
                &provider,
                &[
                    catalog_entry("/m/a.flac", "Radiohead", "Creep", 100),
                    catalog_entry("/m/b.flac", "Portishead", "Roads", 100),
                ],
            )
            .unwrap();
        assert_eq!(write.inserted, 2);

        // The second file was deleted: the catalog must be the source's mirror.
        let write = library
            .replace_catalog(
                &provider,
                &[catalog_entry("/m/a.flac", "Radiohead", "Creep", 100)],
            )
            .unwrap();
        assert_eq!(write.removed, 1);
        assert_eq!(library.catalog_len(&provider).unwrap(), 1);
        assert!(library.search_catalog("portishead", 10).unwrap().is_empty());
    }

    #[test]
    fn dropping_a_file_from_the_catalog_never_touches_its_history() {
        // The **history** of a file you deleted from disk is not deleted.
        // `tracks`/`listens` and `provider_tracks` are separate tables on purpose.
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let provider = ProviderId::new("local");

        library
            .insert_listens(&[listen(
                "Radiohead",
                "Creep",
                "2023-01-01T00:00:00Z",
                200_000,
            )])
            .unwrap();
        library
            .replace_catalog(
                &provider,
                &[catalog_entry("/m/a.flac", "Radiohead", "Creep", 100)],
            )
            .unwrap();

        // The file was deleted from disk → the catalog is empty.
        library.replace_catalog(&provider, &[]).unwrap();
        assert_eq!(library.catalog_len(&provider).unwrap(), 0);

        // The history is still there.
        assert_eq!(library.all_listens().unwrap().len(), 1);
        let hits = library.search("creep", 10, PlayRule::default()).unwrap();
        assert_eq!(
            hits.hits.len(),
            1,
            "the listening history must stay searchable"
        );
    }

    #[test]
    fn an_unchanged_stamp_is_reported_and_the_row_is_left_alone() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let provider = ProviderId::new("local");
        let row = catalog_entry("/m/a.flac", "Radiohead", "Creep", 100);

        library
            .replace_catalog(&provider, std::slice::from_ref(&row))
            .unwrap();
        let write = library.replace_catalog(&provider, &[row]).unwrap();

        assert_eq!(write.unchanged, 1, "the same stamp must not be rewritten");
        assert_eq!(write.updated, 0);
        assert_eq!(write.inserted, 0);
    }

    #[test]
    fn a_changed_stamp_updates_the_metadata() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let provider = ProviderId::new("local");

        library
            .replace_catalog(
                &provider,
                &[catalog_entry("/m/a.flac", "Old Name", "Old Title", 100)],
            )
            .unwrap();
        // The file was edited: the stamp changed, and so did the tags.
        let write = library
            .replace_catalog(
                &provider,
                &[catalog_entry("/m/a.flac", "New Name", "New Title", 200)],
            )
            .unwrap();

        assert_eq!(write.updated, 1);
        let hits = library.search_catalog("new", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].track.artist, "New Name");
        // The old metadata must not stay in FTS.
        assert!(library.search_catalog("old", 10).unwrap().is_empty());
    }

    #[test]
    fn stamps_are_returned_for_incremental_scanning() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let provider = ProviderId::new("local");
        library
            .replace_catalog(
                &provider,
                &[
                    catalog_entry("/m/a.flac", "A", "1", 111),
                    catalog_entry("/m/b.flac", "B", "2", 222),
                ],
            )
            .unwrap();

        let stamps = library.catalog_stamps(&provider).unwrap();
        assert_eq!(stamps.get("/m/a.flac"), Some(&111));
        assert_eq!(stamps.get("/m/b.flac"), Some(&222));
    }

    #[test]
    fn catalogs_of_different_providers_do_not_erase_each_other() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let local = ProviderId::new("local");
        let remote = ProviderId::new("subsonic");

        library
            .replace_catalog(&local, &[catalog_entry("/m/a.flac", "A", "1", 100)])
            .unwrap();
        library
            .replace_catalog(
                &remote,
                &[CatalogTrack {
                    id: ProviderTrackId::new(remote.clone(), "track-9"),
                    track: TrackRef::new("B", "2"),
                    from_tags: true,
                    mtime_ms: None,
                    genres: Vec::new(),
                }],
            )
            .unwrap();

        // Writing a remote provider must not drop the local one.
        assert_eq!(library.catalog_len(&local).unwrap(), 1);
        assert_eq!(library.catalog_len(&remote).unwrap(), 1);
    }

    #[test]
    fn catalog_get_finds_a_row_by_provider_id() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let provider = ProviderId::new("local");
        library
            .replace_catalog(
                &provider,
                &[catalog_entry("/m/a.flac", "Radiohead", "Creep", 100)],
            )
            .unwrap();

        let found = library.catalog_get(&local_id("/m/a.flac")).unwrap();
        assert_eq!(found.map(|row| row.track.title), Some("Creep".to_owned()));
        assert_eq!(
            library.catalog_get(&local_id("/m/missing.flac")).unwrap(),
            None
        );
    }

    #[test]
    fn an_empty_catalog_search_is_empty_not_an_error() {
        let library = SqliteLibrary::open_in_memory().unwrap();
        assert!(library.search_catalog("", 10).unwrap().is_empty());
        assert!(library.search_catalog("  ", 10).unwrap().is_empty());
    }

    #[test]
    fn migrating_a_v1_database_keeps_its_listens() {
        // The v2 migration must not break existing installs: the history the user
        // imported must stay in place when the schema is upgraded.
        let dir = crate::test_support::TempDir::new("migrate");
        let db = dir.join("library.db");

        // Set up the v1 schema by hand and write a listen.
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(schema::MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO tracks (norm_key, artist, title) VALUES ('k', 'Radiohead', 'Creep')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO listens (track_id, played_at, ms_played, source_kind)
                 VALUES (1, 1700000000000, 200000, 'import')",
                [],
            )
            .unwrap();
        }

        // Opening applies the migration.
        let mut library = SqliteLibrary::open(&db).unwrap();
        let version: i64 = library
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3, "the schema must be upgraded to the latest");

        // The old history is still there.
        assert_eq!(library.all_listens().unwrap().len(), 1);
        // The new catalog is usable.
        let provider = ProviderId::new("local");
        library
            .replace_catalog(
                &provider,
                &[catalog_entry("/m/a.flac", "Portishead", "Roads", 1)],
            )
            .unwrap();
        assert_eq!(library.catalog_len(&provider).unwrap(), 1);
    }

    /// v3 (D-087): files scanned before were read without their genres, so
    /// their stamps are cleared and the next scan reads them again.
    #[test]
    fn the_genre_migration_makes_the_next_scan_read_every_file_again() {
        let dir = crate::test_support::TempDir::new("library-v3");
        let db = dir.path().join("library.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(schema::MIGRATIONS[0]).unwrap();
            conn.execute_batch(schema::MIGRATIONS[1]).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
            conn.execute(
                "INSERT INTO provider_tracks
                     (provider, provider_ref, norm_key, artist, title, from_tags, mtime_ms, scanned_at)
                 VALUES ('local', '/m/a.flac', 'k', 'A', 'B', 1, 42, 1)",
                [],
            )
            .unwrap();
        }
        let library = SqliteLibrary::open(&db).unwrap();
        assert!(
            library
                .catalog_stamps(&ProviderId::new("local"))
                .unwrap()
                .is_empty(),
            "no stamp survives: every file is read again"
        );
    }

    #[test]
    fn a_files_genres_are_written_with_its_row_and_follow_a_retag() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        let provider = ProviderId::new("local");
        let mut entry = catalog_entry("/m/a.flac", "Slowdive", "Alison", 1);
        entry.genres = vec!["Shoegaze; Dream Pop".to_owned()];
        library
            .replace_catalog(&provider, &[entry.clone()])
            .unwrap();
        assert_eq!(
            library.genres_of("Slowdive", "Alison").unwrap(),
            vec!["dream pop", "shoegaze"]
        );

        entry.mtime_ms = Some(2);
        entry.genres = vec!["Alternative".to_owned()];
        library.replace_catalog(&provider, &[entry]).unwrap();
        assert_eq!(
            library.genres_of("Slowdive", "Alison").unwrap(),
            vec!["alternative"]
        );
    }

    /// Resetting forgets what was learned and keeps what the files say —
    /// and never a listen.
    #[test]
    fn forgetting_recommendations_keeps_the_files_genres_and_the_history() {
        let mut library = SqliteLibrary::open_in_memory().unwrap();
        library
            .insert_listens(&[listen(
                "Radiohead",
                "Creep",
                "2023-01-01T00:00:00Z",
                200_000,
            )])
            .unwrap();
        let mut entry = catalog_entry("/m/a.flac", "Radiohead", "Creep", 1);
        entry.genres = vec!["Alternative Rock".to_owned()];
        library
            .replace_catalog(&ProviderId::new("local"), &[entry])
            .unwrap();
        library
            .remember_genres(
                &[
                    (
                        "Radiohead".to_owned(),
                        "Creep".to_owned(),
                        vec!["Grunge".to_owned()],
                    ),
                    ("Portishead".to_owned(), "Roads".to_owned(), Vec::new()),
                ],
                GenreSource::MusicBrainz,
            )
            .unwrap();
        assert!(library.dismiss("Radiohead", "Creep").unwrap());
        assert!(
            !library.dismiss("radiohead", "creep").unwrap(),
            "marked already"
        );
        assert_eq!(library.dismissed_keys().unwrap().len(), 1);
        assert_eq!(
            library
                .genre_keys_from(GenreSource::MusicBrainz)
                .unwrap()
                .len(),
            2,
            "\"names none\" is remembered too"
        );
        assert_eq!(
            library.genres_of("Radiohead", "Creep").unwrap(),
            vec!["alternative rock", "grunge"]
        );

        let forgot = library.forget_recommendations().unwrap();
        assert_eq!(forgot.dismissed, 1);
        assert_eq!(forgot.learned_genres, 2);
        assert!(library.dismissed_keys().unwrap().is_empty());
        assert_eq!(
            library.genres_of("Radiohead", "Creep").unwrap(),
            vec!["alternative rock"]
        );
        assert_eq!(library.all_listens().unwrap().len(), 1);
    }
}
