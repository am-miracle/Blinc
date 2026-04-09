//! Interactive window tests
//!
//! This module provides utilities for interactive testing with live windows.
//! Only available when the `interactive` feature is enabled.

use anyhow::Result;
use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowAttributes, WindowId},
};

/// State for the interactive test application
struct InteractiveTestApp<F>
where
    F: Fn(&wgpu::Device, &wgpu::Queue, &wgpu::TextureView),
{
    title: String,
    render_fn: F,
    window: Option<Arc<Window>>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    surface: Option<wgpu::Surface<'static>>,
    config: Option<wgpu::SurfaceConfiguration>,
}

impl<F> InteractiveTestApp<F>
where
    F: Fn(&wgpu::Device, &wgpu::Queue, &wgpu::TextureView),
{
    fn new(title: &str, render_fn: F) -> Self {
        Self {
            title: title.to_string(),
            render_fn,
            window: None,
            device: None,
            queue: None,
            surface: None,
            config: None,
        }
    }
}

impl<F> ApplicationHandler for InteractiveTestApp<F>
where
    F: Fn(&wgpu::Device, &wgpu::Queue, &wgpu::TextureView),
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window_attrs = WindowAttributes::default()
            .with_title(&self.title)
            .with_inner_size(winit::dpi::LogicalSize::new(800, 600));

        let window = Arc::new(
            event_loop
                .create_window(window_attrs)
                .expect("Failed to create window"),
        );

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .expect("Failed to create surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("Failed to find adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("Interactive Test Device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            },
        ))
        .expect("Failed to create device");

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("Surface not supported");
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);

        self.window = Some(window);
        self.device = Some(device);
        self.queue = Some(queue);
        self.surface = Some(surface);
        self.config = Some(config);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        event_loop.set_control_flow(ControlFlow::Wait);

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) if new_size.width > 0 && new_size.height > 0 => {
                if let (Some(surface), Some(device), Some(config)) =
                    (&self.surface, &self.device, &mut self.config)
                {
                    config.width = new_size.width;
                    config.height = new_size.height;
                    surface.configure(device, config);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if let (Some(surface), Some(device), Some(queue)) =
                    (&self.surface, &self.device, &self.queue)
                {
                    if let Ok(frame) = surface.get_current_texture() {
                        let view = frame
                            .texture
                            .create_view(&wgpu::TextureViewDescriptor::default());
                        (self.render_fn)(device, queue, &view);
                        frame.present();
                    }
                }
            }
            _ => {}
        }
    }
}

/// Run an interactive test window
pub fn run_interactive_test<F>(title: &str, render_fn: F) -> Result<()>
where
    F: Fn(&wgpu::Device, &wgpu::Queue, &wgpu::TextureView) + 'static,
{
    let event_loop = EventLoop::new()?;
    let mut app = InteractiveTestApp::new(title, render_fn);
    event_loop.run_app(&mut app)?;
    Ok(())
}
