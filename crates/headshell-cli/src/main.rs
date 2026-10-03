//! `headshell` — the thin CLI shell the core is exercised by hand with.
//!
//! **The Golden Rule:** no business logic here. This file only parses
//! arguments, calls `headshell_core::session::Session`, formats the output and
//! sets the exit code. Delete a feature from here and the core still offers
//! it.

mod output;
mod tui;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use headshell_core::config::Config;
use headshell_core::ids::ProviderId;
use headshell_core::model::{PlayRule, TrackRef};
use headshell_core::playback::LiveSession;
use headshell_core::provider;
use headshell_core::session::{self, LookupMode, Session};
use headshell_core::sleeve::CardPreset;
use headshell_core::stats::StatsQuery;

/// Exit codes: 0 success, 1 core error, 2 usage error (clap).
const EXIT_FAILURE: u8 = 1;

#[derive(Debug, Parser)]
#[command(
    name = "headshell",
    version,
    about = "A provider-independent listening identity"
)]
struct Cli {
    /// Give the data directory by hand. The default varies by system:
    /// Linux/BSD `~/.local/share/headshell`, macOS
    /// `~/Library/Application Support/headshell`, Windows
    /// `%LOCALAPPDATA%\headshell`. `headshell diag` prints the one in use.
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Give the output as JSON.
    #[arg(long, global = true)]
    json: bool,

    /// Ask MusicBrainz during identity resolution.
    ///
    /// Off by default: `headshell` works without a network, and importing an
    /// export never silently connects anyone to the network. MusicBrainz accepts
    /// one request per second — fine for a single-track `resolve`, hours for an
    /// `import` of thousands of tracks.
    #[arg(long, global = true)]
    online: bool,

    /// Verbose logging (repeatable: -v, -vv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

/// The ready-made card formats (a clap ValueEnum — not in the core).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CardFormat {
    Square,
    Story,
}

impl CardFormat {
    fn to_preset(self) -> CardPreset {
        match self {
            Self::Square => CardPreset::Square,
            Self::Story => CardPreset::Story,
        }
    }
}

/// `on` or `off` (a clap ValueEnum — not in the core, which says
/// [`LookupMode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Switch {
    On,
    Off,
}

impl Switch {
    const fn to_mode(self) -> LookupMode {
        match self {
            Self::On => LookupMode::Online,
            Self::Off => LookupMode::Offline,
        }
    }
}

/// A provider's role (D-086) — a clap ValueEnum; the core says
/// [`session::Role`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RoleArg {
    Main,
    Music,
    Video,
}

