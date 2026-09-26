//! The play queue (PLAN §1.5).
//!
//! The state is kept in the core; the CLI and the GUI only show it. The queue
//! is a pure data structure — it knows nothing of the audio pipeline, compiles
//! independently of the `audio` feature, and its tests need no audio device.

use serde::{Deserialize, Serialize};

use crate::ids::ProviderTrackId;
use crate::model::TrackRef;

/// An item in the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    /// Which track, from which provider.
    pub id: ProviderTrackId,
    pub track: TrackRef,
}

/// The repeat mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    /// Stops when the queue ends.
    #[default]
    Off,
    /// Goes back to the start when the queue ends.
    All,
    /// Repeats the same track.
    One,
}

impl RepeatMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }
}

impl std::fmt::Display for RepeatMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A readable view of the queue — **in play order**.
///
/// What the shells (TUI, GUI) take in one go and draw. Not a separate "IPC
/// type": it lives in the core, crosses as it is with `serde`, and the TUI
/// uses the same thing (D-033).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueView {
    /// The items, in play order.
    pub items: Vec<QueueItem>,
    /// The playing position within `items`. Meaningless if the queue is empty.
    pub position: usize,
    pub repeat: RepeatMode,
    pub shuffle: bool,
}

/// The play queue and the position within it.
///
/// Shuffling **does not disturb the order**; it keeps a separate play order:
/// turning shuffle off does not lose the user's list, and "what's next" is
/// answered from the same place in both modes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queue {
    items: Vec<QueueItem>,
    /// `order[position]` → an index into `items`.
    order: Vec<usize>,
    /// The position within `order`. Meaningless if the queue is empty.
    position: usize,
    repeat: RepeatMode,
    shuffle: bool,
    /// The deterministic generator state for shuffling (testability).
    rng_state: u64,
}

