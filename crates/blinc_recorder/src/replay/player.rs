//! Replay player for playing back recorded sessions.
//!
//! The replay player processes recorded events and snapshots
//! with controllable timing and speed.

use super::{EventSimulator, SimulatedInput, VirtualClock};
use crate::{RecordingExport, Timestamp, TimestampedEvent, TreeSnapshot};

/// Configuration for the replay player.
#[derive(Clone, Debug)]
pub struct ReplayConfig {
    /// Initial playback speed (1.0 = normal).
    pub initial_speed: f64,
    /// Whether to loop when reaching the end.
    pub loop_playback: bool,
    /// Frame duration for stepping (microseconds).
    pub frame_duration_us: u64,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            initial_speed: 1.0,
            loop_playback: false,
            frame_duration_us: 16_667, // ~60fps
        }
    }
}

impl ReplayConfig {
    /// Create a config for testing (no loop, normal speed).
    pub fn testing() -> Self {
        Self::default()
    }

    /// Create a config for interactive viewing.
    pub fn interactive() -> Self {
        Self {
            initial_speed: 1.0,
            loop_playback: true,
            frame_duration_us: 16_667,
        }
    }

    /// Set the initial playback speed.
    pub fn with_speed(mut self, speed: f64) -> Self {
        self.initial_speed = speed;
        self
    }

    /// Enable or disable looping.
    pub fn with_loop(mut self, loop_playback: bool) -> Self {
        self.loop_playback = loop_playback;
        self
    }
}

/// Current state of the replay player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayState {
    /// Not started or reset.
    Idle,
    /// Currently playing.
    Playing,
    /// Paused.
    Paused,
    /// Reached the end.
    Finished,
}

/// Replay player for playing back recorded sessions.
pub struct ReplayPlayer {
    /// The recording to replay.
    export: RecordingExport,
    /// Virtual clock for timing.
    clock: VirtualClock,
    /// Event simulator.
    simulator: EventSimulator,
    /// Configuration.
    config: ReplayConfig,
    /// Current state.
    state: ReplayState,
    /// Index of next event to process.
    next_event_index: usize,
    /// Index of next snapshot to process.
    next_snapshot_index: usize,
    /// Last processed position for event deduplication.
    last_processed_position: Timestamp,
}

impl ReplayPlayer {
    /// Create a new replay player with the given recording and config.
    pub fn new(export: RecordingExport, config: ReplayConfig) -> Self {
        let duration = Self::compute_duration(&export);
        let mut clock = VirtualClock::new(duration);
        clock.set_speed(config.initial_speed);

        Self {
            export,
            clock,
            simulator: EventSimulator::new(),
            config,
            state: ReplayState::Idle,
            next_event_index: 0,
            next_snapshot_index: 0,
            last_processed_position: Timestamp::zero(),
        }
    }

    /// Compute the total duration from events and snapshots.
    fn compute_duration(export: &RecordingExport) -> Timestamp {
        let event_duration = export
            .events
            .last()
            .map(|e| e.timestamp)
            .unwrap_or_default();
        let snapshot_duration = export
            .snapshots
            .last()
            .map(|s| s.timestamp)
            .unwrap_or_default();
        if event_duration > snapshot_duration {
            event_duration
        } else {
            snapshot_duration
        }
    }

    /// Get the current state.
    pub fn state(&self) -> ReplayState {
        self.state
    }

    /// Get the virtual clock.
    pub fn clock(&self) -> &VirtualClock {
        &self.clock
    }

    /// Get a mutable reference to the virtual clock.
    pub fn clock_mut(&mut self) -> &mut VirtualClock {
        &mut self.clock
    }

    /// Get the recording export.
    pub fn export(&self) -> &RecordingExport {
        &self.export
    }

    /// Get the current playback position.
    pub fn position(&self) -> Timestamp {
        self.clock.position()
    }

    /// Get the total duration.
    pub fn duration(&self) -> Timestamp {
        self.clock.duration()
    }

    /// Get the progress as a percentage (0.0 to 1.0).
    pub fn progress(&self) -> f32 {
        self.clock.progress()
    }

    /// Check if there are more events to process.
    pub fn has_next(&self) -> bool {
        self.next_event_index < self.export.events.len()
    }

    /// Start or resume playback.
    pub fn play(&mut self) {
        match self.state {
            ReplayState::Idle | ReplayState::Paused => {
                self.clock.play();
                self.state = ReplayState::Playing;
            }
            ReplayState::Finished => {
                if self.config.loop_playback {
                    self.reset();
                    self.clock.play();
                    self.state = ReplayState::Playing;
                }
            }
            ReplayState::Playing => {}
        }
    }

