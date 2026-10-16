//! What the play box offers while it is typed in (D-080).
//!
//! The order is the user's: typing "let it" first brings **the tracks whose
//! title begins with it** ("Let It Happen"), then **the artists** — those
//! whose name begins with it, and the artists of those tracks — then
//! **every other match**. The words are compared the way the identity chain
//! compares them (`normalize_text`), with one addition for typing: an
//! apostrophe is dropped, not turned into a space. "İstanbul" and "istanbul"
//! are the same beginning, and so are "Don't" and "dont".
//!
//! Who is asked is the session's (`Session::suggest_plan`): the provider
//! chosen to play from, and only when it has nothing, every other one — each
//! answer a section of its own, named. The plan carries the catalog's
//! answers already read; asking the rest goes over the network and needs no
//! session ([`SuggestPlan::finish`]), so a shell can ask them off the thread
//! that plays — typing must not hold the player's rounds. The ranking here
//! is pure: one provider's hits in, three lists out.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::identity::normalize::{normalize_artist, normalize_text};
use crate::ids::ProviderId;
use crate::playback::QueueItem;
use crate::provider::Provider;
use crate::session::PlayFrom;

/// How many tracks whose title begins with the text are offered first.
pub const TRACKS: usize = 4;
/// How many artists are offered.
pub const ARTISTS: usize = 3;
/// How many of the other matches are offered.
pub const MORE: usize = 5;
/// How many tracks a provider's own search is asked for: fewer than a
/// play's, since it goes over the network at every pause in typing.
pub const SEARCH_ROWS: usize = 20;

/// An artist offered: picking it plays every track that matches the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistSuggestion {
    pub artist: String,
}

/// One provider's answer, ranked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestSection {
    pub provider: ProviderId,
    /// The name the user knows it by ("YouTube Music").
    pub display_name: String,
    /// Asked because the provider chosen to play from had nothing.
    pub fallback: bool,
    /// Tracks whose title begins with what was typed — the likeliest.
    pub tracks: Vec<QueueItem>,
    /// Artists whose name begins with it, then the artists of `tracks`.
    pub artists: Vec<ArtistSuggestion>,
    /// Every other match, in the provider's own order.
    pub more: Vec<QueueItem>,
}

impl SuggestSection {
    /// Nothing to offer from this provider.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.artists.is_empty() && self.more.is_empty()
    }
}

/// A provider that could not be asked: "not found" and "could not ask" are
/// different answers (K9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestFailure {
    pub provider: ProviderId,
    pub display_name: String,
    /// The stage it stopped at (`PROVIDER_CALL`, `NETWORK_REQUEST`…).
    pub stage: String,
    /// The whole chain, as `headshell diag` would print it.
    pub chain: String,
}

/// The suggestions for a text.
///
/// Not a recorded run: it is asked at every pause in typing, and `headshell
/// diag`'s last run must stay the last command the user ran. What went wrong
/// is in `failures` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestReport {
    pub query: String,
    /// Who was asked first.
    pub play_from: PlayFrom,
    /// The chosen provider's section when it had something; otherwise one per
    /// other provider that had something, in the registry's order.
    pub sections: Vec<SuggestSection>,
    pub failures: Vec<SuggestFailure>,
    /// The text was a link (D-095): the sections hold the song at it, from
    /// the provider that takes it. Absent for a text that is not a link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<SuggestLink>,
}

/// What became of a link typed in the play box (D-095).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestLink {
    /// The template it fitted and what stood in `{id}`; `None` when no
    /// provider takes it.
    pub fit: Option<crate::link::LinkFit>,
    /// Why it cannot be opened when no provider takes it — the whole chain,
    /// as `headshell diag` would print it (stage `LINK_OPEN`, K9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unclaimed: Option<String>,
}

/// Who is asked for a text, in order, with the catalog's answers in hand
/// (`Session::suggest_plan`). [`SuggestPlan::finish`] asks the rest.
pub struct SuggestPlan {
    pub(crate) query: String,
    pub(crate) play_from: PlayFrom,
    pub(crate) steps: Vec<SuggestStep>,
    /// The text is a link (D-095): its one step is the provider that takes
    /// it, and that provider is asked to open it instead of searched.
    pub(crate) link: Option<SuggestLinkPlan>,
}

/// A link and the provider's template it fitted (D-095).
pub(crate) struct SuggestLinkPlan {
    pub(crate) link: crate::link::Link,
    pub(crate) fit: Option<crate::link::LinkFit>,
    pub(crate) unclaimed: Option<String>,
}

/// One provider to ask.
pub(crate) struct SuggestStep {
    pub(crate) provider: Arc<dyn Provider>,
    /// Its rows in the scanned catalog; `None`: never scanned, its own
    /// search is asked.
    pub(crate) catalog: Option<Vec<QueueItem>>,
}

