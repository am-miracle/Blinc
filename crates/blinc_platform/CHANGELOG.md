# Changelog

All notable changes to `blinc_platform` will be documented in this file.

## [Unreleased]

### Added
- `WindowConfig::max_frame_latency: u32` (default `2`, clamped `1..=3`) and the matching builder method. Caps how many frames the GPU is allowed to queue ahead of the currently-presented frame; lowering it halves the in-flight GPU memory pipelined for surfaces / command buffers / bind groups, at the cost of slightly higher input latency.