    /// Pause playback.
    pub fn pause(&mut self) {
        if self.state == ReplayState::Playing {
            self.clock.pause();
            self.state = ReplayState::Paused;
        }
    }

    /// Toggle play/pause.
    pub fn toggle(&mut self) {
        match self.state {
            ReplayState::Playing => self.pause(),
            ReplayState::Idle | ReplayState::Paused => self.play(),
            ReplayState::Finished => {
                if self.config.loop_playback {
                    self.play();
                }
            }
        }
    }

    /// Reset to the beginning.
    pub fn reset(&mut self) {
        self.clock.reset();
        self.simulator.reset();
        self.state = ReplayState::Idle;
        self.next_event_index = 0;
        self.next_snapshot_index = 0;
        self.last_processed_position = Timestamp::zero();
    }

    /// Seek to a specific position.
    pub fn seek(&mut self, position: Timestamp) {
        self.clock.seek(position);

        // Reset indices and find the right positions
        self.find_indices_for_position(position);
        self.last_processed_position = position;
    }

    /// Find the event/snapshot indices for a given position.
    fn find_indices_for_position(&mut self, position: Timestamp) {
        // Find first event at or after position
        self.next_event_index = self
            .export
            .events
            .iter()
            .position(|e| e.timestamp >= position)
            .unwrap_or(self.export.events.len());

        // Find first snapshot at or after position
        self.next_snapshot_index = self
            .export
            .snapshots
            .iter()
            .position(|s| s.timestamp >= position)
            .unwrap_or(self.export.snapshots.len());
    }

    /// Update the player (call every frame).
    ///
    /// Returns a frame update containing events that occurred since the last update.
    pub fn update(&mut self) -> FrameUpdate {
        let mut update = FrameUpdate::default();

        if self.state != ReplayState::Playing {
            return update;
        }

        // Update the clock
        let reached_end = self.clock.update();

        // Collect events up to current position
        let current_pos = self.clock.position();
        update.events = self.collect_events_until(current_pos);

        // Get the current snapshot
        update.snapshot = self.get_snapshot_at(current_pos);

        // Handle end of playback
        if reached_end {
            if self.config.loop_playback {
                self.reset();
                self.play();
            } else {
                self.state = ReplayState::Finished;
            }
        }

        self.last_processed_position = current_pos;
        update
    }

    /// Step forward by one frame.
    ///
    /// Returns events that occurred during this frame.
    pub fn step(&mut self) -> FrameUpdate {
        let current = self.clock.position();
        let next = Timestamp::from_micros(
            current
                .as_micros()
                .saturating_add(self.config.frame_duration_us),
        );

        // Clamp to duration
        let next = if next > self.clock.duration() {
            self.clock.duration()
        } else {
            next
        };

        // Collect events in this range
        let update = FrameUpdate {
            events: self.collect_events_in_range(current, next),
            snapshot: self.get_snapshot_at(next),
        };

        // Move clock forward
        self.clock.seek(next);
        self.last_processed_position = next;

        if next >= self.clock.duration() {
            self.state = ReplayState::Finished;
        } else {
            self.state = ReplayState::Paused;
        }

        update
    }

    /// Step backward by one frame.
    pub fn step_back(&mut self) -> FrameUpdate {
        let current = self.clock.position();
        let prev = if current.as_micros() > self.config.frame_duration_us {
            Timestamp::from_micros(current.as_micros() - self.config.frame_duration_us)
        } else {
            Timestamp::zero()
        };

        self.seek(prev);
        self.state = ReplayState::Paused;

        FrameUpdate {
            events: Vec::new(),
            snapshot: self.get_snapshot_at(prev),
        }
    }

    /// Collect events from current index up to the given position.
    fn collect_events_until(&mut self, position: Timestamp) -> Vec<SimulatedInput> {
        let mut events = Vec::new();

        while self.next_event_index < self.export.events.len() {
            let event = &self.export.events[self.next_event_index];
            if event.timestamp <= position {
                events.push(self.simulator.process(event));
                self.next_event_index += 1;
            } else {
                break;
            }
        }

        events
    }

    /// Collect events in a specific time range.
    fn collect_events_in_range(&mut self, start: Timestamp, end: Timestamp) -> Vec<SimulatedInput> {
        let mut events = Vec::new();

        // Make sure we start from the right index
        while self.next_event_index < self.export.events.len() {
            let event = &self.export.events[self.next_event_index];
            if event.timestamp < start {
                self.next_event_index += 1;
            } else {
                break;
            }
        }

        // Collect events in range
        while self.next_event_index < self.export.events.len() {
            let event = &self.export.events[self.next_event_index];
            if event.timestamp <= end {
                events.push(self.simulator.process(event));
                self.next_event_index += 1;
            } else {
                break;
            }
        }

        events
    }

