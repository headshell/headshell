//! Links (D-095): an address pasted where a song is asked for, played through
//! the provider that takes links like it.
//!
//! A provider says which links it takes as **templates** — a link written out
//! whole, with `{id}` where the part that names the song stands:
//!
//! ```text
//! https://music.youtube.com/watch?v={id}
//! https://youtu.be/{id}
//! https://soundcloud.com/{id}
//! ```
//!
//! The core reads a pasted link against them and hands the provider what
//! stood in `{id}`, with the template it fitted; the provider answers with the
//! song. The core never fetches the link itself — what it names is the
//! provider's to look up, through its own permissions (K5).
//!
//! ## How a link fits a template
//!
//! - **The host** is compared without its case, and a leading `www.` or `m.`
//!   is dropped from both: `youtube.com`, `www.youtube.com` and
//!   `m.youtube.com` are one site; `music.youtube.com` is another.
//! - **The scheme** (`http`, `https`) and a port do not matter; a fragment
//!   (`#t=42`) is dropped.
//! - **The path**: `{id}` takes everything between the template's text before
//!   and after it — `/` included, since a SoundCloud song is `artist/song`.
//!   A slash at the end of the link is not counted.
//! - **The query**: every pair the template names must be in the link, in any
//!   order; `{id}` stands for a whole value (`v={id}`). Pairs the template
//!   does not name — `&t=42s`, `&si=…` — are left aside.
//!
//! When more than one template of a provider fits, the one naming the most of
//! the link wins. Which provider is asked first is the session's (the one
//! music is played from — `Session::queue_from_search`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::playback::QueueItem;
use crate::provider::Provider;

/// The placeholder a template marks the song's part with.
pub const ID: &str = "{id}";

/// One link template a provider takes, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTemplate {
    /// As the provider wrote it.
    pub template: String,
    host: String,
    path: PathPart,
    query: Vec<(String, QueryValue)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathPart {
    /// The path is this, exactly.
    Exact(String),
    /// `{id}` is in the path, between these two.
    Around { before: String, after: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QueryValue {
    Exact(String),
    Id,
}

/// A pasted text read as a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// As it was given, trimmed.
    pub text: String,
    /// The host, as compared: lowercase, without a leading `www.` or `m.`.
    pub host: String,
    path: String,
    query: Vec<(String, String)>,
}

/// What a template took out of a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkFit {
    /// What stood in `{id}`, as it stands in the link.
    pub id: String,
    /// The template it fitted, as the provider wrote it.
    pub template: String,
}

impl LinkTemplate {
    /// Reads a template.
    ///
    /// # Errors
    /// If it is not an `http`/`https` address, its host is missing or carries
    /// a port or a user, it has a fragment, `{id}` is not in it exactly once,
    /// `{id}` is not in the path nor a whole query value, or another brace is
    /// left in it. The text says which.
    pub fn parse(template: &str) -> std::result::Result<Self, String> {
        let raw = template.trim();
        let fail = |why: &str| Err(format!("link template {raw:?}: {why}"));
        let Some(rest) = strip_scheme(raw) else {
            return fail("it must begin with `https://` or `http://`");
        };
        if rest.contains('#') {
            return fail("a template has no `#` part");
        }
        if raw.matches(ID).count() != 1 {
            return fail("`{id}` must be in it exactly once");
        }
        if raw.replace(ID, "").contains(['{', '}']) {
            return fail("`{id}` is the only placeholder");
        }
        if rest.chars().any(char::is_whitespace) {
            return fail("a template has no spaces");
        }

        let (before_query, query) = match rest.split_once('?') {
            Some((head, query)) => (head, Some(query)),
            None => (rest, None),
        };
        let (authority, path) = match before_query.find('/') {
            Some(at) => before_query.split_at(at),
            None => (before_query, "/"),
        };
        if authority.is_empty() {
            return fail("it has no host");
        }
        if authority.contains(ID) {
            return fail("`{id}` cannot stand in the host");
        }
        if authority.contains(['@', ':']) {
            return fail("the host is written without a user or a port");
        }
        if !authority
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return fail("the host is a plain name (letters, digits, `.` and `-`)");
        }

        let path = match path.split_once(ID) {
            Some((before, after)) => PathPart::Around {
                before: before.to_owned(),
                // The link's slash at the end is not counted: nor is this.
                after: after.trim_end_matches('/').to_owned(),
            },
            None => PathPart::Exact(trimmed_path(path).to_owned()),
        };

