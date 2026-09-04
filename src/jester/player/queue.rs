use std::collections::VecDeque;

use poise::serenity_prelude::UserId;

use crate::jester::track::types::TrackInfo;

/// Identifies one explicit queue insertion within a guild.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueueEntryId(pub u64);

/// A track explicitly submitted to the guild queue.
#[derive(Clone, Debug)]
pub struct QueueEntry {
    pub id: QueueEntryId,
    pub track: TrackInfo,
    pub requested_by: Option<UserId>,
}

/// Distinguishes an immediate `/play` from an entry submitted to the queue.
#[derive(Clone, Debug)]
pub enum PlaybackSource {
    Direct,
    Queue(QueueEntry),
}

/// The track currently selected for output by the queue state machine.
#[derive(Clone, Debug)]
pub struct PlaybackItem {
    pub track: TrackInfo,
    pub source: PlaybackSource,
}

impl PlaybackItem {
    fn direct(track: TrackInfo) -> Self {
        Self {
            track,
            source: PlaybackSource::Direct,
        }
    }

    fn queued(entry: QueueEntry) -> Self {
        Self {
            track: entry.track.clone(),
            source: PlaybackSource::Queue(entry),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepeatMode {
    #[default]
    Off,
    Track,
    Queue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryOutcome {
    Completed,
    Skipped,
    Replaced,
}

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub item: PlaybackItem,
    pub outcome: HistoryOutcome,
}

/// Describes a state transition that may require the playback adapter to act.
#[derive(Clone, Debug, Default)]
pub struct QueueTransition {
    pub previous: Option<PlaybackItem>,
    pub current: Option<PlaybackItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueError {
    NoQueuedTrackToSkip,
    PositionOutOfRange,
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoQueuedTrackToSkip => {
                formatter.write_str("No queued track is available to skip to.")
            }
            Self::PositionOutOfRange => formatter.write_str("Queue position is out of range."),
        }
    }
}

impl std::error::Error for QueueError {}

/// Pure per-guild queue state.
///
/// The playback adapter owns audio handles. It must use the returned transition
/// to stop the old input and start `current`, then report natural completion
/// through [`GuildQueue::complete_current`].
pub struct GuildQueue {
    current: Option<PlaybackItem>,
    upcoming: VecDeque<QueueEntry>,
    repeat_cycle: VecDeque<QueueEntry>,
    history: VecDeque<HistoryEntry>,
    repeat_mode: RepeatMode,
    next_entry_id: u64,
    history_capacity: usize,
}

impl GuildQueue {
    pub fn new(history_capacity: usize) -> Self {
        Self {
            current: None,
            upcoming: VecDeque::new(),
            repeat_cycle: VecDeque::new(),
            history: VecDeque::new(),
            repeat_mode: RepeatMode::Off,
            next_entry_id: 1,
            history_capacity,
        }
    }

    pub fn current(&self) -> Option<&PlaybackItem> {
        self.current.as_ref()
    }

    pub fn upcoming(&self) -> &VecDeque<QueueEntry> {
        &self.upcoming
    }

    pub fn history(&self) -> &VecDeque<HistoryEntry> {
        &self.history
    }

    pub fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }

    pub fn set_repeat_mode(&mut self, repeat_mode: RepeatMode) {
        self.repeat_mode = repeat_mode;
    }

    /// Immediately replaces the active track without touching explicit queue entries.
    pub fn play_now(&mut self, track: TrackInfo) -> QueueTransition {
        let previous = self.current.replace(PlaybackItem::direct(track));
        if let Some(item) = previous.as_ref() {
            self.record(item.clone(), HistoryOutcome::Replaced);
        }

        QueueTransition {
            previous,
            current: self.current.clone(),
        }
    }

    /// Appends a track to the explicit queue, starting it when the player is idle.
    pub fn enqueue(&mut self, track: TrackInfo, requested_by: Option<UserId>) -> QueueTransition {
        let entry = self.new_entry(track, requested_by);
        self.insert(entry, false)
    }

    /// Inserts a track after the current item, starting it when the player is idle.
    pub fn enqueue_next(
        &mut self,
        track: TrackInfo,
        requested_by: Option<UserId>,
    ) -> QueueTransition {
        let entry = self.new_entry(track, requested_by);
        self.insert(entry, true)
    }

    /// Advances after Songbird reports that the active input ended naturally.
    pub fn complete_current(&mut self) -> QueueTransition {
        let previous = self.current.take();
        let Some(item) = previous.as_ref() else {
            return QueueTransition::default();
        };

        if self.repeat_mode == RepeatMode::Track {
            self.current.clone_from(&previous);
            return QueueTransition {
                previous,
                current: self.current.clone(),
            };
        }

        self.record(item.clone(), HistoryOutcome::Completed);
        if self.repeat_mode == RepeatMode::Queue
            && let PlaybackSource::Queue(entry) = &item.source
        {
            self.repeat_cycle.push_back(entry.clone());
        }

        self.start_next();
        QueueTransition {
            previous,
            current: self.current.clone(),
        }
    }

    /// Skips to an already queued track. It intentionally ignores repeat-track.
    pub fn skip(&mut self) -> Result<QueueTransition, QueueError> {
        if self.upcoming.is_empty() {
            return Err(QueueError::NoQueuedTrackToSkip);
        }

        let previous = self.current.take();
        if let Some(item) = previous.as_ref() {
            self.record(item.clone(), HistoryOutcome::Skipped);
        }
        self.start_next();

        Ok(QueueTransition {
            previous,
            current: self.current.clone(),
        })
    }

    /// Removes a one-based position from upcoming tracks only.
    pub fn remove(&mut self, position: usize) -> Result<QueueEntry, QueueError> {
        self.position_to_index(position)
            .and_then(|index| self.upcoming.remove(index))
            .ok_or(QueueError::PositionOutOfRange)
    }

    /// Moves an upcoming item between one-based positions.
    pub fn move_entry(&mut self, from: usize, to: usize) -> Result<(), QueueError> {
        let from_index = self
            .position_to_index(from)
            .ok_or(QueueError::PositionOutOfRange)?;
        let to_index = self
            .position_to_index(to)
            .ok_or(QueueError::PositionOutOfRange)?;
        let entry = self
            .upcoming
            .remove(from_index)
            .ok_or(QueueError::PositionOutOfRange)?;
        self.upcoming.insert(to_index, entry);
        Ok(())
    }

    /// Clears upcoming tracks only. The active track and repeat cycle are retained.
    pub fn clear_upcoming(&mut self) {
        self.upcoming.clear();
    }

    /// Shuffles upcoming tracks only, using the supplied deterministic shuffle function.
    pub fn shuffle_with(&mut self, shuffle: impl FnOnce(&mut [QueueEntry])) {
        let contiguous = self.upcoming.make_contiguous();
        shuffle(contiguous);
    }

    fn new_entry(&mut self, track: TrackInfo, requested_by: Option<UserId>) -> QueueEntry {
        let entry = QueueEntry {
            id: QueueEntryId(self.next_entry_id),
            track,
            requested_by,
        };
        self.next_entry_id = self.next_entry_id.wrapping_add(1).max(1);
        entry
    }

    fn insert(&mut self, entry: QueueEntry, next: bool) -> QueueTransition {
        if self.current.is_none() {
            self.current = Some(PlaybackItem::queued(entry));
            return QueueTransition {
                previous: None,
                current: self.current.clone(),
            };
        }

        if next {
            self.upcoming.push_front(entry);
        } else {
            self.upcoming.push_back(entry);
        }
        QueueTransition::default()
    }

    fn start_next(&mut self) {
        if self.upcoming.is_empty() && self.repeat_mode == RepeatMode::Queue {
            self.upcoming.append(&mut self.repeat_cycle);
        }
        self.current = self.upcoming.pop_front().map(PlaybackItem::queued);
    }

    fn record(&mut self, item: PlaybackItem, outcome: HistoryOutcome) {
        if self.history_capacity == 0 {
            return;
        }
        self.history.push_back(HistoryEntry { item, outcome });
        while self.history.len() > self.history_capacity {
            self.history.pop_front();
        }
    }

    fn position_to_index(&self, position: usize) -> Option<usize> {
        position
            .checked_sub(1)
            .filter(|index| *index < self.upcoming.len())
    }
}

impl Default for GuildQueue {
    fn default() -> Self {
        Self::new(50)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::redundant_closure_for_method_calls)]
mod tests {
    use super::{GuildQueue, HistoryOutcome, PlaybackSource, QueueError, RepeatMode};
    use crate::jester::track::types::{TrackInfo, VideoId};