impl Queue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rng_state: 0x2545_F491_4F6C_DD1D,
            ..Self::default()
        }
    }

    /// Replaces the queue with the given tracks and moves to the start.
    pub fn replace(&mut self, items: Vec<QueueItem>) {
        self.items = items;
        self.order = (0..self.items.len()).collect();
        self.position = 0;
        if self.shuffle {
            self.reshuffle();
        }
    }

    /// Appends to the end of the queue.
    pub fn append(&mut self, items: impl IntoIterator<Item = QueueItem>) {
        for item in items {
            self.items.push(item);
            self.order.push(self.items.len() - 1);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.position = 0;
    }

    /// The items in play order.
    #[must_use]
    pub fn items(&self) -> Vec<QueueItem> {
        self.order
            .iter()
            .filter_map(|&index| self.items.get(index).cloned())
            .collect()
    }

    /// The form of the queue shown to the outside.
    ///
    /// The `Queue` itself is not sent to the shell: it contains `order` and
    /// `rng_state`, so the consumer would have to build the play order
    /// **itself** — a second copy of the ordering logic would live in JS. This
    /// view gives the order already applied.
    #[must_use]
    pub fn view(&self) -> QueueView {
        QueueView {
            items: self.items(),
            position: self.position,
            repeat: self.repeat,
            shuffle: self.shuffle,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The item that should be playing right now.
    #[must_use]
    pub fn current(&self) -> Option<&QueueItem> {
        let index = *self.order.get(self.position)?;
        self.items.get(index)
    }

    /// The position in the play order (0-based).
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    #[must_use]
    pub const fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    pub fn set_repeat(&mut self, repeat: RepeatMode) {
        self.repeat = repeat;
    }

    #[must_use]
    pub const fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// Turns shuffle on/off.
    ///
    /// When turning it on, **the playing track stays where it is** — it is not
    /// pulled out from under the user; the rest are shuffled. When turning it
    /// off, the original order comes back and the position is corrected to the
    /// playing track.
    pub fn set_shuffle(&mut self, shuffle: bool) {
        if shuffle == self.shuffle {
            return;
        }
        let current_index = self.order.get(self.position).copied();
        self.shuffle = shuffle;
        if shuffle {
            self.reshuffle();
        } else {
            self.order = (0..self.items.len()).collect();
        }
        // Move the position to the playing track's new place in the order.
        if let Some(index) = current_index {
            if let Some(new_position) = self.order.iter().position(|&i| i == index) {
                self.position = new_position;
            }
        }
    }

    /// Jumps to a given position.
    ///
    /// If it is out of range it returns `false` and the queue does not change —
    /// it does not silently wrap to the start, because the caller should know it
    /// made a mistake.
    pub fn jump_to(&mut self, position: usize) -> bool {
        if position >= self.order.len() {
            return false;
        }
        self.position = position;
        true
    }

    /// Moves on to the next track.
    ///
    /// `RepeatMode::One` **does not repeat on its own here** — that is split off
    /// with [`Queue::advance_after_finish`] when the track ends naturally. If the
    /// user says "next", the queue moves on to the next track in repeat mode too;
    /// otherwise the key would look broken.
    #[expect(
        clippy::should_implement_trait,
        reason = "the queue is not an iterator: it has a position and also goes backwards (previous). \
                  `next` here is the name of the key the user pressed."
    )]
    pub fn next(&mut self) -> Option<&QueueItem> {
        if self.order.is_empty() {
            return None;
        }
        if self.position + 1 < self.order.len() {
            self.position += 1;
        } else if self.repeat == RepeatMode::All {
            self.position = 0;
            if self.shuffle {
                self.reshuffle();
            }
        } else {
            return None;
        }
        self.current()
    }

    /// Goes back to the previous track.
    pub fn previous(&mut self) -> Option<&QueueItem> {
        if self.order.is_empty() {
            return None;
        }
        if self.position > 0 {
            self.position -= 1;
        } else if self.repeat == RepeatMode::All {
            self.position = self.order.len() - 1;
        } else {
            return None;
        }
        self.current()
    }

    /// Says which track comes next on a natural end **without moving the
    /// cursor**.
    ///
    /// For gapless read-ahead (D-024): the next track must start decoding while
    /// today's track is still playing. The cursor only advances when the
    /// transition is **heard**; otherwise the interface would show a track as
    /// playing that is not.
    ///
    /// At the end of the queue, when wrapping to the start with `RepeatMode::All`,
    /// it returns **`None`**: wrapping regenerates the shuffle
    /// ([`Queue::reshuffle`]), and which track comes next cannot be known without
    /// moving the cursor. The price is one gap per round; better than decoding a
    /// made-up track ahead.
    #[must_use]
    pub fn peek_after_finish(&self) -> Option<&QueueItem> {
        if self.repeat == RepeatMode::One {
            return self.current();
        }
        let next = self.position.checked_add(1)?;
        if next >= self.order.len() {
            return None;
        }
        self.order
            .get(next)
            .and_then(|index| self.items.get(*index))
    }

    /// The tracks after the playing one, in play order, **without moving the
    /// cursor**: at most `count`, the next one first (D-077).
    ///
    /// What "next" plays — the user's key — in every repeat mode, so under
    /// `RepeatMode::One` too it is the track after the playing one. It does
    /// not wrap to the start: wrapping reshuffles, and which track comes then
    /// is not known yet ([`Queue::peek_after_finish`] stops there for the
    /// same reason).
    #[must_use]
    pub fn upcoming(&self, count: usize) -> Vec<&QueueItem> {
        let first = self.position.saturating_add(1);
        self.order
            .iter()
            .skip(first)
            .take(count)
            .filter_map(|&index| self.items.get(index))
            .collect()
    }

    /// Picks the next track when the track **ends naturally**.
    ///
    /// The difference from [`Queue::next`]: here `RepeatMode::One` gives the same
    /// track again. This is the one place where a user request and an automatic
    /// transition must be told apart.
    pub fn advance_after_finish(&mut self) -> Option<&QueueItem> {
        if self.repeat == RepeatMode::One {
            return self.current();
        }
        self.next()
    }

    /// Shuffles the rest; the playing track (if any) stays first.
    fn reshuffle(&mut self) {
        let current = self.order.get(self.position).copied();
        let mut rest: Vec<usize> = (0..self.items.len())
            .filter(|index| Some(*index) != current)
            .collect();

        // Fisher-Yates, with a deterministic generator (xorshift64*).
        for i in (1..rest.len()).rev() {
            let j = (self.next_random() % (i as u64 + 1)) as usize;
            rest.swap(i, j);
        }

        self.order = match current {
            Some(index) => std::iter::once(index).chain(rest).collect(),
            None => rest,
        };
        self.position = 0;
    }

    /// xorshift64* — not cryptographic, only a repeatable shuffle.
    fn next_random(&mut self) -> u64 {
        let mut x = self.rng_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng_state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Fixes the shuffle generator. For tests only.
    #[doc(hidden)]
    pub fn seed_rng(&mut self, seed: u64) {
        self.rng_state = seed | 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProviderId;

    fn item(title: &str) -> QueueItem {
        QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), title),
            track: TrackRef::new("Artist", title),
        }
    }

    fn queue_of(titles: &[&str]) -> Queue {
        let mut queue = Queue::new();
        queue.replace(titles.iter().map(|t| item(t)).collect());
        queue
    }

    fn current_title(queue: &Queue) -> Option<String> {
        queue.current().map(|item| item.track.title.clone())
    }

    fn titles(items: &[&QueueItem]) -> Vec<String> {
        items.iter().map(|item| item.track.title.clone()).collect()
    }

    /// D-077: what comes after the playing track, in play order, without
    /// moving the cursor — shuffled, it follows the shuffled order.
    #[test]
    fn upcoming_follows_play_order_without_moving() {
        let mut queue = queue_of(&["a", "b", "c", "d"]);
        assert_eq!(titles(&queue.upcoming(2)), ["b", "c"]);
        assert_eq!(
            current_title(&queue).as_deref(),
            Some("a"),
            "the cursor did not move"
        );

        queue.jump_to(2);
        assert_eq!(
            titles(&queue.upcoming(5)),
            ["d"],
            "it does not wrap to the start"
        );
        queue.set_repeat(RepeatMode::All);
        assert_eq!(titles(&queue.upcoming(5)), ["d"], "not even when repeating");
        queue.set_repeat(RepeatMode::One);
        assert_eq!(
            titles(&queue.upcoming(1)),
            ["d"],
            "\"next\" leaves a repeated track"
        );

        queue.jump_to(0);
        queue.set_shuffle(true);
        let order: Vec<String> = queue
            .items()
            .iter()
            .map(|item| item.track.title.clone())
            .collect();
        assert_eq!(titles(&queue.upcoming(3)), order[1..4]);
    }

    #[test]
    fn walks_forward_and_stops_at_the_end() {
        let mut queue = queue_of(&["a", "b"]);
        assert_eq!(current_title(&queue).as_deref(), Some("a"));
        assert_eq!(
            queue.next().map(|i| i.track.title.clone()).as_deref(),
            Some("b")
        );
        assert!(queue.next().is_none(), "with repeat off the queue must end");
    }

    #[test]
    fn repeat_all_wraps_around() {
        let mut queue = queue_of(&["a", "b"]);
        queue.set_repeat(RepeatMode::All);
        queue.next();
        assert_eq!(
            queue.next().map(|i| i.track.title.clone()).as_deref(),
            Some("a")
        );
        assert_eq!(queue.position(), 0);
    }

    #[test]
    fn repeat_one_only_applies_to_natural_finish() {
        let mut queue = queue_of(&["a", "b"]);
        queue.set_repeat(RepeatMode::One);

        // The track ended on its own: the same track again.
        assert_eq!(
            queue
                .advance_after_finish()
                .map(|i| i.track.title.clone())
                .as_deref(),
            Some("a")
        );
        // The user said "next": it must move on even in repeat mode, otherwise the
        // key looks broken.
        assert_eq!(
            queue.next().map(|i| i.track.title.clone()).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn previous_stops_at_the_start_unless_repeating() {
        let mut queue = queue_of(&["a", "b"]);
        assert!(queue.previous().is_none());
        queue.set_repeat(RepeatMode::All);
        assert_eq!(
            queue.previous().map(|i| i.track.title.clone()).as_deref(),
            Some("b"),
            "repeat all must wrap to the end when going backwards at the start"
        );
    }

    #[test]
    fn shuffle_keeps_the_current_track_playing() {
        let mut queue = queue_of(&["a", "b", "c", "d", "e"]);
        queue.seed_rng(42);
        queue.next(); // "b" is playing
        assert_eq!(current_title(&queue).as_deref(), Some("b"));

        queue.set_shuffle(true);
        assert_eq!(
            current_title(&queue).as_deref(),
            Some("b"),
            "shuffling must not pull the playing track out from under the user"
        );
        assert_eq!(queue.len(), 5, "no track must be lost");
    }

    #[test]
    fn unshuffle_restores_the_original_order() {
        let mut queue = queue_of(&["a", "b", "c", "d"]);
        queue.seed_rng(7);
        queue.set_shuffle(true);
        queue.set_shuffle(false);

        let titles: Vec<String> = queue
            .items()
            .iter()
            .map(|i| i.track.title.clone())
            .collect();
        assert_eq!(titles, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn shuffle_actually_reorders_a_long_queue() {
        let titles: Vec<String> = (0..50).map(|i| format!("p{i}")).collect();
        let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
        let mut queue = queue_of(&refs);
        queue.seed_rng(1234);
        queue.set_shuffle(true);

        let shuffled: Vec<String> = queue
            .items()
            .iter()
            .map(|i| i.track.title.clone())
            .collect();
        assert_ne!(
            shuffled, titles,
            "50 tracks must not stay in the same order"
        );
        // It must be the same set; only the order may change.
        let mut sorted = shuffled.clone();
        sorted.sort();
        let mut expected = titles.clone();
        expected.sort();
        assert_eq!(sorted, expected);
    }

    #[test]
    fn jump_to_rejects_out_of_range_instead_of_wrapping() {
        let mut queue = queue_of(&["a", "b"]);
        assert!(!queue.jump_to(5), "an out-of-range jump must be refused");
        assert_eq!(
            queue.position(),
            0,
            "a failed jump must not disturb the position"
        );
        assert!(queue.jump_to(1));
        assert_eq!(current_title(&queue).as_deref(), Some("b"));
    }

    #[test]
    fn an_empty_queue_is_inert() {
        let mut queue = Queue::new();
        assert!(queue.is_empty());
        assert!(queue.current().is_none());
        assert!(queue.next().is_none());
        assert!(queue.previous().is_none());
        assert!(queue.advance_after_finish().is_none());
    }
}
