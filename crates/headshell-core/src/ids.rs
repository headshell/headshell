//! Type-safe identifiers.
//!
//! The rule: identifiers are not `String`s. You cannot pass a
//! `ProviderTrackId` where a `CanonicalId` is expected — the compiler stops
//! you.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A provider-independent track identity.
///
/// Order of preference: MusicBrainz Recording ID → derived from the ISRC →
/// derived locally. Which one it is can be read with [`CanonicalId::kind`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanonicalId(String);

/// Which authority a [`CanonicalId`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalKind {
    /// MusicBrainz Recording MBID — the gold standard.
    Mbid,
    /// Derived from the ISRC; valid until an MBID is found.
    Isrc,
    /// Derived locally (a hash of the normalised artist+title). Its weakest form.
    Local,
}

impl CanonicalId {
    /// A canonical identity from a MusicBrainz Recording MBID.
    #[must_use]
    pub fn from_mbid(mbid: &Mbid) -> Self {
        Self(format!("mb:{}", mbid.as_str()))
    }

    /// A canonical identity from an ISRC. The interim state until an MBID is
    /// found.
    #[must_use]
    pub fn from_isrc(isrc: &Isrc) -> Self {
        Self(format!("isrc:{}", isrc.as_str()))
    }

    /// A locally derived identity. `key` must be the normalised
    /// "artist\u{1}title".
    #[must_use]
    pub fn from_local_key(key: &str) -> Self {
        Self(format!("local:{}", fnv1a64_hex(key)))
    }

    /// Which authority the identity comes from.
    #[must_use]
    pub fn kind(&self) -> CanonicalKind {
        match self.0.split_once(':') {
            Some(("mb", _)) => CanonicalKind::Mbid,
            Some(("isrc", _)) => CanonicalKind::Isrc,
            _ => CanonicalKind::Local,
        }
    }

    /// The raw form for storage/serialisation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Wraps the raw form read from storage back up.
    ///
    /// Only to be used when reading back data `headshell` itself wrote; for
    /// outside input use the `from_*` constructors.
    #[must_use]
    pub fn from_stored(raw: String) -> Self {
        Self(raw)
    }
}

impl fmt::Display for CanonicalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A provider's own track id. Meaningless without the provider's name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProviderTrackId {
    pub provider: ProviderId,
    pub id: String,
}

impl ProviderTrackId {
    #[must_use]
    pub fn new(provider: ProviderId, id: impl Into<String>) -> Self {
        Self {
            provider,
            id: id.into(),
        }
    }
}

impl fmt::Display for ProviderTrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.id)
    }
}

/// A provider's name (`local`, `subsonic`, `soundcloud`, ...).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The key a cover is cached and asked for under (D-076): the album it
/// belongs to, or the track itself when it has no album.
///
/// Built only by the core ([`crate::artwork::key_for`]) from the normalised
/// artist and album, so the same album is one cover — looked up once — even
/// when it reaches the queue from two providers. The shells treat it as
/// opaque: `album:` or `track:` and sixteen hex digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtworkKey(String);

impl ArtworkKey {
    /// The key of an album: its normalised artist and title.
    #[must_use]
    pub(crate) fn album(artist: &str, album: &str) -> Self {
        Self(format!(
            "album:{}",
            fnv1a64_hex(&format!("{artist}\u{1}{album}"))
        ))
    }

    /// The key of a track without an album: its normalised artist and title.
    #[must_use]
    pub(crate) fn track(artist: &str, title: &str) -> Self {
        Self(format!(
            "track:{}",
            fnv1a64_hex(&format!("{artist}\u{1}{title}"))
        ))
    }