impl SuggestPlan {
    /// Asks whoever is still to be asked — in order, the chosen one first and
    /// the others only when it had nothing — and ranks every answer.
    ///
    /// A provider that cannot be asked is in `failures` with its stage and
    /// chain (K9); the next one is still asked.
    pub async fn finish(self) -> SuggestReport {
        let mut report = SuggestReport {
            query: self.query,
            play_from: self.play_from,
            sections: Vec::new(),
            failures: Vec::new(),
            link: None,
        };
        if let Some(plan) = self.link {
            for ask in self.steps {
                let info = ask.provider.info();
                let Some(fit) = &plan.fit else { break };
                match crate::link::open(&ask.provider, &plan.link, fit).await {
                    Ok(song) => report.sections.push(SuggestSection {
                        provider: info.id,
                        display_name: info.display_name,
                        fallback: false,
                        tracks: vec![song],
                        artists: Vec::new(),
                        more: Vec::new(),
                    }),
                    Err(err) => report.failures.push(SuggestFailure {
                        provider: info.id,
                        display_name: info.display_name,
                        stage: err.stage().as_str().to_owned(),
                        chain: err.chain_text(),
                    }),
                }
            }
            report.link = Some(SuggestLink {
                fit: plan.fit,
                unclaimed: plan.unclaimed,
            });
            return report;
        }
        for (step, ask) in self.steps.into_iter().enumerate() {
            let info = ask.provider.info();
            let hits = match ask.catalog {
                Some(rows) => Ok(rows),
                None => ask
                    .provider
                    .search(&report.query, SEARCH_ROWS)
                    .await
                    .map(|hits| {
                        hits.into_iter()
                            .map(|hit| QueueItem {
                                id: hit.id,
                                track: hit.track,
                            })
                            .collect()
                    }),
            };
            match hits {
                Ok(hits) => {
                    let (tracks, artists, more) = rank(&report.query, hits);
                    let section = SuggestSection {
                        provider: info.id,
                        display_name: info.display_name,
                        fallback: step > 0,
                        tracks,
                        artists,
                        more,
                    };
                    if !section.is_empty() {
                        report.sections.push(section);
                    }
                }
                Err(err) => report.failures.push(SuggestFailure {
                    provider: info.id,
                    display_name: info.display_name,
                    stage: err.stage().as_str().to_owned(),
                    chain: err.chain_text(),
                }),
            }
            // The chosen one had something: the others are not asked.
            if step == 0 && !report.sections.is_empty() {
                break;
            }
        }
        report
    }
}

/// Ranks one provider's hits for `query` into a section.
///
/// The hits come in the provider's order (the catalog's relevance, the
/// service's own); that order is kept inside each list. The same song twice
/// — two files, a re-upload — is offered once.
#[must_use]
pub fn rank(
    query: &str,
    hits: Vec<QueueItem>,
) -> (Vec<QueueItem>, Vec<ArtistSuggestion>, Vec<QueueItem>) {
    let wanted = folded(query);
    let mut seen = std::collections::HashSet::new();
    let mut tracks = Vec::new();
    let mut more = Vec::new();
    for hit in hits {
        let key = (
            normalize_artist(&hit.track.artist),
            normalize_text(&hit.track.title),
        );
        if !seen.insert(key) {
            continue;
        }
        let begins = !wanted.is_empty() && folded(&hit.track.title).starts_with(&wanted);
        if begins && tracks.len() < TRACKS {
            tracks.push(hit);
        } else {
            more.push(hit);
        }
    }

    // Artists whose name — or a word of it — begins with the text: "tame"
    // and "impala" both find Tame Impala. Then the artists of the tracks
    // offered first: "let it" offers Tame Impala after "Let It Happen".
    let mut artists: Vec<ArtistSuggestion> = Vec::new();
    let mut offered = std::collections::HashSet::new();
    let named = more.iter().chain(tracks.iter()).filter(|hit| {
        let name = folded(&hit.track.artist);
        !wanted.is_empty()
            && (name.starts_with(&wanted) || name.split(' ').any(|word| word.starts_with(&wanted)))
    });
    for hit in named.chain(tracks.iter()) {
        if artists.len() == ARTISTS {
            break;
        }
        if offered.insert(normalize_artist(&hit.track.artist)) {
            artists.push(ArtistSuggestion {
                artist: hit.track.artist.clone(),
            });
        }
    }

    more.truncate(MORE);
    (tracks, artists, more)
}

