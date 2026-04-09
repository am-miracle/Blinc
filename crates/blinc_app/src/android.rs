//! Android application runner
//!
//! Provides a unified API for running Blinc applications on Android.
//!
//! # Example
//!
//! ```ignore
//! use blinc_app::prelude::*;
//! use blinc_app::android::AndroidApp;
//!
//! #[no_mangle]
//! fn android_main(app: android_activity::AndroidApp) {
//!     AndroidApp::run(app, |ctx| {
//!         div().w(ctx.width).h(ctx.height)
//!             .bg([0.1, 0.1, 0.15, 1.0])
//!             .flex_center()
//!             .child(text("Hello Android!").size(48.0))
//!     }).unwrap();
//! }
//! ```

use std::sync::{
    atomic::{AtomicBool, AtomicI32, Ordering},
    Arc, Mutex,
};

/// Latest soft-keyboard inset reported by the JVM in **logical pixels**.
///
/// Set from Kotlin via the JNI export
/// `Java_com_blinc_BlincNativeBridge_nativeDispatchKeyboardInset` (defined
/// below in `blinc_app::android`), which `BlincNativeBridge` invokes from
/// its `setOnApplyWindowInsetsListener` whenever
/// `WindowInsets.Type.ime().bottom` changes. The Kotlin side does the
/// `pixels_raw / display_density` division before pushing, so this value
/// is directly comparable to `WindowedContext.height`.
///
/// `i32` because `AtomicF32` isn't in stable std; we round to the nearest
/// pixel on the Kotlin side, which is more than enough resolution for
/// keyboard-inset triggered scroll-into-view.
///
/// `-1` is the sentinel meaning "no value pushed yet" (so a stale `0`
/// from a previous run can't accidentally suppress the first real
/// notification). The android_main loop converts `-1` to `0.0`.
static PENDING_IME_INSET_PX: AtomicI32 = AtomicI32::new(-1);

use android_activity::input::{
    InputEvent as AndroidInputEvent, KeyAction, KeyMapChar, Keycode, MotionAction,
};
use android_activity::{AndroidApp as NdkAndroidApp, InputStatus, MainEvent, PollEvent};
use ndk::native_window::NativeWindow;

use blinc_animation::AnimationScheduler;
use blinc_core::context_state::{BlincContextState, HookState, SharedHookState};
use blinc_core::reactive::{ReactiveGraph, SignalId};
use blinc_layout::event_router::MouseButton;
use blinc_layout::overlay_state::OverlayContext;
use blinc_layout::prelude::*;
use blinc_layout::widgets::overlay::{overlay_manager, OverlayManager};
use blinc_platform::assets::set_global_asset_loader;
use blinc_platform_android::input::{detect_pinch, PinchPhase, PinchState, TouchPointer};
use blinc_platform_android::AndroidAssetLoader;

use crate::app::BlincApp;
use crate::error::{BlincError, Result};
use crate::windowed::{
    RefDirtyFlag, SharedAnimationScheduler, SharedElementRegistry, SharedReactiveGraph,
    SharedReadyCallbacks, WindowedContext,
};

/// Android application runner
///
/// Provides a simple way to run a Blinc application on Android
/// with automatic event handling and rendering.
pub struct AndroidApp;

impl AndroidApp {
    /// Initialize the Android asset loader
    fn init_asset_loader(app: NdkAndroidApp) {
        let loader = AndroidAssetLoader::new(app);
        let _ = set_global_asset_loader(Box::new(loader));
    }

    /// Initialize the theme system
    fn init_theme() {
        use blinc_theme::{
            detect_system_color_scheme, platform_theme_bundle, set_redraw_callback, ThemeState,
        };

        // Only initialize if not already initialized
        if ThemeState::try_get().is_none() {
            let bundle = platform_theme_bundle();
            let scheme = detect_system_color_scheme();
            ThemeState::init(bundle, scheme);
        }

        // Set up the redraw callback
        set_redraw_callback(|| {
            tracing::debug!("Theme changed - requesting full rebuild");
            blinc_layout::widgets::request_full_rebuild();
        });
    }

