# Changelog

All notable changes to `blinc_cn` will be documented in this file.

## [0.4.0] - 2026-04-05

### Changed
- Version bump to align with workspace 0.4.0 release

## [0.1.15] - 2026-03-22

### Fixed

- Removed CSS transition declarations from nav-link, sidebar-item, menubar-trigger, and menubar-item that caused hover-leave visual artifacts
- Sidebar item background set to transparent to prevent stale background on hover-leave
- Clippy warnings in menubar overlay functions (let-binding return)
- Toast slide distance adjusted to 200px for clear right-edge entry animation
- Toast enter/exit animations now use proper off-screen distance for all corner positions