    fn track(title: &str) -> TrackInfo {
        TrackInfo {
            id: VideoId::from(title),
            title: title.into(),
            artist: "artist".into(),
            origin: "origin".into(),
        }
    }

    fn current_title(queue: &GuildQueue) -> Option<&str> {
        queue.current().map(|item| item.track.title.as_str())
    }

    fn upcoming_titles(queue: &GuildQueue) -> Vec<&str> {
        queue
            .upcoming()
            .iter()
            .map(|entry| entry.track.title.as_str())
            .collect()
    }

    #[test]
    fn first_queued_track_starts_and_later_entries_remain_upcoming() {
        let mut queue = GuildQueue::default();

        let first = queue.enqueue(track("first"), None);
        let second = queue.enqueue(track("second"), None);

        assert!(first.previous.is_none());
        assert_eq!(first.current.as_ref().unwrap().track.title, "first");
        assert!(second.current.is_none());
        assert_eq!(current_title(&queue), Some("first"));
        assert_eq!(upcoming_titles(&queue), vec!["second"]);
    }

    #[test]
    fn enqueue_next_inserts_before_existing_upcoming_tracks() {
        let mut queue = GuildQueue::default();
        queue.enqueue(track("current"), None);
        queue.enqueue(track("later"), None);
        queue.enqueue_next(track("next"), None);

        assert_eq!(upcoming_titles(&queue), vec!["next", "later"]);
    }