        let mut pairs: Vec<(String, QueryValue)> = Vec::new();
        for piece in query.unwrap_or_default().split('&') {
            if piece.is_empty() {
                continue;
            }
            let (key, value) = piece.split_once('=').unwrap_or((piece, ""));
            if key.is_empty() || key.contains(ID) {
                return fail("a query pair names its key");
            }
            if pairs.iter().any(|(seen, _)| seen == key) {
                return fail("a query key is named twice");
            }
            let value = if value == ID {
                QueryValue::Id
            } else if value.contains(ID) {
                return fail("`{id}` stands for a whole query value (`v={id}`)");
            } else {
                QueryValue::Exact(value.to_owned())
            };
            pairs.push((key.to_owned(), value));
        }

        Ok(Self {
            template: raw.to_owned(),
            host: site(authority),
            path,
            query: pairs,
        })
    }

    /// What stands in `{id}` when `link` fits this template; `None` when it
    /// does not fit, or `{id}` would be empty.
    #[must_use]
    pub fn fit(&self, link: &Link) -> Option<String> {
        if self.host != link.host {
            return None;
        }
        let mut id = match &self.path {
            PathPart::Exact(path) => {
                if *path != link.path {
                    return None;
                }
                None
            }
            PathPart::Around { before, after } => {
                let middle = link.path.strip_prefix(before.as_str())?;
                let middle = middle.strip_suffix(after.as_str())?;
                if middle.is_empty() {
                    return None;
                }
                Some(middle.to_owned())
            }
        };
        for (key, wanted) in &self.query {
            let value = link
                .query
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value)?;
            match wanted {
                QueryValue::Exact(exact) if exact == value => {}
                QueryValue::Exact(_) => return None,
                QueryValue::Id if value.is_empty() => return None,
                QueryValue::Id => id = Some(value.clone()),
            }
        }
        id
    }

    /// How much of a link this template names: of two that fit, the one
    /// naming more is the closer.
    fn weight(&self) -> usize {
        let path = match &self.path {
            PathPart::Exact(path) => path.len(),
            PathPart::Around { before, after } => before.len() + after.len(),
        };
        let query: usize = self
            .query
            .iter()
            .map(|(key, value)| {
                key.len()
                    + match value {
                        QueryValue::Exact(exact) => exact.len(),
                        QueryValue::Id => 0,
                    }
            })
            .sum();
        self.host.len() + path + query
    }

    /// The site it is a template of, as links are compared (`youtube.com`).
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
}

impl Link {
    /// Reads a text as a link: an `http`/`https` address with a host and no
    /// spaces. `None` for anything else — the text is a search then.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let rest = strip_scheme(text)?;
        if rest.chars().any(char::is_whitespace) {
            return None;
        }
        let rest = rest.split_once('#').map_or(rest, |(head, _)| head);
        let (before_query, query) = match rest.split_once('?') {
            Some((head, query)) => (head, query),
            None => (rest, ""),
        };
        let (authority, path) = match before_query.find('/') {
            Some(at) => before_query.split_at(at),
            None => (before_query, "/"),
        };
        // A user before the host (`a@b.com`) is not the host.
        let authority = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        if authority.starts_with('[') {
            // An IPv6 address: no provider's template names one.
            return None;
        }
        let name = authority
            .split_once(':')
            .map_or(authority, |(name, _)| name);
        if name.is_empty() {
            return None;
        }
        let query = query
            .split('&')
            .filter(|piece| !piece.is_empty())
            .map(|piece| {
                let (key, value) = piece.split_once('=').unwrap_or((piece, ""));
                (key.to_owned(), value.to_owned())
            })
            .collect();
        Some(Self {
            text: text.to_owned(),
            host: site(name),
            path: trimmed_path(path).to_owned(),
            query,
        })
    }
}

/// The closest of `templates` that `link` fits, with what stood in `{id}`.
#[must_use]
pub fn fit_any(templates: &[LinkTemplate], link: &Link) -> Option<LinkFit> {
    templates
        .iter()
        .filter_map(|template| {
            template
                .fit(link)
                .map(|id| (template.weight(), template, id))
        })
        // The first of two equally close ones: `max_by_key` keeps the last.
        .fold(
            None::<(usize, &LinkTemplate, String)>,
            |best, next| match &best {
                Some((weight, _, _)) if *weight >= next.0 => best,
                _ => Some(next),
            },
        )
        .map(|(_, template, id)| LinkFit {
            id,
            template: template.template.clone(),
        })
}