    /// Reads a key a shell sends back. Only the shape is checked: a key that
    /// was never made here finds nothing in the cache, it does not reach the
    /// disk.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let (kind, hash) = raw.split_once(':')?;
        let shaped = matches!(kind, "album" | "track")
            && hash.len() == 16
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        shaped.then(|| Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtworkKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The key a track's lyrics — and the user's shift of them — are kept under
/// (D-078): the track as its provider knows it.
///
/// Not the album's, as a cover's is: lyrics belong to one recording, and
/// their timing to one release of it — a remaster with a longer silence at
/// its start is another timing. The shells treat it as opaque: `lyrics:` and
/// sixteen hex digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LyricsKey(String);

impl LyricsKey {
    /// The key of a track: its provider and the provider's id for it.
    #[must_use]
    pub fn of(id: &ProviderTrackId) -> Self {
        Self(format!(
            "lyrics:{}",
            fnv1a64_hex(&format!("{}\u{1}{}", id.provider, id.id))
        ))
    }

    /// Reads a key a shell sends back. Only the shape is checked, as for
    /// [`ArtworkKey::parse`].
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let hash = raw.strip_prefix("lyrics:")?;
        let shaped = hash.len() == 16
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        shaped.then(|| Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LyricsKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// ISRC — 12 characters: 2 country + 3 registrant + 2 year + 5 designation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Isrc(String);

impl Isrc {
    /// Normalises the input (dashes/spaces dropped, upper-cased) and validates it.
    ///
    /// `None` if the format does not fit — silently accepting it would corrupt
    /// the first link of the identity chain.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let cleaned: String = raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        if cleaned.len() != 12 {
            return None;
        }
        let b = cleaned.as_bytes();
        let shape_ok = b[0..2].iter().all(u8::is_ascii_alphabetic)
            && b[2..5].iter().all(u8::is_ascii_alphanumeric)
            && b[5..12].iter().all(u8::is_ascii_digit);
        shape_ok.then_some(Self(cleaned))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Isrc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A MusicBrainz identifier (UUID format).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Mbid(String);

impl Mbid {
    /// Validates the UUID format (8-4-4-4-12 hexadecimal).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let lower = raw.trim().to_ascii_lowercase();
        let groups: Vec<&str> = lower.split('-').collect();
        let expected = [8usize, 4, 4, 4, 12];
        if groups.len() != expected.len() {
            return None;
        }
        let ok = groups
            .iter()
            .zip(expected)
            .all(|(g, len)| g.len() == len && g.bytes().all(|c| c.is_ascii_hexdigit()));
        ok.then_some(Self(lower))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Mbid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// FNV-1a 64 bit. For deriving local identities; not cryptographic, just
/// stable.
fn fnv1a64_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isrc_normalizes_and_validates() {
        assert_eq!(
            Isrc::parse("us-rc1-17-00001").unwrap().as_str(),
            "USRC11700001"
        );
        assert_eq!(
            Isrc::parse("usrc11700001").unwrap().as_str(),
            "USRC11700001"
        );
        assert!(
            Isrc::parse("USRC1170000").is_none(),
            "11 characters must be rejected"
        );
        assert!(
            Isrc::parse("12RC11700001").is_none(),
            "the country code must be letters"
        );
        assert!(
            Isrc::parse("USRC1170000X").is_none(),
            "the last 7 characters must be digits"
        );
    }

    #[test]
    fn an_artwork_key_is_one_per_album_and_only_its_shape_is_read_back() {
        let a = ArtworkKey::album("radiohead", "pablo honey");
        assert_eq!(a, ArtworkKey::album("radiohead", "pablo honey"));
        assert_ne!(a, ArtworkKey::album("radiohead", "the bends"));
        assert_ne!(
            ArtworkKey::album("a", "b"),
            ArtworkKey::track("a", "b"),
            "an album and a track of the same name are different covers"
        );
        assert_eq!(ArtworkKey::parse(a.as_str()), Some(a.clone()));
        assert!(a.as_str().starts_with("album:"));
        assert_eq!(ArtworkKey::parse("album:../../etc/passwd"), None);
        assert_eq!(
            ArtworkKey::parse("album:0123456789ABCDEF"),
            None,
            "upper-case hex is not ours"
        );
        assert_eq!(ArtworkKey::parse("cover:0123456789abcdef"), None);
    }

    #[test]
    fn a_lyrics_key_is_one_per_provider_track_and_only_its_shape_is_read_back() {
        let local = ProviderTrackId::new(ProviderId::new("local"), "/music/a.flac");
        let key = LyricsKey::of(&local);
        assert_eq!(key, LyricsKey::of(&local.clone()));
        assert_ne!(
            key,
            LyricsKey::of(&ProviderTrackId::new(
                ProviderId::new("navidrome"),
                "/music/a.flac"
            )),
            "the same id at another provider is another track"
        );
        assert!(key.as_str().starts_with("lyrics:"));
        assert_eq!(LyricsKey::parse(key.as_str()), Some(key.clone()));
        assert_eq!(LyricsKey::parse("lyrics:../../etc/passwd"), None);
        assert_eq!(LyricsKey::parse("album:0123456789abcdef"), None);
    }

    #[test]
    fn mbid_validates_uuid_shape() {
        let ok = "b1a9c0e9-d987-4042-ae91-78d6a3267d69";
        assert_eq!(Mbid::parse(ok).unwrap().as_str(), ok);
        assert_eq!(
            Mbid::parse("B1A9C0E9-D987-4042-AE91-78D6A3267D69")
                .unwrap()
                .as_str(),
            ok
        );
        assert!(Mbid::parse("not-a-uuid").is_none());
        assert!(Mbid::parse("b1a9c0e9d9874042ae9178d6a3267d69").is_none());
    }

    #[test]
    fn canonical_id_reports_its_authority() {
        let mbid = Mbid::parse("b1a9c0e9-d987-4042-ae91-78d6a3267d69").unwrap();
        assert_eq!(CanonicalId::from_mbid(&mbid).kind(), CanonicalKind::Mbid);
        let isrc = Isrc::parse("USRC11700001").unwrap();
        assert_eq!(CanonicalId::from_isrc(&isrc).kind(), CanonicalKind::Isrc);
        assert_eq!(
            CanonicalId::from_local_key("a\u{1}b").kind(),
            CanonicalKind::Local
        );
    }

    #[test]
    fn local_key_is_stable_and_distinct() {
        let a = CanonicalId::from_local_key("radiohead\u{1}creep");
        assert_eq!(a, CanonicalId::from_local_key("radiohead\u{1}creep"));
        assert_ne!(a, CanonicalId::from_local_key("radiohead\u{1}creeq"));
    }
}