    /// Initialize Android logging
    fn init_logging() {
        // Initialize android_logger for log crate
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Debug)
                .with_tag("Blinc"),
        );

        // Initialize tracing-android for tracing crate
        use tracing_subscriber::layer::SubscriberExt;
        let subscriber =
            tracing_subscriber::registry().with(tracing_android::layer("Blinc").unwrap());
        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    /// Run an Android Blinc application
    ///
    /// This is the main entry point for Android applications. It sets up
    /// the GPU renderer, handles lifecycle events, and runs the event loop.
    ///
    /// # Arguments
    ///
    /// * `app` - The AndroidApp from android-activity
    /// * `ui_builder` - Function that builds the UI tree given the window context
    ///
    /// # Example
    ///
    /// ```ignore
    /// AndroidApp::run(app, |ctx| {
    ///     div()
    ///         .w(ctx.width).h(ctx.height)
    ///         .bg([0.1, 0.1, 0.15, 1.0])
    ///         .flex_center()
    ///         .child(text("Hello Android!").size(32.0))
    /// })
    /// ```
    pub fn run<F, E>(app: NdkAndroidApp, mut ui_builder: F) -> Result<()>
    where
        F: FnMut(&mut WindowedContext) -> E + 'static,
        E: ElementBuilder + 'static,
    {
        // Initialize logging first
        Self::init_logging();
        tracing::info!("AndroidApp::run starting");

        // Initialize the asset loader
        Self::init_asset_loader(app.clone());

        // Initialize the text measurer
        crate::text_measurer::init_text_measurer();

        // Initialize the theme system
        Self::init_theme();

        // Shared state
        let ref_dirty_flag: RefDirtyFlag = Arc::new(AtomicBool::new(false));
        let reactive: SharedReactiveGraph = Arc::new(Mutex::new(ReactiveGraph::new()));
        let hooks: SharedHookState = Arc::new(Mutex::new(HookState::new()));

        // Initialize global context state singleton
        if !BlincContextState::is_initialized() {
            #[allow(clippy::type_complexity)]
            let stateful_callback: Arc<dyn Fn(&[SignalId]) + Send + Sync> =
                Arc::new(|signal_ids| {
                    blinc_layout::check_stateful_deps(signal_ids);
                });
            BlincContextState::init_with_callback(
                Arc::clone(&reactive),
                Arc::clone(&hooks),
                Arc::clone(&ref_dirty_flag),
                stateful_callback,
            );
        }

        // Animation scheduler - single-threaded for mobile efficiency
        // Unlike desktop, we tick animations on main thread to avoid mutex contention
        // and high CPU usage from background thread + main thread fighting
        let scheduler = AnimationScheduler::new();
        let animations: SharedAnimationScheduler = Arc::new(Mutex::new(scheduler));

        // Set global scheduler handle
        {
            let scheduler_handle = animations.lock().unwrap().handle();
            blinc_animation::set_global_scheduler(scheduler_handle);
        }

        // Element registry for query API
        let element_registry: SharedElementRegistry =
            Arc::new(blinc_layout::selector::ElementRegistry::new());

        // Set up query callback
        {
            let registry_for_query = Arc::clone(&element_registry);
            let query_callback: blinc_core::QueryCallback = Arc::new(move |id: &str| {
                registry_for_query.get(id).map(|node_id| node_id.to_raw())
            });
            BlincContextState::get().set_query_callback(query_callback);
        }

        // Set up bounds callback
        {
            let registry_for_bounds = Arc::clone(&element_registry);
            let bounds_callback: blinc_core::BoundsCallback =
                Arc::new(move |id: &str| registry_for_bounds.get_bounds(id));
            BlincContextState::get().set_bounds_callback(bounds_callback);
        }

        // Store element registry in BlincContextState
        BlincContextState::get()
            .set_element_registry(Arc::clone(&element_registry) as blinc_core::AnyElementRegistry);

        // Ready callbacks
        let ready_callbacks: SharedReadyCallbacks = Arc::new(Mutex::new(Vec::new()));

        // Overlay manager
        let overlays: OverlayManager = overlay_manager();
        if !OverlayContext::is_initialized() {
            OverlayContext::init(Arc::clone(&overlays));
        }

        // Connect theme animation to scheduler
        blinc_theme::ThemeState::get().set_scheduler(&animations);

        // Render state and motion states
        let shared_motion_states = blinc_layout::create_shared_motion_states();

        // Set up motion state callback
        {
            let motion_states_for_callback = Arc::clone(&shared_motion_states);
            let motion_callback: blinc_core::MotionStateCallback = Arc::new(move |key: &str| {
                motion_states_for_callback
                    .read()
                    .ok()
                    .and_then(|states| states.get(key).copied())
                    .unwrap_or(blinc_core::MotionAnimationState::NotFound)
            });
            BlincContextState::get().set_motion_state_callback(motion_callback);
        }

        // Application state
        let mut blinc_app: Option<BlincApp> = None;
        let mut surface: Option<wgpu::Surface<'static>> = None;
        let mut surface_config: Option<wgpu::SurfaceConfiguration> = None;
        let mut ctx: Option<WindowedContext> = None;
        let mut render_tree: Option<RenderTree> = None;
        let mut render_state: Option<blinc_layout::RenderState> = None;
        let mut native_window: Option<NativeWindow> = None;
        let mut needs_rebuild = true;
        let mut needs_redraw_next_frame = false;
        let mut last_frame_time_ms: u64 = 0;
        let mut running = true;
        let mut focused = false;
        // Latest keyboard inset already pushed into `windowed_ctx.keyboard_inset`.
        // The poll loop reads `PENDING_IME_INSET_PX` (set from Kotlin via the
        // `Java_com_blinc_BlincNativeBridge_nativeDispatchKeyboardInset` JNI
        // export) and only triggers `scroll_focused_text_input_above_keyboard`
        // when the value changes — otherwise we'd re-clamp the focused
        // input's container scroll offset every vsync tick and fight the
        // user trying to pan around.
        let mut last_applied_keyboard_inset_px: i32 = -1;
        // Last value of `text_input::focus_tap_generation()` we processed.
        // The widget bumps that counter on every `on_mouse_down` that
        // lands on a text input — that's the signal we use to detect
        // re-taps and same-frame focus swaps that
        // `take_keyboard_state_change` misses (because that flag only
        // fires on `0 → 1` / `1 → 0` focus-count transitions).
        let mut last_focus_tap_generation: u64 = 0;

        // Touch tracking for scroll delta calculation
        // On mobile, scroll happens via touch drag, not wheel events
        let mut last_touch_x: Option<f32> = None;
        let mut last_touch_y: Option<f32> = None;
        let mut is_scrolling = false;
        let mut pinch_state = PinchState::default();

        tracing::info!("Entering Android event loop");

        while running {
            // When animating: don't wait - vsync in present() handles frame pacing
            // When idle: wait for events to save power
            let poll_timeout = if needs_rebuild || needs_redraw_next_frame {
                Some(std::time::Duration::ZERO) // Don't wait - vsync paces us
            } else {
                Some(std::time::Duration::from_millis(100)) // Idle - save power
            };
            needs_redraw_next_frame = false;

            app.poll_events(poll_timeout, |event| {
                match event {
                    PollEvent::Main(main_event) => match main_event {
                        MainEvent::InitWindow { .. } => {
                            tracing::info!("Native window initialized");
                            if let Some(window) = app.native_window() {
                                let width = window.width() as u32;
                                let height = window.height() as u32;
                                tracing::info!("Window size: {}x{}", width, height);

                                // Initialize GPU with native window
                                match Self::init_gpu(&window) {
                                    Ok((app_instance, surf)) => {
                                        let format = app_instance.texture_format();
                                        // Use `Inherit` rather than `Auto`.
                                        //
                                        // The Pixel 10 Pro / Tensor G5
                                        // PowerVR Vulkan driver
                                        // (25.1@6794074) ONLY reports
                                        // `[Inherit]` as a supported
                                        // composite alpha mode — `Opaque`
                                        // is rejected at `Surface::configure`
                                        // with a validation error. `Auto`
                                        // also resolves to `Inherit` here,
                                        // but goes through a code path that
                                        // produced a blank/black surface.
                                        // Forcing `Inherit` explicitly works
                                        // around it. Per the Vulkan spec
                                        // (VK_COMPOSITE_ALPHA_INHERIT_BIT_KHR),
                                        // the application is responsible for
                                        // configuring the host window's
                                        // alpha treatment — we do that on
                                        // the Java side by setting
                                        // `window.setFormat(PixelFormat.OPAQUE)`
                                        // in `MainActivity.onCreate` so the
                                        // SurfaceFlinger composes our
                                        // framebuffer as fully opaque.
                                        let alpha_mode = wgpu::CompositeAlphaMode::Inherit;
                                        tracing::info!(
                                            "Android surface: format={:?}, alpha_mode={:?}, size={}x{}",
                                            format,
                                            alpha_mode,
                                            width,
                                            height,
                                        );

                                        let config = wgpu::SurfaceConfiguration {
                                            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                                            format,
                                            width,
                                            height,
                                            present_mode: wgpu::PresentMode::AutoVsync,
                                            alpha_mode,
                                            view_formats: vec![],
                                            desired_maximum_frame_latency: 2,
                                        };
                                        surf.configure(app_instance.device(), &config);

                                        // Update text measurer
                                        crate::text_measurer::init_text_measurer_with_registry(
                                            app_instance.font_registry(),
                                        );

                                        surface = Some(surf);
                                        surface_config = Some(config);
                                        blinc_app = Some(app_instance);
                                        native_window = Some(window);

                                        // Create WindowedContext with actual display density
                                        let scale_factor =
                                            blinc_platform_android::get_display_density(&app);
                                        let logical_width = width as f32 / scale_factor as f32;
                                        let logical_height = height as f32 / scale_factor as f32;

                                        ctx = Some(WindowedContext::new_android(
                                            logical_width,
                                            logical_height,
                                            scale_factor,
                                            width as f32,
                                            height as f32,
                                            focused,
                                            Arc::clone(&animations),
                                            Arc::clone(&ref_dirty_flag),
                                            Arc::clone(&reactive),
                                            Arc::clone(&hooks),
                                            Arc::clone(&overlays),
                                            Arc::clone(&element_registry),
                                            Arc::clone(&ready_callbacks),
                                        ));

                                        // Set viewport size
                                        BlincContextState::get()
                                            .set_viewport_size(logical_width, logical_height);

                                        // Initialize render state
                                        let mut rs =
                                            blinc_layout::RenderState::new(Arc::clone(&animations));
                                        rs.set_shared_motion_states(Arc::clone(
                                            &shared_motion_states,
                                        ));
                                        render_state = Some(rs);

                                        needs_rebuild = true;
                                        tracing::info!("GPU initialized successfully");
                                    }
                                    Err(e) => {
                                        tracing::error!("Failed to initialize GPU: {}", e);
                                    }
                                }
                            }
                        }

                        MainEvent::TerminateWindow { .. } => {
                            tracing::info!("Native window terminated");
                            native_window = None;
                            surface = None;
                            surface_config = None;
                            blinc_app = None;
                            ctx = None;
                            render_tree = None;
                            render_state = None;
                        }

                        MainEvent::WindowResized { .. } => {
                            if let Some(ref window) = native_window {
                                let width = window.width() as u32;
                                let height = window.height() as u32;
                                tracing::info!("Window resized: {}x{}", width, height);

                                if let (
                                    Some(ref app_instance),
                                    Some(ref surf),
                                    Some(ref mut config),
                                ) = (&blinc_app, &surface, &mut surface_config)
                                {
                                    if width > 0 && height > 0 {
                                        config.width = width;
                                        config.height = height;
                                        surf.configure(app_instance.device(), config);

                                        if let Some(ref mut windowed_ctx) = ctx {
                                            let scale_factor = windowed_ctx.scale_factor;
                                            windowed_ctx.width = width as f32 / scale_factor as f32;
                                            windowed_ctx.height =
                                                height as f32 / scale_factor as f32;

                                            BlincContextState::get().set_viewport_size(
                                                windowed_ctx.width,
                                                windowed_ctx.height,
                                            );
                                        }

                                        needs_rebuild = true;
                                    }
                                }
                            }
                        }

                        MainEvent::GainedFocus => {
                            tracing::info!("App gained focus");
                            focused = true;
                            if let Some(ref mut windowed_ctx) = ctx {
                                windowed_ctx.focused = true;
                            }
                        }

                        MainEvent::LostFocus => {
                            tracing::info!("App lost focus");
                            focused = false;
                            if let Some(ref mut windowed_ctx) = ctx {
                                windowed_ctx.focused = false;
                            }
                        }

                        MainEvent::Resume { .. } => {
                            tracing::info!("App resumed");
                            focused = true;
                        }

                        MainEvent::Pause => {
                            tracing::info!("App paused");
                            focused = false;
                        }

                        MainEvent::Destroy => {
                            tracing::info!("App destroyed");
                            running = false;
                        }

                        MainEvent::LowMemory => {
                            tracing::warn!("Low memory warning");
                            // TODO: Release caches
                        }

                        _ => {}
                    },

                    PollEvent::Wake => {
                        // Animation thread wake - request redraw only (NOT rebuild)
                        needs_redraw_next_frame = true;
                    }

                    _ => {}
                }
            });

            // Process touch/input events from android-activity
            // Collect pending events for dispatch (like desktop windowed.rs)
            #[derive(Clone, Default)]
            struct PendingEvent {
                node_id: blinc_layout::LayoutNodeId,
                event_type: u32,
                mouse_x: f32,
                mouse_y: f32,
            }

            let mut pending_events: Vec<PendingEvent> = Vec::new();
            // Track scroll info for dispatch after event processing (mouse_x, mouse_y, delta_x, delta_y)
            let mut scroll_info: Option<(f32, f32, f32, f32)> = None;
            // Track if touch ended (for scroll physics)
            let mut touch_ended = false;

            if let (Some(ref mut windowed_ctx), Some(ref mut tree)) = (&mut ctx, &mut render_tree) {
                // Get the scale factor for coordinate conversion
                let scale = windowed_ctx.scale_factor as f32;
                let router = &mut windowed_ctx.event_router;

                // Set up callback to collect events (like desktop)
                router.set_event_callback({
                    let events = &mut pending_events as *mut Vec<PendingEvent>;
                    move |node, event_type| {
                        // SAFETY: This callback is only used within this scope
                        unsafe {
                            (*events).push(PendingEvent {
                                node_id: node,
                                event_type,
                                ..Default::default()
                            });
                        }
                    }
                });

                // Process all pending input events
                match app.input_events_iter() {
                    Ok(mut input_iter) => {
                        // Handle input events using android-activity 0.6 API
                        while input_iter.next(|event| {
                            match event {
                                AndroidInputEvent::MotionEvent(motion_event) => {
                                    let action = motion_event.action();
                                    let pointer_count = motion_event.pointer_count();
                                    let action_index = motion_event.pointer_index();

                                    if pointer_count == 0 {
                                        if action == MotionAction::Cancel {
                                            tracing::debug!("Touch CANCEL");
                                            windowed_ctx.pointer_query.set_pressure(0.0);
                                            windowed_ctx.pointer_query.set_touch_count(0);
                                            router.on_mouse_leave();
                                            pinch_state.reset();
                                            last_touch_x = None;
                                            last_touch_y = None;
                                            if is_scrolling {
                                                touch_ended = true;
                                            }
                                            is_scrolling = false;
                                            return InputStatus::Handled;
                                        }
                                        return InputStatus::Unhandled;
                                    }

                                    if pointer_count > 0 {
                                        let pointer_idx = match action {
                                            MotionAction::PointerDown | MotionAction::PointerUp => {
                                                action_index
                                            }
                                            _ => 0,
                                        };

                                        let pointer = motion_event.pointer_at_index(pointer_idx);
                                        let lx = pointer.x() / scale;
                                        let ly = pointer.y() / scale;
                                        let pointers: Vec<TouchPointer> =
                                            (0..pointer_count)
                                                .map(|i| {
                                                    let p = motion_event.pointer_at_index(i);
                                                    TouchPointer {
                                                        id: p.pointer_id(),
                                                        x: p.x() / scale,
                                                        y: p.y() / scale,
                                                        pressure: p.pressure(),
                                                        size: p.size(),
                                                    }
                                                })
                                                .collect();

                                        // Forward pressure and touch count to pointer query
                                        if let Some(primary) = pointers.first() {
                                            windowed_ctx.pointer_query.set_pressure(primary.pressure);
                                        }
                                        windowed_ctx.pointer_query.set_touch_count(pointers.len() as u32);

                                        let pinch_gesture = detect_pinch(&pointers, &mut pinch_state);
                                        if let Some(gesture) = pinch_gesture {
                                            if matches!(gesture.phase, PinchPhase::Started | PinchPhase::Moved)
                                            {
                                                if let Some(hit) =
                                                    router.hit_test(tree, gesture.center.0, gesture.center.1)
                                                {
                                                    tree.dispatch_pinch_chain(
                                                        &hit,
                                                        gesture.center.0,
                                                        gesture.center.1,
                                                        gesture.scale,
                                                    );
                                                    needs_redraw_next_frame = true;
                                                }
                                            }

                                            if matches!(gesture.phase, PinchPhase::Started | PinchPhase::Ended)
                                            {
                                                last_touch_x = None;
                                                last_touch_y = None;
                                                is_scrolling = false;
                                            }
                                        }

                                        match action {
                                            MotionAction::Down | MotionAction::PointerDown => {
                                                tracing::debug!(
                                                    "Touch DOWN at logical ({:.1}, {:.1})",
                                                    lx,
                                                    ly
                                                );
                                                // Mark this event as touch input
                                                // so editable widgets can branch
                                                // their drag / double-tap logic
                                                // for mobile semantics. See
                                                // `widgets::text_input::is_touch_input`.
                                                blinc_layout::widgets::text_input::set_touch_input(true);
                                                // Blur any focused text inputs
                                                // BEFORE processing the touch.
                                                // The text input that gets
                                                // tapped re-focuses itself via
                                                // its own `on_mouse_down`
                                                // handler; tapping outside
                                                // any input clears focus,
                                                // which decrements the focus
                                                // count, which fires
                                                // `take_keyboard_state_change`
                                                // on the next frame so the
                                                // soft keyboard hides
                                                // automatically. Mirrors the
                                                // desktop runner at
                                                // `windowed.rs:2913` and the
                                                // iOS runner.
                                                blinc_layout::widgets::blur_all_text_inputs();
                                                router.on_mouse_down(
                                                    &*tree,
                                                    lx,
                                                    ly,
                                                    MouseButton::Left,
                                                );
                                                // Initialize touch tracking for scroll
                                                if pointer_count == 1 {
                                                    last_touch_x = Some(lx);
                                                    last_touch_y = Some(ly);
                                                    is_scrolling = false;
                                                }
                                                // Update pending events with coordinates
                                                unsafe {
                                                    let events = &mut pending_events
                                                        as *mut Vec<PendingEvent>;
                                                    for event in (*events).iter_mut() {
                                                        event.mouse_x = lx;
                                                        event.mouse_y = ly;
                                                    }
                                                }
                                            }
                                            MotionAction::Move => {
                                                router.on_mouse_move(&*tree, lx, ly);

                                                if pointer_count == 1 {
                                                    // Calculate scroll delta from touch movement
                                                    // Touch: dragging down = positive delta = content scrolls up (shows below)
                                                    if let (Some(prev_x), Some(prev_y)) =
                                                        (last_touch_x, last_touch_y)
                                                    {
                                                        let delta_x = lx - prev_x;
                                                        let delta_y = ly - prev_y;

                                                        // Only collect scroll if there's actual movement
                                                        // Small threshold to avoid jitter
                                                        if delta_x.abs() > 0.5 || delta_y.abs() > 0.5 {
                                                            is_scrolling = true;
                                                            // Store scroll info for dispatch after event loop
                                                            scroll_info =
                                                                Some((lx, ly, delta_x, delta_y));
                                                            tracing::trace!(
                                                                "Touch scroll: delta=({:.1}, {:.1})",
                                                                delta_x,
                                                                delta_y
                                                            );
                                                        }
                                                    }

                                                    // Update last touch position
                                                    last_touch_x = Some(lx);
                                                    last_touch_y = Some(ly);
                                                }

                                                unsafe {
                                                    let events = &mut pending_events
                                                        as *mut Vec<PendingEvent>;
                                                    for event in (*events).iter_mut() {
                                                        event.mouse_x = lx;
                                                        event.mouse_y = ly;
                                                    }
                                                }
                                            }
                                            MotionAction::Up | MotionAction::PointerUp => {
                                                tracing::debug!(
                                                    "Touch UP at logical ({:.1}, {:.1})",
                                                    lx,
                                                    ly
                                                );
                                                windowed_ctx.pointer_query.set_pressure(0.0);
                                                router.on_mouse_up(&*tree, lx, ly, MouseButton::Left);

                                                // Mark touch ended for scroll physics
                                                if is_scrolling {
                                                    touch_ended = true;
                                                }
                                                // Clear touch tracking
                                                last_touch_x = None;
                                                last_touch_y = None;
                                                is_scrolling = false;

                                                unsafe {
                                                    let events = &mut pending_events
                                                        as *mut Vec<PendingEvent>;
                                                    for event in (*events).iter_mut() {
                                                        event.mouse_x = lx;
                                                        event.mouse_y = ly;
                                                    }
                                                }
                                            }
                                            MotionAction::Cancel => {
                                                tracing::debug!("Touch CANCEL");
                                                windowed_ctx.pointer_query.set_pressure(0.0);
                                                windowed_ctx.pointer_query.set_touch_count(0);
                                                router.on_mouse_leave();
                                                pinch_state.reset();
                                                // Clear touch tracking on cancel too
                                                last_touch_x = None;
                                                last_touch_y = None;
                                                if is_scrolling {
                                                    touch_ended = true;
                                                }
                                                is_scrolling = false;
                                            }
                                            _ => {}
                                        }
                                        InputStatus::Handled
                                    } else {
                                        InputStatus::Unhandled
                                    }
                                }
                                AndroidInputEvent::KeyEvent(key_event) => {
                                    // Forward soft-keyboard / hardware-keyboard
                                    // input to the Rust text-input widget. The
                                    // mirror of the iOS path in `ios.rs::handle_text_input`
                                    // / `handle_key_down` — without this, the
                                    // Android IME pops up correctly (the runner
                                    // already calls `app.show_soft_input(true)`
                                    // when `take_keyboard_state_change()` returns
                                    // `Some(true)`) but every typed character is
                                    // silently dropped because nothing forwards
                                    // it through to `broadcast_text_input_event`.
                                    //
                                    // We only react to `KeyAction::Down`. Up
                                    // events are uninteresting for the
                                    // text-input widget which advances state on
                                    // press, not release.
                                    if key_event.action() == KeyAction::Down {
                                        let key_code = key_event.key_code();
                                        let meta_state = key_event.meta_state();

                                        // Map Android `Keycode` -> the virtual
                                        // key codes the desktop runner uses
                                        // (which the `text_input` widget's
                                        // `on_key_down` handler matches against
                                        // at line 1639 of `text_input.rs`):
                                        //   8  = Backspace / Delete
                                        //   13 = Enter / Return
                                        //   27 = Escape
                                        //   37/38/39/40 = ←/↑/→/↓
                                        //   36 = Home, 35 = End
                                        // Anything not in the table falls
                                        // through to the unicode-character
                                        // path below, which broadcasts as
                                        // TEXT_INPUT instead.
                                        let virtual_key = match key_code {
                                            Keycode::Del => Some(8u32),
                                            Keycode::Enter | Keycode::NumpadEnter => Some(13u32),
                                            Keycode::Escape => Some(27u32),
                                            Keycode::DpadLeft => Some(37u32),
                                            Keycode::DpadUp => Some(38u32),
                                            Keycode::DpadRight => Some(39u32),
                                            Keycode::DpadDown => Some(40u32),
                                            Keycode::MoveHome => Some(36u32),
                                            Keycode::MoveEnd => Some(35u32),
                                            _ => None,
                                        };

                                        if let Some(vkey) = virtual_key {
                                            tree.broadcast_key_event(
                                                blinc_core::events::event_types::KEY_DOWN,
                                                vkey,
                                                false,
                                                false,
                                                false,
                                                false,
                                            );
                                        } else {
                                            // Unicode character path. The
                                            // android-activity API exposes a
                                            // per-device `KeyCharacterMap` that
                                            // maps `(key_code, meta_state)` to
                                            // a unicode char (or a combining
                                            // accent). For now we ignore
                                            // combining accents and
                                            // forward only direct unicode
                                            // characters — full dead-key
                                            // composition would track an
                                            // accent buffer across events,
                                            // mirroring the desktop runner.
                                            if let Ok(map) = app.device_key_character_map(
                                                key_event.device_id(),
                                            ) {
                                                if let Ok(KeyMapChar::Unicode(ch)) =
                                                    map.get(key_code, meta_state)
                                                {
                                                    tree.broadcast_text_input_event(
                                                        ch, false, false, false, false,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    InputStatus::Handled
                                }
                                _ => InputStatus::Unhandled,
                            }
                        }) {
                            // Event was processed, continue loop
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to get input events iterator: {:?}", e);
                    }
                }

                // Clear the callback
                router.clear_event_callback();
            } else {
                // Log when we can't process input (missing context or tree)
                if ctx.is_none() {
                    tracing::trace!("Input: ctx is None");
                }
                if render_tree.is_none() {
                    tracing::trace!("Input: render_tree is None");
                }
            }

            // Dispatch collected events to the tree (critical for click handlers!)
            //
            // Use `dispatch_event_full` (matches the desktop and iOS runners)
            // so the receiving handler sees `EventContext::local_x/local_y`
            // and `bounds_*` populated. The simpler `dispatch_event` only
            // forwards `mouse_x/mouse_y` and leaves the local-coordinate
            // fields at their default `0.0`, which silently broke any
            // handler that does click-to-position cursor placement,
            // hit-test math, or in-element coordinate work — most
            // visibly the `text_input` widget, which compiled
            // `cursor_position_from_x(0.0, _) == 0` on every tap and
            // dropped the caret at the start of the field on every
            // refocus. Look up the actual node bounds via
            // `EventRouter::get_node_bounds` so the local coordinates
            // remain correct even when the event has bubbled to an
            // ancestor whose bounds differ from the original hit target.
            if !pending_events.is_empty() {
                if let (Some(ref mut tree), Some(ref windowed_ctx)) =
                    (&mut render_tree, &ctx)
                {
                    let router = &windowed_ctx.event_router;
                    for event in pending_events {
                        let (bounds_x, bounds_y, bounds_width, bounds_height) = router
                            .get_node_bounds(event.node_id)
                            .unwrap_or((0.0, 0.0, 0.0, 0.0));
                        let local_x = event.mouse_x - bounds_x;
                        let local_y = event.mouse_y - bounds_y;
                        tracing::debug!(
                            "Dispatching event: node={:?}, type={}, pos=({:.1}, {:.1}), local=({:.1}, {:.1})",
                            event.node_id,
                            event.event_type,
                            event.mouse_x,
                            event.mouse_y,
                            local_x,
                            local_y,
                        );
                        tree.dispatch_event_full(
                            event.node_id,
                            event.event_type,
                            event.mouse_x,
                            event.mouse_y,
                            local_x,
                            local_y,
                            bounds_x,
                            bounds_y,
                            bounds_width,
                            bounds_height,
                            0.0, // drag_delta_x — touch drag uses scroll path
                            0.0, // drag_delta_y
                            1.0, // pinch_scale
                        );
                    }
                }
            }

            // Dispatch scroll events (touch scrolling)
            // NOTE: Do NOT set needs_rebuild here - that triggers full UI rebuild!
            // Scroll just updates internal offset and needs redraw, not rebuild.
            if let Some((mouse_x, mouse_y, delta_x, delta_y)) = scroll_info {
                if let (Some(ref mut windowed_ctx), Some(ref mut tree)) =
                    (&mut ctx, &mut render_tree)
                {
                    let router = &mut windowed_ctx.event_router;
                    // Hit test to get node chain for nested scroll dispatch
                    if let Some(hit) = router.hit_test(tree, mouse_x, mouse_y) {
                        // Get current time for velocity tracking (momentum scrolling)
                        let scroll_time = blinc_layout::prelude::elapsed_ms() as f64;
                        tracing::debug!(
                            "Dispatching scroll: hit={:?}, delta=({:.1}, {:.1})",
                            hit.node,
                            delta_x,
                            delta_y
                        );
                        tree.dispatch_scroll_chain_with_time(
                            hit.node,
                            &hit.ancestors,
                            mouse_x,
                            mouse_y,
                            delta_x,
                            delta_y,
                            scroll_time,
                        );
                        // Trigger redraw (NOT rebuild)
                        needs_redraw_next_frame = true;
                    }
                }
            }

            // Handle touch end - notify scroll physics for bounce/momentum
            if touch_ended {
                if let Some(ref mut tree) = render_tree {
                    tracing::debug!("Touch ended - notifying scroll physics");
                    tree.on_scroll_end();
                    // Trigger redraw for bounce animation (NOT rebuild)
                    needs_redraw_next_frame = true;
                }
            }

            // Tick scroll physics for momentum/bounce animations
            let scroll_animating = if let Some(ref mut tree) = render_tree {
                let current_time = blinc_layout::prelude::elapsed_ms();
                let animating = tree.tick_scroll_physics(current_time);
                tree.process_pending_scroll_refs();
                animating
            } else {
                false
            };
            if scroll_animating {
                needs_redraw_next_frame = true;
            }

            // =========================================================
            // Soft keyboard: show/hide based on text widget focus
            // =========================================================
            //
            // We prefer routing through the JVM `BlincNativeBridge`
            // (`keyboard.show` / `keyboard.hide`) which uses
            // `InputMethodManager.showSoftInput` against the Activity's
            // decor view. The NDK helper `ANativeActivity_showSoftInput`
            // (which `app.show_soft_input(true)` wraps) is famously
            // unreliable on modern Android — on most devices it silently
            // no-ops because the NativeActivity decor view is not
            // focusable in touch mode.
            //
            // The bridge is wired up by user code in `android_main` via
            // `blinc_platform_android::init_android_native_bridge(&app)`.
            // If it isn't initialized, we fall back to the unreliable NDK
            // path so apps that opt out of the bridge still get *something*.
            // Show / hide the soft keyboard when focus crosses the global
            // 0 / 1 boundary. `take_keyboard_state_change` returns
            // `Some(true)` on the first text input gaining focus and
            // `Some(false)` when the last focused input loses it; it does
            // NOT fire on re-taps or focus-swaps between two inputs.
            // That's fine for show/hide signaling — the keyboard is
            // already up in those cases. Re-tap detection lives below
            // via `focus_tap_generation`.
            if let Some(show) = blinc_layout::widgets::text_input::take_keyboard_state_change() {
                let bridge_ready =
                    blinc_core::native_bridge::NativeBridgeState::is_initialized();
                let routed_via_bridge = if bridge_ready {
                    let result: blinc_core::native_bridge::NativeResult<()> =
                        blinc_core::native_bridge::native_call(
                            "keyboard",
                            if show { "show" } else { "hide" },
                            (),
                        );
                    match result {
                        Ok(()) => true,
                        Err(e) => {
                            tracing::warn!(
                                "BlincNativeBridge keyboard.{} failed: {:?} — falling back to NDK helper",
                                if show { "show" } else { "hide" },
                                e
                            );
                            false
                        }
                    }
                } else {
                    false
                };

                if !routed_via_bridge {
                    if show {
                        app.show_soft_input(true);
                    } else {
                        app.hide_soft_input(true);
                    }
                }
            }

            // =========================================================
            // Soft-keyboard inset → scroll the focused text input above the
            // keyboard.
            //
            // Driven by `PENDING_IME_INSET_PX`, set from Kotlin via the
            // `Java_com_blinc_BlincNativeBridge_nativeDispatchKeyboardInset`
            // JNI export. The Kotlin side reads
            // `WindowInsets.Type.ime().bottom`, divides by display density,
            // and pushes the logical-pixel value here.
            //
            // We need TWO independent triggers because the focus-count
            // signal `take_keyboard_state_change` is not enough on its
            // own:
            //
            //   1. **Inset change** — keyboard slides in or out.
            //      Caught by diffing `pending_inset_px` against
            //      `last_applied_keyboard_inset_px`.
            //
            //   2. **Tap-on-text-input generation bump** — the user
            //      tapped a text input (any of them, including
            //      re-tapping the same one) and we should re-evaluate
            //      whether the focused input is currently obscured.
            //      `take_keyboard_state_change` misses this because it
            //      only fires on `0 → 1` / `1 → 0` focus-count
            //      transitions; re-tapping a focused input stays at
            //      count = 1 the whole time. We use a separate
            //      `focus_tap_generation` counter that bumps in the
            //      `text_input` widget's `on_mouse_down` handler.
            // =========================================================
            let pending_inset_px = PENDING_IME_INSET_PX.load(Ordering::Relaxed);
            let current_tap_gen = blinc_layout::widgets::text_input::focus_tap_generation();
            let inset_changed =
                pending_inset_px >= 0 && pending_inset_px != last_applied_keyboard_inset_px;
            let tap_changed = current_tap_gen != last_focus_tap_generation;
            let needs_scroll_pass = inset_changed
                || (tap_changed && pending_inset_px > 0);

            if needs_scroll_pass {
                let inset_to_apply = pending_inset_px.max(0) as f32;
                if let Some(ref mut windowed_ctx) = ctx {
                    windowed_ctx.keyboard_inset = inset_to_apply;
                    let viewport_h = windowed_ctx.height;
                    if let Some(ref mut tree) = render_tree {
                        let scrolled = tree
                            .scroll_focused_text_input_above_keyboard(viewport_h, inset_to_apply);
                        if scrolled {
                            needs_redraw_next_frame = true;
                        }
                    }
                    tracing::debug!(
                        "Android keyboard inset: last={} pending={} viewport_h={} tap_gen={}->{} inset_changed={} tap_changed={}",
                        last_applied_keyboard_inset_px,
                        pending_inset_px,
                        windowed_ctx.height,
                        last_focus_tap_generation,
                        current_tap_gen,
                        inset_changed,
                        tap_changed,
                    );
                }
                if pending_inset_px >= 0 {
                    last_applied_keyboard_inset_px = pending_inset_px;
                }
                last_focus_tap_generation = current_tap_gen;
            }

            // =========================================================
            // PHASE 1: Check for incremental updates (prop changes, subtree rebuilds)
            // This avoids full rebuild for simple state changes
            // =========================================================
            let mut needs_redraw = false;

            // Check if stateful elements requested a redraw (hover/press/state changes)
            let has_stateful_updates = blinc_layout::take_needs_redraw();
            let has_pending_rebuilds = blinc_layout::has_pending_subtree_rebuilds();

            if has_stateful_updates || has_pending_rebuilds {
                if has_stateful_updates {
                    tracing::debug!("Redraw requested by: stateful state change");
                }

                // Get all pending prop updates
                let prop_updates = blinc_layout::take_pending_prop_updates();
                let had_prop_updates = !prop_updates.is_empty();

                // Apply prop updates to the tree
                if let Some(ref mut tree) = render_tree {
                    for (node_id, props) in &prop_updates {
                        tree.update_render_props(*node_id, |p| *p = props.clone());
                    }
                }

                // Process subtree rebuilds
                let mut needs_layout = false;
                if let Some(ref mut tree) = render_tree {
                    needs_layout = tree.process_pending_subtree_rebuilds();
                }

                if needs_layout {
                    if let Some(ref mut tree) = render_tree {
                        if let Some(ref windowed_ctx) = ctx {
                            tracing::debug!("Subtree rebuilds processed, recomputing layout");
                            tree.compute_layout(windowed_ctx.width, windowed_ctx.height);
                            tree.apply_flip_transitions();
                            tree.update_flip_bounds();
                        }
                    }
                }

                if had_prop_updates && !needs_layout {
                    tracing::trace!("Visual-only prop updates, skipping layout");
                }

                needs_redraw = true;
            }

            // Check dirty flag from State::set() calls
            if ref_dirty_flag.swap(false, Ordering::SeqCst) {
                tracing::debug!("Rebuild triggered by: ref_dirty_flag (State::set)");
                needs_rebuild = true;
            }

            // Check if tree was marked dirty by event handlers
            if let Some(ref tree) = render_tree {
                if tree.needs_rebuild() {
                    tracing::debug!("Rebuild triggered by: tree.needs_rebuild()");
                    needs_rebuild = true;
                }
            }

            // Tick animations on main thread (single-threaded for mobile)
            let animations_active = {
                if let Ok(mut sched) = animations.lock() {
                    sched.tick()
                } else {
                    false
                }
            };
            if animations_active {
                needs_redraw = true;
                needs_redraw_next_frame = true;
            }

            // =========================================================
            // PHASE 2: Full rebuild only when structure changes
            // =========================================================
            static REBUILD_COUNT: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            if needs_rebuild && focused {
                let count = REBUILD_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if count % 60 == 0 {
                    tracing::warn!("REBUILD #{} (every 60th logged)", count);
                }
                if let (
                    Some(ref mut app_instance),
                    Some(ref surf),
                    Some(ref config),
                    Some(ref mut windowed_ctx),
                    Some(ref rs),
                ) = (
                    &mut blinc_app,
                    &surface,
                    &surface_config,
                    &mut ctx,
                    &render_state,
                ) {
                    // Build UI
                    let element = ui_builder(windowed_ctx);

                    // Clear stale Stateful base_render_props updaters before rebuild
                    blinc_layout::clear_stateful_base_updaters();
                    blinc_layout::click_outside::clear_click_outside_handlers();

                    // Create or update render tree
                    if render_tree.is_none() {
                        // First time: create tree
                        let mut tree = RenderTree::from_element(&element);
                        tree.set_scale_factor(windowed_ctx.scale_factor as f32);
                        if let Some(ref stylesheet) = windowed_ctx.stylesheet {
                            tree.set_stylesheet_arc(stylesheet.clone());
                        }
                        tree.apply_all_stylesheet_styles();
                        tree.compute_layout(windowed_ctx.width, windowed_ctx.height);
                        tree.update_flip_bounds();
                        tree.start_all_css_animations();
                        tree.clear_dirty(); // Start clean
                        render_tree = Some(tree);
                    } else if let Some(ref mut tree) = render_tree {
                        // Full rebuild
                        *tree = RenderTree::from_element(&element);
                        tree.set_scale_factor(windowed_ctx.scale_factor as f32);
                        if let Some(ref stylesheet) = windowed_ctx.stylesheet {
                            tree.set_stylesheet_arc(stylesheet.clone());
                        }
                        tree.apply_all_stylesheet_styles();
                        tree.compute_layout(windowed_ctx.width, windowed_ctx.height);
                        tree.update_flip_bounds();
                        tree.start_all_css_animations();
                        // Clear dirty on the NEW tree to prevent immediate re-rebuild
                        tree.clear_dirty();
                    }
                    needs_redraw = true;
                }
                // Reset rebuild flag after successful rebuild
                needs_rebuild = false;
            }

            // Tick CSS animations and apply state styles (visual-only)
            {
                let current_time = blinc_layout::prelude::elapsed_ms();
                if let Some(ref mut tree) = render_tree {
                    // Tick CSS animations synchronously to stay in phase with rendering
                    let dt_ms = if last_frame_time_ms > 0 {
                        (current_time - last_frame_time_ms) as f32
                    } else {
                        16.0
                    };
                    {
                        let store = tree.css_anim_store();
                        let mut s = store.lock().unwrap();
                        s.tick(dt_ms);
                    }
                    let flip_active = tree.tick_flip_animations(dt_ms);
                    let css_active =
                        tree.css_has_active() || !tree.css_transitions_empty() || flip_active;

                    // Apply CSS state styles (:hover, :active, :focus)
                    // This also detects property changes and starts new transitions
                    if let Some(ref windowed_ctx) = ctx {
                        if tree.stylesheet().is_some() {
                            tree.apply_stylesheet_state_styles(&windowed_ctx.event_router);
                        }
                    }
                    if css_active
                        || !tree.css_transitions_empty()
                        || tree.has_active_flip_animations()
                    {
                        tree.apply_all_css_animation_props();
                        tree.apply_all_css_transition_props();
                        tree.apply_flip_animation_props();
                        needs_redraw = true;
                        needs_redraw_next_frame = true;
                        // Apply animated layout properties and recompute layout if needed
                        if tree.apply_animated_layout_props() {
                            if let Some(ref windowed_ctx) = ctx {
                                tree.compute_layout(windowed_ctx.width, windowed_ctx.height);
                                tree.update_flip_bounds();
                            }
                        }
                    }
                }
                last_frame_time_ms = current_time;
            }

            // =========================================================
            // PHASE 3: Render if we need redraw
            // =========================================================
            static REDRAW_COUNT: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            if needs_redraw && focused {
                let count = REDRAW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if count % 120 == 0 {
                    tracing::info!("REDRAW #{} (every 120th logged)", count);
                }
                if let (
                    Some(ref mut app_instance),
                    Some(ref surf),
                    Some(ref config),
                    Some(ref mut windowed_ctx),
                    Some(ref rs),
                    Some(ref tree),
                ) = (
                    &mut blinc_app,
                    &surface,
                    &surface_config,
                    &mut ctx,
                    &render_state,
                    &render_tree,
                ) {
                    // Render
                    //
                    // The PowerVR Vulkan driver on the Pixel 10 Pro
                    // appears to mark every acquired SurfaceTexture as
                    // `suboptimal`, and on some drivers a suboptimal
                    // texture's contents are silently discarded during
                    // presentation. We log + reconfigure when that
                    // happens, mirroring the desktop runner. We also
                    // explicitly handle `Outdated`, which the wgpu 26
                    // surface API can return after the swapchain becomes
                    // stale (e.g. after a window resize the runner
                    // hasn't picked up yet).
                    let frame = surf.get_current_texture();
                    static SUBOPTIMAL_LOGGED: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    match frame {
                        Ok(output) => {
                            if output.suboptimal
                                && !SUBOPTIMAL_LOGGED
                                    .swap(true, std::sync::atomic::Ordering::Relaxed)
                            {
                                tracing::warn!(
                                    "SurfaceTexture is suboptimal — will reconfigure swapchain"
                                );
                            }
                            let view = output.texture.create_view(&Default::default());
                            if let Err(e) = app_instance.render_tree_with_motion(
                                tree,
                                rs,
                                &view,
                                config.width,
                                config.height,
                            ) {
                                tracing::error!("Render error: {}", e);
                            }
                            let was_suboptimal = output.suboptimal;
                            output.present();
                            if was_suboptimal {
                                surf.configure(app_instance.device(), config);
                            }
                        }
                        Err(wgpu::SurfaceError::Lost)
                        | Err(wgpu::SurfaceError::Outdated) => {
                            tracing::warn!(
                                "Surface lost / outdated — reconfiguring swapchain"
                            );
                            surf.configure(app_instance.device(), config);
                        }
                        Err(wgpu::SurfaceError::OutOfMemory) => {
                            tracing::error!("Out of GPU memory");
                            running = false;
                        }
                        Err(e) => {
                            tracing::error!("Surface error: {:?}", e);
                        }
                    }

                    // Increment rebuild count
                    windowed_ctx.rebuild_count += 1;

                    // Execute ready callbacks after first rebuild
                    if windowed_ctx.rebuild_count == 1 {
                        if let Ok(mut callbacks) = ready_callbacks.lock() {
                            for callback in callbacks.drain(..) {
                                callback();
                            }
                        }
                    }
                }

                needs_rebuild = false;
            }

            // =========================================================
            // PHASE 4: Check if we need another frame for animations
            // =========================================================
            {
                // Check animation scheduler for active animations
                if let Ok(scheduler) = animations.lock() {
                    if scheduler.has_active_animations() {
                        needs_redraw_next_frame = true;
                    }
                }

                // Check for animating stateful elements (spring animations, state transitions)
                if blinc_layout::has_animating_statefuls() {
                    needs_redraw_next_frame = true;
                }

                // Check for pending subtree rebuilds that might need processing
                if blinc_layout::has_pending_subtree_rebuilds() {
                    needs_redraw_next_frame = true;
                }
            }
        }

        tracing::info!("AndroidApp::run exiting");
        Ok(())
    }

    /// Initialize GPU with a native window
    fn init_gpu(window: &NativeWindow) -> Result<(BlincApp, wgpu::Surface<'static>)> {
        use blinc_gpu::{GpuRenderer, RendererConfig, TextRenderingContext};

        // Force the underlying ANativeWindow to use an opaque (no-alpha)
        // pixel format BEFORE we create the wgpu/Vulkan swapchain on it.
        //
        // Why: NativeActivity windows default to a TRANSLUCENT pixel
        // format on modern Android, which makes SurfaceFlinger composite
        // our framebuffer using its alpha channel. On the Pixel 10 Pro /
        // Tensor G5 PowerVR Vulkan driver this combines with `Inherit`
        // composite alpha to produce a fully invisible window even though
        // wgpu is rendering opaque content. `R8G8B8X8_UNORM` (the modern
        // alias of the legacy `WINDOW_FORMAT_RGBX_8888`) tells the
        // compositor "this surface has no alpha — treat every pixel as
        // opaque". The Java-side `window.setFormat(PixelFormat.OPAQUE)`
        // we set in MainActivity is silently overridden once the
        // NativeActivity's native window comes up, so this NDK-side call
        // (which lives in the same process and runs after InitWindow) is
        // the authoritative one. Calling it on a window that's already
        // RGBA8888 is harmless on devices where the bug doesn't apply.
        if let Err(e) = window.set_buffers_geometry(
            0,
            0,
            Some(ndk::hardware_buffer_format::HardwareBufferFormat::R8G8B8X8_UNORM),
        ) {
            tracing::warn!(
                "ANativeWindow_setBuffersGeometry(R8G8B8X8_UNORM) failed: {} \
                — surface may composite with alpha and appear blank on PowerVR-class GPUs",
                e
            );
        } else {
            tracing::info!(
                "ANativeWindow buffer format forced to R8G8B8X8_UNORM (opaque)"
            );
        }

        let config = crate::BlincConfig::default();

        let renderer_config = RendererConfig {
            max_primitives: config.max_primitives,
            max_glass_primitives: config.max_glass_primitives,
            max_glyphs: config.max_glyphs,
            sample_count: 1,
            texture_format: None,
            unified_text_rendering: true,
            ..RendererConfig::default()
        };

        // Create instance with Vulkan backend
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });

        // Create surface from native window using raw handles
        // Safety: The native window handle is valid for the lifetime of the window
        use raw_window_handle::{
            AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle,
        };
        use std::ptr::NonNull;

        let raw_window = NonNull::new(window.ptr().as_ptr() as *mut std::ffi::c_void)
            .ok_or_else(|| BlincError::GpuInit("Invalid native window pointer".to_string()))?;

        let window_handle = AndroidNdkWindowHandle::new(raw_window);
        let display_handle = AndroidDisplayHandle::new();

        let surface_target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: RawDisplayHandle::Android(display_handle),
            raw_window_handle: RawWindowHandle::AndroidNdk(window_handle),
        };

        let surface = unsafe {
            instance
                .create_surface_unsafe(surface_target)
                .map_err(|e| BlincError::GpuInit(e.to_string()))?
        };

        // Create renderer
        let renderer = pollster::block_on(async {
            GpuRenderer::with_instance_and_surface(instance, &surface, renderer_config).await
        })
        .map_err(|e| BlincError::GpuInit(e.to_string()))?;

        let device = renderer.device_arc();
        let queue = renderer.queue_arc();

        let mut text_ctx = TextRenderingContext::new(device.clone(), queue.clone());

        // Load Android system fonts
        let mut fonts_loaded = 0;
        for font_path in crate::system_font_paths() {
            let path = std::path::Path::new(font_path);
            tracing::debug!("Checking font path: {}", font_path);
            if path.exists() {
                match std::fs::read(path) {
                    Ok(data) => {
                        tracing::info!("Loading font from: {} ({} bytes)", font_path, data.len());
                        match text_ctx.load_font_data(data) {
                            Ok(_) => {
                                tracing::info!("Successfully loaded font: {}", font_path);
                                fonts_loaded += 1;
                            }
                            Err(e) => {
                                tracing::warn!("Failed to load font {}: {:?}", font_path, e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to read font file {}: {}", font_path, e);
                    }
                }
            } else {
                tracing::debug!("Font path does not exist: {}", font_path);
            }
        }
        tracing::info!("Loaded {} system fonts", fonts_loaded);

        // Preload common fonts
        text_ctx.preload_fonts(&["Roboto", "Noto Sans", "Droid Sans"]);
        text_ctx.preload_generic_styles(blinc_gpu::GenericFont::SansSerif, &[400, 700], false);
        tracing::info!("Font preloading complete");

        let ctx = crate::context::RenderContext::new(
            renderer,
            text_ctx,
            device,
            queue,
            config.sample_count,
        );
        let app = BlincApp::from_context(ctx, config);

        Ok((app, surface))
    }
}

// ============================================================================
// Deep link handling
// ============================================================================

/// Dispatch a deep link URI from JNI intent data.
///
/// Auto-dispatches to the router registered via `RouterBuilder::build()`.
/// No user setup required.
pub fn dispatch_deep_link(uri: &str) {
    tracing::info!("Android deep link received: {}", uri);
    blinc_router::dispatch_deep_link(uri);
}

/// Receive stream data from JNI (camera frames, audio buffers).
///
/// Called from Kotlin via JNI:
/// ```kotlin
/// external fun nativeDispatchStreamData(streamId: Long, data: ByteArray)
/// ```
pub fn dispatch_stream_data(stream_id: u64, data: &[u8]) {
    blinc_core::native_bridge::dispatch_stream_data(
        stream_id,
        blinc_core::native_bridge::NativeValue::Bytes(data.to_vec()),
    );
}

/// JNI export — receive a soft-keyboard inset update from Kotlin.
///
/// Called from `BlincNativeBridge`'s `setOnApplyWindowInsetsListener`
/// whenever `WindowInsets.Type.ime().bottom` changes (the Android
/// equivalent of `UIKeyboardWillChangeFrameNotification`).
///
/// The Kotlin side converts the raw pixel value (which Android reports
/// in physical pixels) to **logical pixels** by dividing by the display
/// density before pushing here, so the value stored in
/// `PENDING_IME_INSET_PX` is directly comparable to
/// `WindowedContext.height` and `width` (which the Android runner
/// also stores in logical pixels).
///
/// The android_main poll loop reads this atomic on every tick and
/// pushes the value into `WindowedContext.keyboard_inset` if it
/// changed. From there the layout / scroll-into-focused-input
/// machinery picks it up via the same path the iOS runner uses.
///
/// # Kotlin declaration
/// ```kotlin
/// external fun nativeDispatchKeyboardInset(insetLogicalPx: Int)
/// ```
///
/// # JNI signature
/// `(I)V`
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_blinc_BlincNativeBridge_nativeDispatchKeyboardInset(
    _env: jni::JNIEnv,
    _class: jni::objects::JClass,
    inset_logical_px: jni::sys::jint,
) {
    // Clamp anything negative (which would be nonsense from the IME
    // API but worth defending against) to zero. Sentinel `-1` is
    // reserved for "not yet pushed", so anything we accept here is
    // a real keyboard-inset update.
    let clamped = inset_logical_px.max(0);
    PENDING_IME_INSET_PX.store(clamped, Ordering::Relaxed);
}