    #[test]
    fn play_now_replaces_current_records_it_and_preserves_the_queue() {
        let mut queue = GuildQueue::default();
        queue.enqueue(track("queued current"), None);
        queue.enqueue(track("queued next"), None);

        let transition = queue.play_now(track("direct"));

        assert_eq!(transition.previous.unwrap().track.title, "queued current");
        assert_eq!(current_title(&queue), Some("direct"));
        assert_eq!(upcoming_titles(&queue), vec!["queued next"]);
        assert_eq!(queue.history().len(), 1);
        assert_eq!(queue.history()[0].item.track.title, "queued current");
        assert_eq!(queue.history()[0].outcome, HistoryOutcome::Replaced);
    }

    #[test]
    fn completing_tracks_advances_then_becomes_idle_when_repeat_is_off() {
        let mut queue = GuildQueue::default();
        queue.enqueue(track("first"), None);
        queue.enqueue(track("second"), None);

        let first_completion = queue.complete_current();
        let second_completion = queue.complete_current();

        assert_eq!(first_completion.previous.unwrap().track.title, "first");
        assert_eq!(first_completion.current.unwrap().track.title, "second");
        assert_eq!(second_completion.previous.unwrap().track.title, "second");
        assert!(second_completion.current.is_none());
        assert!(queue.current().is_none());
        assert_eq!(
            queue
                .history()
                .iter()
                .map(|entry| entry.outcome)
                .collect::<Vec<_>>(),
            vec![HistoryOutcome::Completed, HistoryOutcome::Completed]
        );
    }