/// The provider a link is given to: the first in `order` (the session's
/// search order — the one music is played from first) with a template the
/// link fits, and what that template took out of it.
///
/// # Errors
/// `LINK_OPEN` when none in `order` takes it; the text names the sites the
/// providers in `all` take links from, so "no plugin for this site" and "a
/// plugin that isn't asked" read apart (K9).
pub(crate) fn taker(
    order: &[Arc<dyn Provider>],
    all: &[Arc<dyn Provider>],
    link: &Link,
) -> Result<(Arc<dyn Provider>, LinkFit)> {
    for provider in order {
        if let Some(fit) = fit_any(&provider.link_templates(), link) {
            return Ok((Arc::clone(provider), fit));
        }
    }
    let takers: Vec<String> = all
        .iter()
        .filter_map(|provider| {
            let mut sites: Vec<String> = Vec::new();
            for template in provider.link_templates() {
                if !sites.iter().any(|site| site == template.host()) {
                    sites.push(template.host().to_owned());
                }
            }
            (!sites.is_empty()).then(|| format!("{} ({})", provider.info().id, sites.join(", ")))
        })
        .collect();
    let known = if takers.is_empty() {
        "no source here takes links; a plugin that does names them in its manifest (`headshell plugin catalog`)".to_owned()
    } else {
        format!("the sources here that take links: {}", takers.join("; "))
    };
    Err(Error::new(
        Stage::LinkOpen,
        ErrorKind::NotFound {
            what: format!(
                "a source that plays links from {} like {} — {known}",
                link.host, link.text
            ),
        },
    ))
}

/// Asks `provider` what is at `link`, and takes the song from its answer.
///
/// # Errors
/// The provider's own failure, in its own stage; `LINK_OPEN` when it found
/// nothing there, or found a list — only a song's link is played for now
/// (D-095).
pub(crate) async fn open(
    provider: &Arc<dyn Provider>,
    link: &Link,
    fit: &LinkFit,
) -> Result<QueueItem> {
    let info = provider.info();
    let mut tracks = provider.open_link(&fit.id, &fit.template).await?;
    match tracks.len() {
        0 => Err(Error::new(
            Stage::LinkOpen,
            ErrorKind::NotFound {
                what: format!(
                    "a song at {} — {} looked and found nothing there",
                    link.text, info.display_name
                ),
            },
        )),
        1 => {
            let track = tracks.remove(0);
            Ok(QueueItem {
                id: track.id,
                track: track.track,
            })
        }
        many => Err(Error::new(
            Stage::LinkOpen,
            ErrorKind::InvalidInput {
                detail: format!(
                    "{} is a list of {many} songs at {}; only a song's link is played for now",
                    link.text, info.display_name
                ),
            },
        )),
    }
}

/// The text after `http://` or `https://`, whatever their case.
fn strip_scheme(text: &str) -> Option<&str> {
    ["https://", "http://"].into_iter().find_map(|scheme| {
        text.get(..scheme.len())
            .filter(|head| head.eq_ignore_ascii_case(scheme))
            .map(|_| &text[scheme.len()..])
    })
}

/// A host as hosts are compared: lowercase, without a trailing dot and a
/// leading `www.` or `m.`.
fn site(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    ["www.", "m."]
        .into_iter()
        .find_map(|prefix| host.strip_prefix(prefix).map(str::to_owned))
        .unwrap_or(host)
}

