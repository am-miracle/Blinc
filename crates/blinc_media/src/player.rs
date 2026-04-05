//! Shared player trait for audio and video

/// Common playback controls shared by audio and video players
pub trait Player {
    /// Start or resume playback
    fn play(&self);
    /// Pause playback
    fn pause(&self);
    /// Stop playback and reset
    fn stop(&self);
    /// Seek to position in milliseconds
    fn seek(&self, position_ms: u64);
    /// Get current position in milliseconds
    fn position_ms(&self) -> u64;
    /// Get total duration in milliseconds (0 if unknown/streaming)
    fn duration_ms(&self) -> u64;
    /// Get current volume (0.0 to 1.0)
    fn volume(&self) -> f32;
    /// Set volume (0.0 to 1.0)
    fn set_volume(&self, volume: f32);
    /// Check if currently playing
    fn is_playing(&self) -> bool;
    /// Check if this is a live stream (no seek, no duration)
    fn is_live(&self) -> bool {
        false
    }
}