    #[test]
    fn repeat_track_restarts_without_writing_history_or_advancing() {
        let mut queue = GuildQueue::default();
        queue.enqueue(track("first"), None);
        queue.enqueue(track("second"), None);
        queue.set_repeat_mode(RepeatMode::Track);

        let transition = queue.complete_current();

        assert_eq!(transition.previous.unwrap().track.title, "first");
        assert_eq!(transition.current.unwrap().track.title, "first");
        assert_eq!(upcoming_titles(&queue), vec!["second"]);
        assert!(queue.history().is_empty());
    }

    #[test]
    fn repeat_queue_cycles_only_completed_explicit_queue_entries() {
        let mut queue = GuildQueue::default();
        queue.enqueue(track("first"), None);
        queue.enqueue(track("second"), None);
        queue.set_repeat_mode(RepeatMode::Queue);

        queue.play_now(track("direct interruption"));
        queue.complete_current();
        assert_eq!(current_title(&queue), Some("second"));

        queue.complete_current();
        assert_eq!(current_title(&queue), Some("second"));
        assert!(matches!(
            &queue.current().unwrap().source,
            PlaybackSource::Queue(_)
        ));
        assert_eq!(upcoming_titles(&queue), Vec::<&str>::new());
    }

    #[test]
    fn skip_requires_an_upcoming_track_and_does_not_mutate_when_absent() {
        let mut queue = GuildQueue::default();
        queue.enqueue(track("only"), None);

        assert!(matches!(queue.skip(), Err(QueueError::NoQueuedTrackToSkip)));
        assert_eq!(current_title(&queue), Some("only"));
        assert!(queue.history().is_empty());
    }

    #[test]
    fn skip_bypasses_repeat_track_and_records_the_skipped_item() {
        let mut queue = GuildQueue::default();
        queue.enqueue(track("first"), None);
        queue.enqueue(track("second"), None);
        queue.set_repeat_mode(RepeatMode::Track);

        let transition = queue.skip().unwrap();

        assert_eq!(transition.previous.unwrap().track.title, "first");
        assert_eq!(transition.current.unwrap().track.title, "second");
        assert_eq!(current_title(&queue), Some("second"));
        assert_eq!(queue.history()[0].outcome, HistoryOutcome::Skipped);
    }

    #[test]
    fn remove_and_move_use_one_based_upcoming_positions() {
        let mut queue = GuildQueue::default();
        queue.enqueue(track("current"), None);
        queue.enqueue(track("one"), None);
        queue.enqueue(track("two"), None);
        queue.enqueue(track("three"), None);

        queue.move_entry(3, 1).unwrap();
        let removed = queue.remove(2).unwrap();

        assert_eq!(removed.track.title, "one");
        assert_eq!(upcoming_titles(&queue), vec!["three", "two"]);
        assert!(matches!(
            queue.remove(0),
            Err(QueueError::PositionOutOfRange)
        ));
        assert!(matches!(
            queue.move_entry(1, 3),
            Err(QueueError::PositionOutOfRange)
        ));
    }

    #[test]
    fn clear_and_shuffle_apply_to_upcoming_tracks_only() {
        let mut queue = GuildQueue::default();
        queue.enqueue(track("current"), None);
        queue.enqueue(track("one"), None);
        queue.enqueue(track("two"), None);

        queue.shuffle_with(|entries| entries.reverse());
        assert_eq!(current_title(&queue), Some("current"));
        assert_eq!(upcoming_titles(&queue), vec!["two", "one"]);

        queue.clear_upcoming();
        assert_eq!(current_title(&queue), Some("current"));
        assert!(queue.upcoming().is_empty());
    }

    #[test]
    fn history_is_bounded_and_retains_the_most_recent_entries() {
        let mut queue = GuildQueue::new(2);
        queue.play_now(track("one"));
        queue.play_now(track("two"));
        queue.play_now(track("three"));

        assert_eq!(queue.history().len(), 2);
        assert_eq!(queue.history()[0].item.track.title, "one");
        assert_eq!(queue.history()[1].item.track.title, "two");
        assert!(
            queue
                .history()
                .iter()
                .all(|entry| entry.outcome == HistoryOutcome::Replaced)
        );
    }
}