/// A text as a beginning is looked for in it: the identity chain's words,
/// an apostrophe dropped first — `normalize_text` turns it into a space, and
/// "dont" would never begin "Don't".
fn folded(text: &str) -> String {
    normalize_text(&text.replace(['\'', '\u{2019}'], ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProviderTrackId;
    use crate::model::TrackRef;

    fn hit(artist: &str, title: &str) -> QueueItem {
        QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), format!("{artist}/{title}")),
            track: TrackRef::new(artist, title),
        }
    }

    fn titles(items: &[QueueItem]) -> Vec<&str> {
        items.iter().map(|item| item.track.title.as_str()).collect()
    }

    fn names(artists: &[ArtistSuggestion]) -> Vec<&str> {
        artists
            .iter()
            .map(|artist| artist.artist.as_str())
            .collect()
    }

    /// The desktop asks the network part off the thread that plays: a plan
    /// must cross threads, and so must its finishing. This fails to compile
    /// if either stops being `Send`.
    #[test]
    fn a_plan_and_its_finishing_cross_threads() {
        fn sendable<T: Send + 'static>() {}
        fn finishing(plan: SuggestPlan) -> impl std::future::Future<Output = SuggestReport> + Send {
            plan.finish()
        }
        sendable::<SuggestPlan>();
        let _ = finishing;
    }

    #[test]
    fn let_it_offers_the_track_then_its_artist_then_the_rest() {
        // The user's own example, in the catalog's order: a match on another
        // word comes first there, and must not stay first here.
        let (tracks, artists, more) = rank(
            "let it",
            vec![
                hit("The Beatles", "Two of Us (Let It Be sessions)"),
                hit("Tame Impala", "Let It Happen"),
                hit("Ray LaMontagne", "Let It Be Me"),
            ],
        );
        assert_eq!(titles(&tracks), ["Let It Happen", "Let It Be Me"]);
        assert_eq!(names(&artists), ["Tame Impala", "Ray LaMontagne"]);
        assert_eq!(titles(&more), ["Two of Us (Let It Be sessions)"]);
    }

    #[test]
    fn an_artists_name_is_offered_before_their_tracks() {
        // "tame": no title begins with it, so the artist comes first and their
        // tracks follow among the rest.
        let (tracks, artists, more) = rank(
            "tame",
            vec![
                hit("Tame Impala", "Eventually"),
                hit("Tame Impala", "Let It Happen"),
            ],
        );
        assert!(tracks.is_empty());
        assert_eq!(names(&artists), ["Tame Impala"]);
        assert_eq!(titles(&more), ["Eventually", "Let It Happen"]);
        // A word of the name finds it too.
        let (_, artists, _) = rank("impala", vec![hit("Tame Impala", "Eventually")]);
        assert_eq!(names(&artists), ["Tame Impala"]);
    }

    #[test]
    fn beginnings_are_compared_as_the_identity_chain_compares_words() {
        let (tracks, artists, _) = rank(
            "ISTANBUL",
            vec![
                hit("Sezen Aksu", "İstanbul Hatırası"),
                hit("Şebnem Ferah", "Sil Baştan"),
            ],
        );
        assert_eq!(titles(&tracks), ["İstanbul Hatırası"]);
        assert_eq!(names(&artists), ["Sezen Aksu"]);
        let (tracks, ..) = rank("dont", vec![hit("Journey", "Don\u{2019}t Stop Believin'")]);
        assert_eq!(titles(&tracks), ["Don\u{2019}t Stop Believin'"]);
        let (tracks, ..) = rank("don't s", vec![hit("Journey", "Don't Stop Believin'")]);
        assert_eq!(titles(&tracks), ["Don't Stop Believin'"]);
        // "The" is part of a name as it is typed.
        let (_, artists, _) = rank("the bea", vec![hit("The Beatles", "Help!")]);
        assert_eq!(names(&artists), ["The Beatles"]);
    }

    #[test]
    fn the_same_song_twice_is_offered_once() {
        let (tracks, artists, more) = rank(
            "creep",
            vec![
                hit("Radiohead", "Creep"),
                hit("radiohead", "Creep (Remastered)"),
                hit("Radiohead", "Creep"),
            ],
        );
        assert_eq!(titles(&tracks), ["Creep"]);
        assert_eq!(names(&artists), ["Radiohead"]);
        assert!(more.is_empty(), "{more:?}");
    }

    #[test]
    fn every_list_keeps_to_its_size_and_nothing_typed_offers_no_beginnings() {
        let many: Vec<QueueItem> = (0..20)
            .map(|n| hit(&format!("Artist {n}"), &format!("Song {n}")))
            .collect();
        let (tracks, artists, more) = rank("song", many);
        assert_eq!(tracks.len(), TRACKS);
        assert_eq!(artists.len(), ARTISTS);
        assert_eq!(more.len(), MORE);
        // Overflow of the first list is not lost: it leads the rest.
        assert_eq!(more[0].track.title, "Song 4");

        let (tracks, artists, more) = rank("  ", vec![hit("A", "B")]);
        assert!(tracks.is_empty() && artists.is_empty());
        assert_eq!(titles(&more), ["B"]);
    }
}