/// A path without the slash at its end; `/` stays.
fn trimmed_path(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() && path.starts_with('/') {
        "/"
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(text: &str) -> LinkTemplate {
        LinkTemplate::parse(text).unwrap()
    }

    fn fit(template_text: &str, link: &str) -> Option<String> {
        template(template_text).fit(&Link::parse(link).unwrap())
    }

    #[test]
    fn a_query_template_takes_its_value_whatever_else_the_link_carries() {
        let watch = "https://www.youtube.com/watch?v={id}";
        for link in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=dQw4w9WgXcQ",
            "http://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "HTTPS://WWW.YouTube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?list=RD123&v=dQw4w9WgXcQ&t=42s",
            "https://www.youtube.com/watch/?v=dQw4w9WgXcQ#comments",
            "  https://www.youtube.com:443/watch?v=dQw4w9WgXcQ  ",
        ] {
            assert_eq!(fit(watch, link).as_deref(), Some("dQw4w9WgXcQ"), "{link}");
        }
    }

    #[test]
    fn another_site_another_path_or_an_empty_value_does_not_fit() {
        let watch = "https://www.youtube.com/watch?v={id}";
        for link in [
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/results?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=",
            "https://www.youtube.com/watch?list=RD123",
            "https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ",
        ] {
            assert_eq!(fit(watch, link), None, "{link}");
        }
    }

    #[test]
    fn a_path_template_takes_the_part_between_slashes_included() {
        assert_eq!(
            fit(
                "https://youtu.be/{id}",
                "https://youtu.be/dQw4w9WgXcQ?si=abc"
            )
            .as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            fit(
                "https://soundcloud.com/{id}",
                "https://soundcloud.com/ezhel/geceler/?utm_source=clipboard"
            )
            .as_deref(),
            Some("ezhel/geceler")
        );
        assert_eq!(
            fit("https://soundcloud.com/{id}", "https://soundcloud.com/"),
            None
        );
        assert_eq!(
            fit(
                "https://example.com/track/{id}/play",
                "https://example.com/track/42/play/"
            )
            .as_deref(),
            Some("42")
        );
        assert_eq!(
            fit(
                "https://example.com/track/{id}/play",
                "https://example.com/track//play"
            ),
            None
        );
    }

    #[test]
    fn a_user_before_the_host_is_not_the_host() {
        assert_eq!(
            fit(
                "https://soundcloud.com/{id}",
                "https://soundcloud.com@evil.example/a/b"
            ),
            None
        );
    }

    #[test]
    fn literal_query_pairs_must_be_there() {
        let t = "https://example.com/play?kind=song&id={id}";
        assert_eq!(
            fit(t, "https://example.com/play?id=7&kind=song").as_deref(),
            Some("7")
        );
        assert_eq!(fit(t, "https://example.com/play?id=7&kind=list"), None);
        assert_eq!(fit(t, "https://example.com/play?id=7"), None);
    }

    #[test]
    fn the_closest_template_wins_and_the_first_of_equals() {
        let templates = [
            template("https://example.com/{id}"),
            template("https://example.com/song/{id}"),
            template("https://example.com/song/{id}/"),
        ];
        let link = Link::parse("https://example.com/song/42").unwrap();
        assert_eq!(
            fit_any(&templates, &link),
            Some(LinkFit {
                id: "42".to_owned(),
                template: "https://example.com/song/{id}".to_owned(),
            })
        );
        let other = Link::parse("https://example.com/user/x").unwrap();
        assert_eq!(fit_any(&templates, &other).unwrap().id, "user/x");
        assert_eq!(
            fit_any(&templates, &Link::parse("https://other.example/x").unwrap()),
            None
        );
    }

    #[test]
    fn only_an_http_address_is_a_link() {
        for text in [
            "Tame Impala - Let It Happen",
            "youtu.be/dQw4w9WgXcQ",
            "ftp://example.com/x",
            "https://",
            "https://example.com/a song",
            "https://[::1]/x",
        ] {
            assert_eq!(Link::parse(text), None, "{text}");
        }
        assert_eq!(
            Link::parse("https://Example.COM./x").unwrap().host,
            "example.com"
        );
    }

    #[test]
    fn a_template_says_what_is_wrong_with_it() {
        for (text, says) in [
            ("youtu.be/{id}", "must begin with"),
            ("https://youtu.be/x", "exactly once"),
            ("https://youtu.be/{id}/{id}", "exactly once"),
            ("https://youtu.be/{id}/{n}", "only placeholder"),
            ("https:///{id}", "no host"),
            ("https://{id}.example.com/", "host"),
            ("https://example.com:8080/{id}", "without a user or a port"),
            ("https://ex_ample.com/{id}", "plain name"),
            ("https://example.com/{id}#x", "`#`"),
            ("https://example.com/watch?v=x{id}", "whole query value"),
            ("https://example.com/watch?v={id}&v=1", "twice"),
            ("https://example.com/a b/{id}", "no spaces"),
        ] {
            let err = LinkTemplate::parse(text).unwrap_err();
            assert!(err.contains(says), "{text}: {err}");
        }
    }
}
