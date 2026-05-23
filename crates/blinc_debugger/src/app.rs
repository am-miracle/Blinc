//! Main application module for the debugger
//!
//! Handles window creation, event loop, and top-level state management.
//! Layout matches the Phase 12 plan with four main panels:
//! - Tree Panel (left): Element tree with diff
//! - Preview Panel (center): UI preview
//! - Inspector Panel (right): Element properties
//! - Timeline Panel (bottom): Event timeline with scrubber

use crate::panels::{
    InspectorPanel, PreviewConfig, PreviewPanel, TimelinePanel, TimelinePanelState, TreePanel,
    TreePanelState,
};
use crate::theme::DebuggerColors;
use anyhow::Result;
use blinc_app::WindowConfig;
use blinc_app::windowed::{WindowedApp, WindowedContext};
use blinc_layout::prelude::*;
use blinc_recorder::replay::{ReplayConfig, ReplayPlayer, ReplayState};
use blinc_recorder::{ElementSnapshot, RecordingExport, TreeSnapshot};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

/// Application state
#[derive(Default)]
pub struct AppState {
    /// Loaded recording (if any)
    pub recording: Option<RecordingExport>,
    /// Replay player (if recording loaded)
    pub player: Option<Arc<Mutex<ReplayPlayer>>>,
    /// Current tree snapshot
    pub current_snapshot: Option<TreeSnapshot>,
    /// Selected element ID
    pub selected_element_id: Option<String>,
    /// Tree panel state
    pub tree_state: TreePanelState,
    /// Preview config
    pub preview_config: PreviewConfig,
    /// Timeline state
    pub timeline_state: TimelinePanelState,
    /// Connected to debug server
    pub connected: bool,
    /// Server address
    pub server_addr: Option<String>,
}

impl AppState {
    /// Load a recording from file
    pub fn load_recording(&mut self, path: &PathBuf) -> Result<()> {
        let contents = std::fs::read_to_string(path)?;
        let export: RecordingExport = serde_json::from_str(&contents)?;

        let player = ReplayPlayer::new(export.clone(), ReplayConfig::interactive());

        // Update timeline state with duration
        self.timeline_state.duration = player.duration();
        self.timeline_state.playback_state = ReplayState::Idle;

        // Load initial snapshot if available
        if let Some(snapshot) = export.snapshots.first() {
            self.current_snapshot = Some(snapshot.clone());
        }

        self.recording = Some(export);
        self.player = Some(Arc::new(Mutex::new(player)));

        log::info!("Loaded recording from {}", path.display());
        Ok(())
    }

    /// Get the selected element snapshot
    pub fn selected_element(&self) -> Option<&ElementSnapshot> {
        let snapshot = self.current_snapshot.as_ref()?;
        let id = self.selected_element_id.as_ref()?;
        snapshot.elements.get(id)
    }

    /// Get cursor position during replay
    pub fn cursor_position(&self) -> Option<(f32, f32)> {
        // TODO: Get from replay player's simulator
        None
    }
}

/// Shared application state for thread-safe access
pub type SharedAppState = Arc<RwLock<AppState>>;

/// Run the debugger application
pub fn run(width: u32, height: u32, file: Option<PathBuf>, connect: Option<String>) -> Result<()> {
    // Create shared application state
    let app_state = Arc::new(RwLock::new(AppState::default()));

    // Load recording if file path provided
    if let Some(ref path) = file {
        if let Err(e) = app_state.write().unwrap().load_recording(path) {
            log::warn!("Failed to load recording from {:?}: {}", path, e);
        }
    }

    // Store server address for later connection
    if let Some(ref addr) = connect {
        app_state.write().unwrap().server_addr = Some(addr.clone());
    }

    // Configure the window
    let config = WindowConfig {
        title: "Blinc Debugger".to_string(),
        width,
        height,
        resizable: true,
        ..Default::default()
    };

    // Run the windowed application
    let state_for_ui = app_state.clone();
    Ok(WindowedApp::run(config, move |ctx| {
        build_debugger_ui(ctx, &state_for_ui)
    })?)
}

/// Build the debugger UI using WindowedContext
fn build_debugger_ui(
    ctx: &WindowedContext,
    app_state: &SharedAppState,
) -> impl ElementBuilder + use<> {
    let state = app_state.read().unwrap();

    div()
        .w(ctx.width)
        .h(ctx.height)
        .bg(DebuggerColors::bg_base())
        .flex_col()
        .child(
            // Main panel area (tree + preview + inspector)
            div()
                .flex_grow()
                .flex_row()
                // Tree Panel (left)
                .child(TreePanel::new(
                    state.current_snapshot.as_ref(),
                    &state.tree_state,
                ))
                // Preview Panel (center)
                .child(PreviewPanel::new(
                    state.current_snapshot.as_ref(),
                    &state.preview_config,
                    state.cursor_position(),
                ))
                // Inspector Panel (right)
                .child(InspectorPanel::new(state.selected_element())),
        )
        // Timeline Panel (bottom)
        .child(TimelinePanel::new(
            state
                .recording
                .as_ref()
                .map(|r| r.events.as_slice())
                .unwrap_or(&[]),
            &state.timeline_state,
        ))
}
