//! Timeline orchestration for multiple animations
//!
//! Inspired by anime.js timelines, this module provides orchestration of multiple
//! animations with precise timing control, staggered offsets, and looping modes.

use crate::easing::Easing;
use slotmap::{SlotMap, new_key_type};

new_key_type! {
    pub struct TimelineEntryId;
}

/// An entry in a timeline
struct TimelineEntry {
    /// Offset in milliseconds from timeline start
    offset_ms: i32,
    /// Duration of the animation
    duration_ms: u32,
    /// Animation start value
    start_value: f32,
    /// Animation end value
    end_value: f32,
    /// Easing function for this entry
    easing: Easing,
}

/// A timeline that orchestrates multiple animations
///
/// Timelines synchronize multiple animations together, each with their own
/// offset, duration, and easing. Supports looping and alternate (ping-pong) mode.
///
/// # Example
///
/// ```ignore
/// let mut timeline = Timeline::new();
///
/// // Add three staggered animations
/// let bar1 = timeline.add(0, 500, 0.0, 60.0);     // starts at 0ms
/// let bar2 = timeline.add(100, 500, 0.0, 60.0);   // starts at 100ms
/// let bar3 = timeline.add(200, 500, 0.0, 60.0);   // starts at 200ms
///
/// // Enable ping-pong and infinite loop
/// timeline.set_alternate(true);
/// timeline.set_loop(-1);
/// timeline.start();
/// ```
pub struct Timeline {
    entries: SlotMap<TimelineEntryId, TimelineEntry>,
    current_time: f32,
    duration_ms: u32,
    playing: bool,
    loop_count: i32, // -1 for infinite
    current_loop: i32,
    /// Whether to reverse direction on each loop (ping-pong/alternate mode)
    alternate: bool,
    /// Whether currently playing in reverse
    reversed: bool,
    /// Playback rate (1.0 = normal, 2.0 = 2x speed, 0.5 = half speed)
    playback_rate: f32,
}

impl Timeline {
    pub fn new() -> Self {
        Self {
            entries: SlotMap::with_key(),
            current_time: 0.0,
            duration_ms: 0,
            playing: false,
            loop_count: 1,
            current_loop: 0,
            alternate: false,
            reversed: false,
            playback_rate: 1.0,
        }
    }

    /// Add an animation to the timeline at a given offset
    pub fn add(
        &mut self,
        offset_ms: i32,
        duration_ms: u32,
        start_value: f32,
        end_value: f32,
    ) -> TimelineEntryId {
        self.add_with_easing(
            offset_ms,
            duration_ms,
            start_value,
            end_value,
            Easing::Linear,
        )
    }

    /// Add an animation with a specific easing function
    pub fn add_with_easing(
        &mut self,
        offset_ms: i32,
        duration_ms: u32,
        start_value: f32,
        end_value: f32,
        easing: Easing,
    ) -> TimelineEntryId {
        let id = self.entries.insert(TimelineEntry {
            offset_ms,
            duration_ms,
            start_value,
            end_value,
            easing,
        });

        // Update total duration
        let end_time = (offset_ms.max(0) as u32) + duration_ms;
        self.duration_ms = self.duration_ms.max(end_time);

        id
    }

    /// Start the timeline from the beginning
    pub fn start(&mut self) {
        self.current_time = if self.reversed {
            self.duration_ms as f32
        } else {
            0.0
        };
        self.current_loop = 0;
        self.playing = true;
    }

    /// Stop the timeline
    pub fn stop(&mut self) {
        self.playing = false;
    }

    /// Pause the timeline (can be resumed)
    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Resume a paused timeline
    pub fn resume(&mut self) {
        self.playing = true;
    }

    /// Reverse the playback direction
    pub fn reverse(&mut self) {
        self.reversed = !self.reversed;
    }

    /// Seek to a specific time position (in milliseconds)
    pub fn seek(&mut self, time_ms: f32) {
        self.current_time = time_ms.clamp(0.0, self.duration_ms as f32);
    }

    /// Set loop count (-1 for infinite, 0 to disable, positive for specific count)
    pub fn set_loop(&mut self, count: i32) {
        self.loop_count = count;
    }

    /// Enable/disable alternate (ping-pong) mode
    ///
    /// When enabled, the timeline reverses direction each loop instead of
    /// jumping back to the start.
    pub fn set_alternate(&mut self, enabled: bool) {
        self.alternate = enabled;
    }

    /// Set playback rate (1.0 = normal speed)
    pub fn set_playback_rate(&mut self, rate: f32) {
        self.playback_rate = rate.max(0.0);
    }

    /// Check if timeline is currently playing
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Check if timeline is in reversed state
    pub fn is_reversed(&self) -> bool {
        self.reversed
    }

    /// Get the total duration of the timeline
    pub fn duration(&self) -> u32 {
        self.duration_ms
    }

    /// Get the current time position
    pub fn current_time(&self) -> f32 {
        self.current_time
    }

    /// Get current loop iteration
    pub fn current_loop(&self) -> i32 {
        self.current_loop
    }

