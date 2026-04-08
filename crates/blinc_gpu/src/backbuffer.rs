//! Backbuffer management for double/triple buffering
//!
//! This module provides backbuffer support for:
//! - Glass/vibrancy effects (need to read previous frame)
//! - WASM/WebGL targets where swapchain access may be limited
//! - Post-processing effects that need to sample the rendered scene
//! - Screenshot/capture functionality

// Backbuffer management for double/triple buffering

/// Configuration for backbuffer
#[derive(Clone, Debug)]
pub struct BackbufferConfig {
    /// Number of buffers (1 = single, 2 = double, 3 = triple)
    pub buffer_count: u32,
    /// Whether to include a depth buffer
    pub depth_buffer: bool,
    /// Texture format for color buffers
    pub format: wgpu::TextureFormat,
    /// Enable MSAA (sample count)
    pub sample_count: u32,
}

impl Default for BackbufferConfig {
    fn default() -> Self {
        Self {
            buffer_count: 2, // Double buffering by default
            depth_buffer: false,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            sample_count: 1,
        }
    }
}

/// A single buffer in the backbuffer chain
struct Buffer {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// Manages a chain of backbuffers for rendering
///
/// This is essential for:
/// - Glass effects that need to sample the backdrop
/// - WASM targets where we can't read from the swapchain
/// - Post-processing pipelines
pub struct Backbuffer {
    /// The backbuffer textures
    buffers: Vec<Buffer>,
    /// Depth buffer (optional)
    depth: Option<Buffer>,
    /// Current write buffer index
    write_index: usize,
    /// Current read buffer index (for glass effects)
    read_index: usize,
    /// Buffer dimensions
    width: u32,
    height: u32,
    /// Configuration
    config: BackbufferConfig,
    /// Sampler for reading backbuffers
    sampler: wgpu::Sampler,
}

impl Backbuffer {
    /// Create a new backbuffer chain
    pub fn new(device: &wgpu::Device, width: u32, height: u32, config: BackbufferConfig) -> Self {
        let buffers = Self::create_buffers(device, width, height, &config);
        let depth = if config.depth_buffer {
            Some(Self::create_depth_buffer(
                device,
                width,
                height,
                config.sample_count,
            ))
        } else {
            None
        };

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Backbuffer Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        Self {
            buffers,
            depth,
            write_index: 0,
            read_index: 1.min(config.buffer_count as usize - 1),
            width,
            height,
            config,
            sampler,
        }
    }

    fn create_buffers(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        config: &BackbufferConfig,
    ) -> Vec<Buffer> {
        (0..config.buffer_count)
            .map(|i| {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("Backbuffer {}", i)),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: config.sample_count,
                    dimension: wgpu::TextureDimension::D2,
                    format: config.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });

                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

                Buffer { texture, view }
            })
            .collect()
    }

    fn create_depth_buffer(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        sample_count: u32,
    ) -> Buffer {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Backbuffer Depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        Buffer { texture, view }
    }

    /// Resize the backbuffers
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }

        self.width = width;
        self.height = height;
        self.buffers = Self::create_buffers(device, width, height, &self.config);

        if self.config.depth_buffer {
            self.depth = Some(Self::create_depth_buffer(
                device,
                width,
                height,
                self.config.sample_count,
            ));
        }
    }

    /// Get the current write target (where we render to)
    pub fn write_target(&self) -> &wgpu::TextureView {
        &self.buffers[self.write_index].view
    }

    /// Get the current read target (previous frame, for glass effects)
    pub fn read_target(&self) -> &wgpu::TextureView {
        &self.buffers[self.read_index].view
    }

    /// Get the depth buffer view (if enabled)
    pub fn depth_target(&self) -> Option<&wgpu::TextureView> {
        self.depth.as_ref().map(|d| &d.view)
    }

    /// Get the sampler for reading backbuffers
    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    /// Get the texture format
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Get the current dimensions
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Swap buffers - call after rendering a frame
    ///
    /// This advances the write/read indices for the next frame
    pub fn swap(&mut self) {
        let buffer_count = self.buffers.len();
        self.read_index = self.write_index;
        self.write_index = (self.write_index + 1) % buffer_count;
    }

    /// Copy the current write buffer to the swapchain
    ///
    /// This is used to present the final rendered frame to the screen.
    /// Essential for WASM where we render to a backbuffer first.
    pub fn copy_to_surface(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        surface_texture: &wgpu::Texture,
    ) {
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.buffers[self.write_index].texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: surface_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Get the write buffer texture (for advanced use cases)
    pub fn write_texture(&self) -> &wgpu::Texture {
        &self.buffers[self.write_index].texture
    }

    /// Get the read buffer texture (for advanced use cases)
    pub fn read_texture(&self) -> &wgpu::Texture {
        &self.buffers[self.read_index].texture
    }
}

/// Frame context that manages backbuffer and surface rendering
///
/// This provides a high-level API for frame rendering that works
/// across all platforms including WASM.
pub struct FrameContext<'a> {
    /// The backbuffer being rendered to
    pub backbuffer: &'a mut Backbuffer,
    /// Optional surface texture (None for headless/offscreen)
    pub surface_texture: Option<wgpu::SurfaceTexture>,
    /// Device reference for creating resources
    device: &'a wgpu::Device,
    /// Queue reference for submitting commands
    queue: &'a wgpu::Queue,
}

impl<'a> FrameContext<'a> {
    /// Begin a new frame
    ///
    /// If a surface is provided, this acquires the next swapchain texture.
    /// The frame will be rendered to the backbuffer and then copied to the surface.
    pub fn begin(
        backbuffer: &'a mut Backbuffer,
        surface: Option<&wgpu::Surface>,
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    ) -> Result<Self, wgpu::SurfaceError> {
        let surface_texture = if let Some(surface) = surface {
            Some(surface.get_current_texture()?)
        } else {
            None
        };

        Ok(Self {
            backbuffer,
            surface_texture,
            device,
            queue,
        })
    }

    /// Get the target view for rendering
    pub fn target(&self) -> &wgpu::TextureView {
        self.backbuffer.write_target()
    }

    /// Get the backdrop view (previous frame) for glass effects
    pub fn backdrop(&self) -> &wgpu::TextureView {
        self.backbuffer.read_target()
    }

    /// Get the depth view (if depth buffer is enabled)
    pub fn depth(&self) -> Option<&wgpu::TextureView> {
        self.backbuffer.depth_target()
    }

    /// Get the sampler for sampling the backdrop
    pub fn sampler(&self) -> &wgpu::Sampler {
        self.backbuffer.sampler()
    }

    /// Present the frame to the surface (if any) and swap buffers
    pub fn present(self) {
        if let Some(surface_texture) = self.surface_texture {
            // Create an encoder to copy backbuffer to surface
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Backbuffer Copy Encoder"),
                });

            self.backbuffer
                .copy_to_surface(&mut encoder, &surface_texture.texture);

            self.queue.submit(std::iter::once(encoder.finish()));
            surface_texture.present();
        }

        // Swap the backbuffers for the next frame
        self.backbuffer.swap();
    }

    /// Get the device
    pub fn device(&self) -> &wgpu::Device {
        self.device
    }

    /// Get the queue
    pub fn queue(&self) -> &wgpu::Queue {
        self.queue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require a GPU and are marked as ignored by default
    // Run with: cargo test --features test-gpu -- --ignored

    #[test]
    #[ignore]
    fn test_backbuffer_creation() {
        // Would need actual wgpu device for this test
    }
}