impl RoleArg {
    const fn to_role(self) -> session::Role {
        match self {
            Self::Main => session::Role::Main,
            Self::Music => session::Role::Music,
            Self::Video => session::Role::Video,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Import a data export archive (a zip or an extracted directory).
    Import {
        /// The export zip or the extracted export directory.
        path: PathBuf,
    },
    /// Listening statistics.
    Stats {
        /// Only this year (UTC).
        #[arg(long, value_name = "YEAR")]
        year: Option<i16>,
        /// How many rows in lists.
        #[arg(long, default_value_t = 10, value_name = "N")]
        top: usize,
        /// The threshold for counting as "listened" (ms).
        #[arg(long, value_name = "MS")]
        min_ms: Option<u64>,
    },
    /// Run a single track through the identity chain.
    Resolve {
        /// A query of the form `"Artist - Title"`.
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        query: Option<String>,
        /// An audio file instead of a query: it is read from the metadata tags, and
        /// if the text links come up empty the audio fingerprint is asked.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
    /// Library operations.
    Library {
        #[command(subcommand)]
        command: LibraryCommand,
    },
    /// Find the covers of the tracks a play query finds (D-076): where each
    /// track lives first — its tags, its folder, its provider — then, with
    /// `--online`, MusicBrainz and the Cover Art Archive.
    Artwork {
        /// The play query: the tracks it finds, in play order.
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        query: Option<String>,
        /// Every matching track instead of the first.
        #[arg(long)]
        all: bool,
        /// An audio file instead of a query.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Write the covers to this directory, one image per album.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
    /// Find the lyrics of the tracks a play query finds (D-078): the file's
    /// `.lrc` and tags, the server or plugin, then LRCLIB — its version of the
    /// recording's length, or the nearest one, marked. Running this is
    /// asking for lyrics: LRCLIB is asked without `--online`.
    Lyrics {
        /// The play query: the tracks it finds, in play order.
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        query: Option<String>,
        /// Every matching track instead of the first.
        #[arg(long)]
        all: bool,
        /// An audio file instead of a query.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Print the lyrics themselves, with the time of each line.
        #[arg(long)]
        print: bool,
        /// Keep a shift for the tracks found, in milliseconds: positive, the
        /// lines come sooner; 0 takes it back.
        #[arg(long, value_name = "MS", allow_hyphen_values = true)]
        shift: Option<i64>,
    },
    /// Make the first tracks a play query finds ready ahead of time (D-077):
    /// the track `play` would start and the two after it — each one's address
    /// resolved and the first bytes of its stream fetched — and say how long
    /// each took. What a playing session does for its queue, as a command.
    Prefetch {
        /// The play query.
        query: String,
        /// Queue every matching track, so the two after the first are made
        /// ready too.
        #[arg(long)]
        all: bool,
    },
    /// Produce a shareable listening card (Sleeve).
    Sleeve {
        /// Only this year (UTC).
        #[arg(long, value_name = "YEAR")]
        year: Option<i16>,
        /// The output file; the extension decides the format (.svg / .png).
        /// If not given, the card data is only written to the screen/JSON.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// The card format.
        #[arg(long, default_value_t = CardFormat::Square, value_enum)]
        format: CardFormat,
    },
    /// Provider operations.
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// Plugin operations: catalog, install, consent, update, removal.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// The secret store: credentials for plugins and providers.
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
    /// What the play box offers for a text while it is typed (D-080).
    ///
    /// The tracks whose title begins with it, then the artists, then the other
    /// matches — from the provider chosen to play from (`provider use`), or,
    /// when it has nothing, from every other one, each named.
    Suggest {
        /// The text typed so far.
        query: String,
    },
    /// The videos for a song, from the provider chosen for videos (D-086).
    ///
    /// A file's own video first, when it is one; otherwise the video provider
    /// is searched with the song's artist and title, and the rows that are
    /// it are listed, best first — what the desktop's music/video switch
    /// shows.
    Video {
        /// `Artist - Title`.
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        query: Option<String>,
        /// An audio or video file instead of a query.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
    /// Play a local track.
    Play {
        /// The search text for finding the track to play.
        query: String,
        /// Queue all of the matches instead of the first one.
        #[arg(long)]
        all: bool,
        /// Shuffle the queue.
        #[arg(long)]
        shuffle: bool,
        /// Exit without waiting for playback (only show the queue).
        #[arg(long)]
        dry_run: bool,
        /// Open the terminal interface (queue, progress, key controls).
        #[arg(long)]
        tui: bool,
    },
    /// Whether the desktop asks MusicBrainz and the Cover Art Archive (D-082).
    ///
    /// Without an argument it shows the saved choice, `HEADSHELL_ONLINE` as
    /// this shell has it, and what a desktop started from here goes by. The
    /// choice is the desktop's: the CLI itself goes online only with
    /// `--online`, run by run.
    Online {
        /// Save the choice: `on` or `off`.
        #[arg(value_enum)]
        state: Option<Switch>,
    },
    /// The folders the local files come from (D-085).
    ///
    /// Without a subcommand it lists them and says who decided: the
    /// folders chosen here, `HEADSHELL_MUSIC_DIRS` for this run, or the
    /// system's music folder when nothing is chosen. A changed folder is read
    /// at the next `headshell provider scan`.
    MusicDirs {
        #[command(subcommand)]
        action: Option<MusicDirsCommand>,
    },
    /// Songs picked for you from one source (D-087).
    ///
    /// Worked out here, from your own history: songs played around the
    /// ones you love lately, the genres you play most, and — for the local
    /// files — your favourite artists' songs you haven't heard. The source
    /// is asked only for songs like one of its own tracks, by that track's
    /// id; your history never leaves this machine. With `--online`,
    /// MusicBrainz is asked for the genres of a few loved songs.
    Recommend {
        #[command(subcommand)]
        action: Option<RecommendCommand>,
        /// The source to recommend from; the one music plays from when
        /// left out.
        #[arg(long, value_name = "PROVIDER")]
        from: Option<String>,
    },
    /// The diagnostics report of the last run.
    Diag,
}

#[derive(Debug, Subcommand)]
enum RecommendCommand {
    /// Mark a song "not for me": it is left out until recommendations are
    /// reset. Your history is not touched.
    Dismiss {
        /// `Artist - Title`.
        query: String,
    },
    /// Start recommendations over: every "not for me" mark and every genre
    /// learned online is forgotten, and your taste is read again from now
    /// on. Your history and stats stay as they are.
    Reset,
}

#[derive(Debug, Subcommand)]
enum MusicDirsCommand {
    /// Add a folder; with nothing chosen yet, the system's folder stays.
    Add {
        /// The folder.
        dir: PathBuf,
    },
    /// Take a folder off the list; its songs leave at the next scan.
    Remove {
        /// The folder, as the list shows it.
        dir: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ProviderCommand {
    /// List the registered providers.
    List,
    /// Test a provider (is it up, how many tracks does it see).
    Test {
        /// The provider name (`local`).
        name: String,
    },
    /// Rescan the local music directories.
    Scan {
        /// Scan only if the directories changed since the last scan.
        ///
        /// Looks at directory stamps; does not do a full scan. It cannot see files
        /// re-tagged in place — that needs a plain `scan`.
        #[arg(long)]
        if_stale: bool,
    },
    /// Register a remote server (Subsonic or Jellyfin).
    ///
    /// The password is taken from the `HEADSHELL_PASSWORD` environment variable
    /// or from a prompt; it is not written on the command line (it would end up
    /// in the shell history).
    Add {
        /// The server type: `subsonic` | `jellyfin`.
        kind: String,
        /// The base address (`https://music.home`).
        #[arg(long, value_name = "URL")]
        url: String,
        /// The user name.
        #[arg(long, value_name = "NAME")]
        user: String,
        /// The provider name; derived from the address if not given.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// An API key directly instead of a password (Jellyfin only).
        #[arg(long, value_name = "KEY")]
        api_key: Option<String>,
        /// Connect to the server and verify the credentials before saving.
        #[arg(long)]
        no_verify: bool,
    },
    /// Delete a registered remote server.
    Remove {
        /// The provider name.
        name: String,
    },
    /// List the registered remote servers (credentials are not shown).
    Servers,
    /// Choose the provider the play box and `play` search first (D-080).
    ///
    /// The others are asked only when it finds nothing. The choice is kept
    /// in the data directory; the desktop's providers section makes the same
    /// choice.
    ///
    /// Since D-086 this is the **main** provider: music and video follow it
    /// until each is chosen on its own with `--for`.
    Use {
        /// The provider name (`local`, a server's name, a plugin's name).
        name: String,
        /// The role to give it: `music` (what the play box searches) or
        /// `video` (where a song's video is found); `main` when left out.
        #[arg(long = "for", value_enum, value_name = "ROLE")]
        role: Option<RoleArg>,
    },
    /// Let music or video follow the main provider again (D-086); for
    /// `main`, forget the choice — the first that can play is used.
    Follow {
        #[arg(value_enum)]
        role: RoleArg,
    },
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    /// List the installed plugins and their state (does not run plugins).
    List,
    /// Show the plugin catalog: what can be installed, what is installed, what
    /// can be updated.
    ///
    /// The catalog is the `index.json` in the `headshell/plugins` repository;
    /// `HEADSHELL_PLUGIN_INDEX` gives another address (a mirror, a fork). It only
    /// reads, it installs nothing. It does not wait for `--online`: asking for
    /// the list is going online.
    Catalog,
    /// Approve the permissions a plugin declares.
    ///
    /// Approved permissions are enforced (D-069): the plugin can only connect to
    /// the hosts it declares and cannot access the file system. The tools the
    /// engine installs (like yt-dlp) are separate programs and are outside this
    /// boundary.
    Approve {
        /// The plugin name (the directory name).
        name: String,
    },
    /// Install a plugin: download it from the catalog if it is not on disk, then
    /// install its tools.
    ///
    /// Every file from the catalog is verified with its sha256, and the
    /// downloaded manifest is compared with the one shown in the catalog; if they
    /// do not match, nothing is written. An installed plugin **awaits consent**:
    /// `headshell plugin approve <name>`. For a plugin already on disk the
    /// catalog is not read, only its tools are installed — updating is
    /// `update`'s job (D-071).
    ///
    /// The engine downloads the tools (like yt-dlp), not the plugin; each comes
    /// with a pinned version and a separate binary per platform, and is not put
    /// in place before its sha256 is verified. No root is asked for, no Python is
    /// needed (D-055, D-069).
    ///
    /// It does not wait for `--online`: downloading is the command itself, not a
    /// side effect.
    Install {
        /// The plugin name.
        name: String,
    },
    /// Bring the plugins installed from the catalog up to the catalog's version.
    ///
    /// Without a name, all of those installed from the catalog. A plugin put
    /// there by hand or changed locally is not overwritten. If the new version
    /// asks for extra permissions the plugin awaits consent again; if a tool
    /// (yt-dlp) changed, the output says so (D-071).
    Update {
        /// The plugin name; all of them if not given.
        name: Option<String>,
    },
    /// Remove a plugin: deletes its directory, forgets its consent.
    ///
    /// Secrets and the tools the engine installed are not deleted; the names of
    /// the remaining secrets are printed. If the directory is a link, only the
    /// link is removed.
    Remove {
        /// The plugin name.
        name: String,
    },
    /// Produce or check the catalog repository's index (catalog maintenance).
    ///
    /// `DIR` is a copy of `headshell/plugins`: each plugin is `<name>/plugin.json`
    /// and its script. Every manifest goes through the same validation as an
    /// install, the files' sha256 are computed and `<DIR>/index.json` is written.
    Index {
        /// The root of the catalog repository.
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        /// The file address template: `{name}`, `{version}` and `{path}` are
        /// required. If not given, the template in the existing `index.json` is used.
        #[arg(long, value_name = "TEMPLATE")]
        url_template: Option<String>,
        /// Write nothing; if the index is not up to date, say what differs and fail
        /// (for the catalog repository's CI).
        #[arg(long)]
        check: bool,
    },
    /// Disable a plugin (the consent record is kept).
    Disable {
        /// The plugin name.
        name: String,
    },
    /// Re-enable a disabled plugin.
    Enable {
        /// The plugin name.
        name: String,
    },
    /// Forget the consent entirely; it is asked from scratch next time.
    Forget {
        /// The plugin name.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum SecretCommand {
    /// List the namespaces and key names (values are not shown).
    List,
    /// Write a secret. The value is read from `HEADSHELL_SECRET` or from a prompt
    /// without echo.
    Set {
        /// The namespace (`plugin:soundcloud`).
        namespace: String,
        /// The key name (`client_id`).
        key: String,
    },
    /// Delete a secret.
    Remove {
        /// The namespace.
        namespace: String,
        /// The key name.
        key: String,
    },
}

#[derive(Debug, Subcommand)]
enum LibraryCommand {
    /// Full-text search.
    Search {
        /// The text to search for.
        query: String,
        /// At most how many results.
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
        /// The threshold for counting as "listened" (ms) — the same rule as `stats`.
        #[arg(long, value_name = "MS")]
        min_ms: Option<u64>,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    // A command that can partly succeed (updating every plugin) still prints
    // its report, but does not return zero: a script must be able to see
    // "something did not happen".
    let mut succeeded = true;
    match run(&cli, &mut succeeded).await {
        Ok(text) => {
            print!("{text}");
            if succeeded {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_FAILURE)
            }
        }
        Err(err) => {
            eprintln!("{}", err.chain_text());
            eprintln!("\nfor details: headshell diag");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Runs the command and returns the text to print. If the command partly
/// fails it lowers `succeeded`; the text is still printed.
async fn run(cli: &Cli, succeeded: &mut bool) -> headshell_core::Result<String> {
    let config = match &cli.data_dir {
        Some(dir) => Config::with_data_dir(dir),
        None => Config::discover()?,
    };
    let mut session = Session::open(config)?;
    let lookup_mode = if cli.online {
        LookupMode::Online
    } else {
        LookupMode::Offline
    };

    match &cli.command {
        Command::Import { path } => {
            let report = session
                .import_archive(path, session::lookup_for(lookup_mode)?)
                .await?;
            render(cli.json, &report, || output::import(&report))
        }
        Command::Stats { year, top, min_ms } => {
            let query = StatsQuery {
                year: *year,
                top: *top,
                min_ms_played: min_ms.unwrap_or(headshell_core::stats::DEFAULT_MIN_MS_PLAYED),
            };
            let response = session.stats(query)?;
            render(cli.json, &response, || output::stats(&response))
        }
        Command::Resolve { query, file } => {
            let report = match (query, file) {
                (_, Some(path)) => {
                    session
                        .resolve_file(
                            path,
                            session::lookup_for(lookup_mode)?,
                            session.fingerprint_lookup_for(lookup_mode)?,
                        )
                        .await?
                }
                (Some(query), None) => {
                    session
                        .resolve_track(query, session::lookup_for(lookup_mode)?)
                        .await?
                }
                // `clap` already rejects this combination (`required_unless_present`);
                // still, we do not produce a silent default.
                (None, None) => {
                    return Err(headshell_core::Error::new(
                        headshell_core::diag::Stage::IdentityResolve,
                        headshell_core::error::ErrorKind::InvalidInput {
                            detail: "a query or --file must be given".to_owned(),
                        },
                    ));
                }
            };
            render(cli.json, &report, || output::resolve(&report))
        }
        Command::Artwork {
            query,
            all,
            file,
            out,
        } => {
            let registry = provider::default_registry(session.config())?;
            let request = session::ArtworkRequest {
                query: query.clone(),
                all: *all,
                file: file.clone(),
                out: out.clone(),
            };
            let report = session.artwork(&registry, request, lookup_mode).await?;
            // Not found is an answer; a link that failed is a partial failure.
            if report.summary.failed > 0 {
                *succeeded = false;
            }
            render(cli.json, &report, || output::artwork(&report))
        }
        Command::Lyrics {
            query,
            all,
            file,
            print,
            shift,
        } => {
            let registry = provider::default_registry(session.config())?;
            let request = session::LyricsRequest {
                query: query.clone(),
                all: *all,
                file: file.clone(),
                print: *print,
                shift_ms: *shift,
            };
            let report = session.lyrics(&registry, request).await?;
            // Not found is an answer; a link that failed is a partial failure.
            if report.summary.failed > 0 {
                *succeeded = false;
            }
            render(cli.json, &report, || output::lyrics(&report))
        }
        Command::Prefetch { query, all } => {
            let registry = provider::default_registry(session.config())?;
            let report = session.prefetch(&registry, query, *all).await?;
            // A track that could not be made ready is a partial failure.
            if report.summary.failed > 0 {
                *succeeded = false;
            }
            render(cli.json, &report, || output::prefetch(&report))
        }
        Command::Library { command } => match command {
            LibraryCommand::Search {
                query,
                limit,
                min_ms,
            } => {
                let rule = min_ms.map_or_else(PlayRule::default, PlayRule::new);
                let report = session.search(query, *limit, rule)?;
                render(cli.json, &report, || output::search(&report))
            }
        },
        Command::Sleeve { year, out, format } => {
            let query = StatsQuery {
                year: *year,
                top: 10,
                min_ms_played: headshell_core::stats::DEFAULT_MIN_MS_PLAYED,
            };
            let size = format.to_preset().size();
            let response = session.sleeve(query, size, out.as_deref())?;
            render(cli.json, &response, || output::sleeve(&response))
        }
        Command::Provider { command } => {
            let registry = provider::default_registry(session.config())?;
            match command {
                ProviderCommand::List => {
                    let report = session.providers(&registry)?;
                    render(cli.json, &report, || output::provider_list(&report))
                }
                ProviderCommand::Test { name } => {
                    let id = ProviderId::new(name.clone());
                    let report = session.test_provider(&registry, &id).await?;
                    render(cli.json, &report, || output::provider_test(&report))
                }
                ProviderCommand::Use { name, role: None } => {
                    let id = ProviderId::new(name.clone());
                    let report = session.set_play_from(&registry, &id)?;
                    render(cli.json, &report, || output::play_from(&report.play_from))
                }
                ProviderCommand::Use {
                    name,
                    role: Some(role),
                } => {
                    let id = ProviderId::new(name.clone());
                    let report = session.set_role(&registry, role.to_role(), Some(&id))?;
                    render(cli.json, &report, || output::roles(&report.roles))
                }
                ProviderCommand::Follow { role } => {
                    let report = session.set_role(&registry, role.to_role(), None)?;
                    render(cli.json, &report, || output::roles(&report.roles))
                }
                ProviderCommand::Scan { if_stale } => {
                    let report = if *if_stale {
                        session.scan_providers_if_stale(&registry).await?
                    } else {
                        session.scan_providers(&registry).await?
                    };
                    render(cli.json, &report, || output::scan(&report))
                }
                ProviderCommand::Add {
                    kind,
                    url,
                    user,
                    name,
                    api_key,
                    no_verify,
                } => {
                    let kind = provider::remote::ServerKind::parse(kind)?;
                    let id = match name {
                        Some(name) => ProviderId::new(name.clone()),
                        // The suggestion is produced in the core: the GUI will show
                        // the same one (the Golden Rule).
                        None => provider::remote::suggest_id(url, kind),
                    };
                    // The password only from the prompt/environment: taking it as an
                    // argument would write it into the shell history and `ps` output.
                    let password = match api_key {
                        Some(_) => None,
                        None => Some(read_password(&id)?),
                    };
                    let spec = provider::remote::NewServer {
                        id,
                        kind,
                        url: url.clone(),
                        username: user.clone(),
                        password,
                        api_key: api_key.clone(),
                        verify: !*no_verify,
                    };
                    let http = headshell_core::net::default_http_client()?;
                    let report = session.add_server(spec, http).await?;
                    render(cli.json, &report, || output::server_add(&report))
                }
                ProviderCommand::Remove { name } => {
                    let report = session.remove_server(&ProviderId::new(name.clone()))?;
                    render(cli.json, &report, || output::server_remove(&report))
                }
                ProviderCommand::Servers => {
                    let report = session.list_servers()?;
                    render(cli.json, &report, || output::server_list(&report))
                }
            }
        }
        Command::Plugin { command } => match command {
            PluginCommand::List => {
                let report = session.plugins()?;
                render(cli.json, &report, || output::plugin_list(&report))
            }
            PluginCommand::Approve { name } => {
                let report = session.approve_plugin(name)?;
                render(cli.json, &report, || output::plugin_consent(&report))
            }
            PluginCommand::Catalog => {
                let http = headshell_core::net::default_http_client()?;
                let report = session.plugin_catalog(http).await?;
                render(cli.json, &report, || output::plugin_catalog(&report))
            }
            PluginCommand::Install { name } => {
                let http = headshell_core::net::default_http_client()?;
                let report = session.install_plugin(name, http).await?;
                render(cli.json, &report, || output::plugin_install(&report))
            }
            PluginCommand::Update { name } => {
                let http = headshell_core::net::default_http_client()?;
                let report = session.update_plugins(name.as_deref(), http).await?;
                *succeeded = report.summary.failed == 0;
                render(cli.json, &report, || output::plugin_update(&report))
            }
            PluginCommand::Remove { name } => {
                let report = session.remove_plugin(name)?;
                render(cli.json, &report, || output::plugin_remove(&report))
            }
            PluginCommand::Index {
                dir,
                url_template,
                check,
            } => {
                let report = session.build_plugin_index(dir, url_template.as_deref(), *check)?;
                render(cli.json, &report, || output::plugin_index(&report))
            }
            PluginCommand::Disable { name } => {
                let report = session.disable_plugin(name)?;
                render(cli.json, &report, || output::plugin_consent(&report))
            }
            PluginCommand::Enable { name } => {
                let report = session.enable_plugin(name)?;
                render(cli.json, &report, || output::plugin_consent(&report))
            }
            PluginCommand::Forget { name } => {
                let report = session.forget_plugin(name)?;
                render(cli.json, &report, || output::plugin_consent(&report))
            }
        },
        Command::Secret { command } => match command {
            SecretCommand::List => {
                let report = session.secrets()?;
                render(cli.json, &report, || output::secret_list(&report))
            }
            SecretCommand::Set { namespace, key } => {
                // The value is not taken as an argument: the same reasoning as the
                // password (the shell history + `ps` output).
                let value = read_hidden_value(&format!("{namespace} / {key}"))?;
                let report = session.set_secret(namespace, key, &value)?;
                render(cli.json, &report, || output::secret_write(&report))
            }
            SecretCommand::Remove { namespace, key } => {
                let report = session.remove_secret(namespace, key)?;
                render(cli.json, &report, || output::secret_write(&report))
            }
        },
        Command::Suggest { query } => {
            let registry = provider::default_registry(session.config())?;
            let report = session.suggest(&registry, query).await?;
            render(cli.json, &report, || output::suggest(&report))
        }
        Command::Video { query, file } => {
            let registry = provider::default_registry(session.config())?;
            // clap asks for one of the two; an empty query is the core's to
            // refuse, with its stage.
            let target = match file {
                Some(path) => session::VideoTarget::File(path.clone()),
                None => session::VideoTarget::Query(query.clone().unwrap_or_default()),
            };
            let report = session.video_report(&registry, target).await?;
            render(cli.json, &report, || output::video(&report))
        }
        Command::Play {
            query,
            all,
            shuffle,
            dry_run,
            tui: use_tui,
        } => {
            // **No** scan is done: the index is persistent (SQLite `provider_tracks`).
            // The user scans once with `headshell provider scan`; `play` only
            // searches. If the catalog is empty, the error points the user to scanning.
            let registry = provider::default_registry(session.config())?;

            let options = session::PlayOptions {
                all: *all,
                shuffle: *shuffle,
                dry_run: *dry_run,
                ..session::PlayOptions::new(query.clone())
            };

            if *use_tui {
                // The terminal is tested **before** the audio device. Both are
                // needed, but one is a free test and the other a hardware
                // resource; in the reverse order, on a machine without a sound card
                // the user saw an ALSA error even though they had typed `--tui` (K9:
                // the error should describe what the user did).
                tui::require_terminal().map_err(|err| anyhow_to_core(&anyhow::Error::from(err)))?;

                // The TUI only drives the loop; ticks and listen recording are
                // `LiveSession`'s job (in the core, K1).
                let player = session.player_from_search(&registry, options).await?;
                let mut live = LiveSession::new(session, player);
                let recorded = tui::run(&mut live)
                    .await
                    .map_err(|err| anyhow_to_core(&err))?;
                return Ok(format!("listens recorded: {recorded}\n"));
            }

            let report = session.play(&registry, options).await?;
            render(cli.json, &report, || output::play(&report))
        }
        Command::Online { state } => {
            let report = match state {
                Some(state) => session.set_online(state.to_mode())?,
                None => session.online()?,
            };
            render(cli.json, &report, || output::online(&report))
        }
        Command::MusicDirs { action } => {
            let report = match action {
                None => session.music_folders()?,
                Some(MusicDirsCommand::Add { dir }) => session.add_music_folder(dir)?,
                Some(MusicDirsCommand::Remove { dir }) => session.remove_music_folder(dir)?,
            };
            render(cli.json, &report, || {
                output::music_folders(&report, action.is_some())
            })
        }
        Command::Recommend { action, from } => match action {
            None => {
                let registry = provider::default_registry(session.config())?;
                let from = from.as_deref().map(ProviderId::new);
                let report = session
                    .recommend(&registry, from.as_ref(), lookup_mode)
                    .await?;
                // "Could not ask" is a partial failure, not silence (K9).
                if !report.failures.is_empty() {
                    *succeeded = false;
                }
                render(cli.json, &report, || output::recommend(&report))
            }
            Some(RecommendCommand::Dismiss { query }) => {
                let track = TrackRef::parse_query(query)?;
                let report = session.dismiss_recommendation(&track.artist, &track.title)?;
                render(cli.json, &report, || output::recommend_dismiss(&report))
            }
            Some(RecommendCommand::Reset) => {
                let report = session.reset_recommendations()?;
                render(cli.json, &report, || output::recommend_reset(&report))
            }
        },
        Command::Diag => {
            let report = session.last_diag()?;
            match report {
                Some(report) => render(cli.json, &report, || report.render()),
                None => Ok(if cli.json {
                    "null\n".to_owned()
                } else {
                    "no command has been run yet\n".to_owned()
                }),
            }
        }
    }
}

/// Wraps a terminal/I/O error from the TUI in the core error type.
///
/// The TUI is a presentation layer and can use `anyhow` (convention: free in
/// the CLI); but `run` returns the core error type. The stage is
/// [`Stage::PlaybackOutput`]: the surface the user sees is broken.
fn anyhow_to_core(err: &anyhow::Error) -> headshell_core::Error {
    headshell_core::Error::new(
        headshell_core::diag::Stage::PlaybackOutput,
        headshell_core::ErrorKind::Audio {
            detail: format!("terminal interface: {err}"),
        },
    )
}

/// The environment variable the password can be read from instead of the
/// command line.
const PASSWORD_ENV: &str = "HEADSHELL_PASSWORD";

/// The environment variable a secret value can be read from (`headshell
/// secret set`).
const SECRET_ENV: &str = "HEADSHELL_SECRET";

/// Produces a usage error (stage: reading the configuration).
fn input_err(detail: impl Into<String>) -> headshell_core::Error {
    headshell_core::Error::new(
        headshell_core::diag::Stage::ConfigLoad,
        headshell_core::ErrorKind::InvalidInput {
            detail: detail.into(),
        },
    )
}

/// Reads the password from `HEADSHELL_PASSWORD` or from the terminal
/// **without echo**.
///
/// We do not take it as an argument: a password written on the command line
/// ends up in the shell history and the `ps` output. Scripts use the
/// environment variable, people the prompt.
fn read_password(id: &ProviderId) -> headshell_core::Result<String> {
    if let Ok(from_env) = std::env::var(PASSWORD_ENV) {
        if from_env.is_empty() {
            // Counting empty as a password would make the server come back with
            // a meaningless "unauthorised" error.
            return Err(input_err(format!("{PASSWORD_ENV} is set but empty")));
        }
        return Ok(from_env);
    }
    prompt_password(id)
}

/// Reads a secret value from `HEADSHELL_SECRET` or from the terminal
/// **without echo**.
///
/// The same reasoning as the password: a secret written on the command line
/// ends up in the shell history and the `ps` output (D-042).
fn read_hidden_value(label: &str) -> headshell_core::Result<String> {
    if let Ok(from_env) = std::env::var(SECRET_ENV) {
        if from_env.is_empty() {
            return Err(input_err(format!("{SECRET_ENV} is set but empty")));
        }
        return Ok(from_env);
    }
    prompt_hidden(&format!("{label} value"), SECRET_ENV)
}

/// Puts the terminal in raw mode and reads the password without echoing it.
fn prompt_password(id: &ProviderId) -> headshell_core::Result<String> {
    prompt_hidden(&format!("{id} password"), PASSWORD_ENV)
}

/// A prompt without echo. `env_hint`: the variable to suggest to the user if
/// there is no tty.
fn prompt_hidden(label: &str, env_hint: &str) -> headshell_core::Result<String> {
    use crossterm::terminal;
    use std::io::Write as _;

    eprint!("{label}: ");
    let _ = std::io::stderr().flush();

    terminal::enable_raw_mode().map_err(|source| {
        // Without a tty (a pipeline, CI) we do not silently fall back to
        // reading with echo: saying what to do is better than printing the
        // password on screen.
        input_err(format!(
            "could not put the terminal into no-echo mode ({source}); give the value with {env_hint}"
        ))
    })?;
    let secret = read_secret();
    // Raw mode is given back on every path — even on an error, the user's
    // terminal must not be left broken (the same reasoning as the TUI's
    // `TerminalGuard`).
    let _ = terminal::disable_raw_mode();
    eprintln!();
    secret
}

/// Reads a line in raw mode; no key is echoed to the screen.
fn read_secret() -> headshell_core::Result<String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

    let mut secret = String::new();
    loop {
        let event =
            event::read().map_err(|source| input_err(format!("could not read a key: {source}")))?;
        let Event::Key(key) = event else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // In raw mode Ctrl+C produces no signal; we handle cancelling ourselves.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            return Err(input_err("input cancelled"));
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            continue;
        }
        match key.code {
            KeyCode::Enter => break,
            KeyCode::Backspace => {
                secret.pop();
            }
            KeyCode::Char(c) => secret.push(c),
            _ => {}
        }
    }
    if secret.is_empty() {
        return Err(input_err("an empty value is not accepted"));
    }
    Ok(secret)
}

/// If `--json` was given, serialises the data; otherwise uses the human
/// format.
fn render<T: serde::Serialize>(
    json: bool,
    value: &T,
    human: impl FnOnce() -> String,
) -> headshell_core::Result<String> {
    if json {
        let mut text = serde_json::to_string_pretty(value).map_err(|source| {
            headshell_core::Error::new(
                headshell_core::diag::Stage::ConfigLoad,
                headshell_core::ErrorKind::Json {
                    entry: "stdout".to_owned(),
                    source,
                },
            )
        })?;
        text.push('\n');
        Ok(text)
    } else {
        Ok(human())
    }
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!("headshell_core={level},headshell_cli={level}"))
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