    /// Get the snapshot at or before the given position.
    fn get_snapshot_at(&self, position: Timestamp) -> Option<TreeSnapshot> {
        self.export
            .snapshots
            .iter()
            .rfind(|s| s.timestamp <= position)
            .cloned()
    }

    /// Get events in a time range without advancing state.
    pub fn peek_events(&self, start: Timestamp, end: Timestamp) -> Vec<&TimestampedEvent> {
        self.export
            .events
            .iter()
            .filter(|e| e.timestamp >= start && e.timestamp <= end)
            .collect()
    }

    /// Get all events.
    pub fn all_events(&self) -> &[TimestampedEvent] {
        &self.export.events
    }

    /// Get all snapshots.
    pub fn all_snapshots(&self) -> &[TreeSnapshot] {
        &self.export.snapshots
    }
}

/// Update returned by the replay player each frame.
#[derive(Clone, Debug, Default)]
pub struct FrameUpdate {
    /// Events that occurred in this frame.
    pub events: Vec<SimulatedInput>,
    /// Current tree snapshot (if available).
    pub snapshot: Option<TreeSnapshot>,
}

impl FrameUpdate {
    /// Check if this update has any events.
    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Check if this update has a snapshot.
    pub fn has_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Modifiers, MouseButton, MouseEvent, Point, RecordedEvent, RecordingConfig};

    fn create_test_export() -> RecordingExport {
        let events = vec![
            TimestampedEvent::new(
                Timestamp::from_micros(0),
                RecordedEvent::Click(MouseEvent {
                    position: Point::new(100.0, 100.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                    target_element: None,
                }),
            ),
            TimestampedEvent::new(
                Timestamp::from_micros(100_000), // 100ms
                RecordedEvent::Click(MouseEvent {
                    position: Point::new(200.0, 200.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                    target_element: None,
                }),
            ),
            TimestampedEvent::new(
                Timestamp::from_micros(200_000), // 200ms
                RecordedEvent::Click(MouseEvent {
                    position: Point::new(300.0, 300.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                    target_element: None,
                }),
            ),
        ];

        RecordingExport {
            config: RecordingConfig::minimal(),
            events,
            snapshots: Vec::new(),
            stats: Default::default(),
        }
    }

    #[test]
    fn test_player_creation() {
        let export = create_test_export();
        let player = ReplayPlayer::new(export, ReplayConfig::default());

        assert_eq!(player.state(), ReplayState::Idle);
        assert_eq!(player.position().as_micros(), 0);
        assert_eq!(player.duration().as_micros(), 200_000);
    }

    #[test]
    fn test_player_step() {
        let export = create_test_export();
        let config = ReplayConfig::default().with_speed(1.0);
        let mut player = ReplayPlayer::new(export, config);

        // First step should get the first event
        let update = player.step();
        assert!(update.has_events());
    }

    #[test]
    fn test_player_seek() {
        let export = create_test_export();
        let mut player = ReplayPlayer::new(export, ReplayConfig::default());

        // Seek to middle
        player.seek(Timestamp::from_micros(150_000));
        assert_eq!(player.position().as_micros(), 150_000);

        // next_event_index should be pointing at the 200ms event
        assert_eq!(player.next_event_index, 2);
    }

    #[test]
    fn test_player_reset() {
        let export = create_test_export();
        let mut player = ReplayPlayer::new(export, ReplayConfig::default());

        player.seek(Timestamp::from_micros(150_000));
        player.reset();

        assert_eq!(player.state(), ReplayState::Idle);
        assert_eq!(player.position().as_micros(), 0);
        assert_eq!(player.next_event_index, 0);
    }

    #[test]
    fn test_has_next() {
        let export = create_test_export();
        let mut player = ReplayPlayer::new(export, ReplayConfig::default());

        assert!(player.has_next());
        assert_eq!(player.all_events().len(), 3);

        // Seek beyond the last event timestamp
        player.seek(Timestamp::from_micros(200_001));

        // Should have no more events to process
        assert!(!player.has_next());
    }

    #[test]
    fn test_step_with_custom_frame_duration() {
        let export = create_test_export();
        // Use a large frame duration (100ms) so each step covers more time
        let config = ReplayConfig {
            frame_duration_us: 100_000, // 100ms
            ..ReplayConfig::default()
        };
        let mut player = ReplayPlayer::new(export, config);

        // First step: 0ms -> 100ms, should get events at 0ms and 100ms
        let update = player.step();
        assert!(!update.events.is_empty());
    }
}