    /// Get the number of entries in this timeline
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Get all entry IDs in this timeline
    pub fn entry_ids(&self) -> Vec<TimelineEntryId> {
        self.entries.keys().collect()
    }

    /// Advance the timeline by dt milliseconds
    pub fn tick(&mut self, dt_ms: f32) {
        if !self.playing {
            return;
        }

        let dt_adjusted = dt_ms * self.playback_rate;

        if self.reversed {
            self.current_time -= dt_adjusted;

            if self.current_time <= 0.0 {
                self.handle_boundary(0.0);
            }
        } else {
            self.current_time += dt_adjusted;

            if self.current_time >= self.duration_ms as f32 {
                self.handle_boundary(self.duration_ms as f32);
            }
        }
    }

    /// Handle reaching a timeline boundary (start or end)
    fn handle_boundary(&mut self, boundary_time: f32) {
        let should_loop = self.loop_count == -1 || self.current_loop < self.loop_count - 1;

        if should_loop {
            self.current_loop += 1;

            if self.alternate {
                // Ping-pong: reverse direction
                self.reversed = !self.reversed;
                self.current_time = boundary_time;
            } else {
                // Normal loop: jump back to start
                self.current_time = if self.reversed {
                    self.duration_ms as f32
                } else {
                    0.0
                };
            }
        } else {
            // Animation complete
            self.current_time = boundary_time;
            self.playing = false;
        }
    }

    /// Get the current value for an animation entry
    pub fn value(&self, id: TimelineEntryId) -> Option<f32> {
        let entry = self.entries.get(id)?;

        // Calculate local time relative to entry offset
        let local_time = self.current_time - entry.offset_ms as f32;

        // Before entry starts
        if local_time < 0.0 {
            return Some(entry.start_value);
        }

        // After entry ends
        if local_time >= entry.duration_ms as f32 {
            return Some(entry.end_value);
        }

        // Calculate progress (0.0 to 1.0)
        let progress = local_time / entry.duration_ms as f32;

        // Apply easing
        let eased_progress = entry.easing.apply(progress);

        // Interpolate value
        Some(entry.start_value + (entry.end_value - entry.start_value) * eased_progress)
    }

    /// Get progress of a specific entry (0.0 to 1.0)
    pub fn entry_progress(&self, id: TimelineEntryId) -> Option<f32> {
        let entry = self.entries.get(id)?;
        let local_time = self.current_time - entry.offset_ms as f32;

        if local_time < 0.0 {
            Some(0.0)
        } else if local_time >= entry.duration_ms as f32 {
            Some(1.0)
        } else {
            Some(local_time / entry.duration_ms as f32)
        }
    }

    /// Get overall timeline progress (0.0 to 1.0)
    pub fn progress(&self) -> f32 {
        if self.duration_ms == 0 {
            return 1.0;
        }
        self.current_time / self.duration_ms as f32
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for creating staggered timeline entries
///
/// Utility to add multiple entries with automatic offset calculation.
pub struct StaggerBuilder<'a> {
    timeline: &'a mut Timeline,
    base_offset: i32,
    stagger: i32,
    current_index: usize,
}

impl<'a> StaggerBuilder<'a> {
    /// Create a new stagger builder
    pub fn new(timeline: &'a mut Timeline, base_offset: i32, stagger: i32) -> Self {
        Self {
            timeline,
            base_offset,
            stagger,
            current_index: 0,
        }
    }

    /// Add an entry with automatically calculated stagger offset
    pub fn add(&mut self, duration_ms: u32, start_value: f32, end_value: f32) -> TimelineEntryId {
        self.add_with_easing(duration_ms, start_value, end_value, Easing::Linear)
    }

    /// Add an entry with easing and automatically calculated stagger offset
    pub fn add_with_easing(
        &mut self,
        duration_ms: u32,
        start_value: f32,
        end_value: f32,
        easing: Easing,
    ) -> TimelineEntryId {
        let offset = self.base_offset + (self.current_index as i32 * self.stagger);
        self.current_index += 1;
        self.timeline
            .add_with_easing(offset, duration_ms, start_value, end_value, easing)
    }
}

impl Timeline {
    /// Create a stagger builder for adding entries with automatic offset calculation
    ///
    /// # Arguments
    /// * `base_offset` - The offset of the first entry in milliseconds
    /// * `stagger` - The delay between each entry in milliseconds
    ///
    /// # Example
    /// ```ignore
    /// let mut timeline = Timeline::new();
    /// let mut stagger = timeline.stagger(0, 100);
    /// let bar1 = stagger.add(500, 0.0, 60.0);  // offset 0
    /// let bar2 = stagger.add(500, 0.0, 60.0);  // offset 100
    /// let bar3 = stagger.add(500, 0.0, 60.0);  // offset 200
    /// ```
    pub fn stagger(&mut self, base_offset: i32, stagger: i32) -> StaggerBuilder<'_> {
        StaggerBuilder::new(self, base_offset, stagger)
    }
}
