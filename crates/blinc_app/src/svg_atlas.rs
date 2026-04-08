//! SVG texture atlas for batched GPU rendering
//!
//! Packs all rasterized SVG icons into a single shared RGBA texture using
//! shelf-packing (skyline algorithm). Eliminates per-icon GPU textures and
//! enables single-draw-call rendering for all SVG instances in a frame.

use std::collections::HashMap;

/// Region allocated in the SVG atlas
#[derive(Debug, Clone, Copy)]
pub struct SvgAtlasRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl SvgAtlasRegion {
    /// Returns [u_min, v_min, u_max, v_max] matching GpuImageInstance.src_uv format
    pub fn uv_bounds(&self, atlas_w: u32, atlas_h: u32) -> [f32; 4] {
        let u_min = self.x as f32 / atlas_w as f32;
        let v_min = self.y as f32 / atlas_h as f32;
        let u_max = (self.x + self.width) as f32 / atlas_w as f32;
        let v_max = (self.y + self.height) as f32 / atlas_h as f32;
        [u_min, v_min, u_max, v_max]
    }
}

/// A shelf in the skyline packing algorithm
#[derive(Debug)]
struct Shelf {
    y: u32,
    height: u32,
    x: u32,
}

const INITIAL_SIZE: u32 = 1024;
const MAX_SIZE: u32 = 4096;
const PADDING: u32 = 2;

/// SVG texture atlas — packs rasterized SVGs into a single GPU texture
pub struct SvgAtlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    shelves: Vec<Shelf>,
    entries: HashMap<u64, SvgAtlasRegion>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    dirty: bool,
}

impl SvgAtlas {
    pub fn new(device: &wgpu::Device) -> Self {
        let (texture, view) = create_atlas_texture(device, INITIAL_SIZE, INITIAL_SIZE);
        Self {
            width: INITIAL_SIZE,
            height: INITIAL_SIZE,
            pixels: vec![0u8; (INITIAL_SIZE * INITIAL_SIZE * 4) as usize],
            shelves: Vec::new(),
            entries: HashMap::new(),
            texture,
            view,
            dirty: false,
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Look up an existing entry by cache key
    pub fn get(&self, cache_key: u64) -> Option<&SvgAtlasRegion> {
        self.entries.get(&cache_key)
    }

    /// Allocate space, write pixels, and insert a cache entry. Returns the region.
    ///
    /// Returns `None` if the atlas is full at the maximum size — callers
    /// should either fall back to a per-icon texture or simply skip the
    /// element for this frame. We deliberately do *not* `clear()` and
    /// retry on overflow at MAX_SIZE: that turns the atlas into a
    /// thrashing cache where every overflowing frame wipes valid entries
    /// and re-rasterizes them on the next frame, churning the GPU and
    /// keeping the texture pinned at the maximum 4096×4096 footprint
    /// (~67 MB CPU + ~67 MB GPU) forever. If repeated overflow happens,
    /// the warning surfaces it instead of silently degrading.
    pub fn insert(
        &mut self,
        cache_key: u64,
        width: u32,
        height: u32,
        rgba_pixels: &[u8],
        device: &wgpu::Device,
    ) -> Option<SvgAtlasRegion> {
        // Try to allocate
        let region = match self.allocate(width, height) {
            Some(r) => r,
            None => {
                // Try growing — succeeds until we hit MAX_SIZE
                if self.grow(device) {
                    self.allocate(width, height)?
                } else {
                    tracing::warn!(
                        "SVG atlas at max size {}x{} could not fit {}x{} ({} entries, {:.1}% used) — skipping insertion",
                        self.width,
                        self.height,
                        width,
                        height,
                        self.entries.len(),
                        self.utilization() * 100.0,
                    );
                    return None;
                }
            }
        };

        self.write_pixels(&region, rgba_pixels);
        self.entries.insert(cache_key, region);
        Some(region)
    }

    /// Upload dirty pixels to the GPU texture
    pub fn upload(&mut self, queue: &wgpu::Queue) {
        if !self.dirty {
            return;
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * 4),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.dirty = false;
    }

    /// Clear all entries and shelves (full eviction)
    pub fn clear(&mut self) {
        self.entries.clear();
        self.shelves.clear();
        self.pixels.fill(0);
        self.dirty = true;
    }

    /// Calculate atlas utilization (0.0 to 1.0)
    pub fn utilization(&self) -> f32 {
        let used_height = self.shelves.last().map(|s| s.y + s.height).unwrap_or(0);
        used_height as f32 / self.height as f32
    }

    /// Allocate a region using shelf packing
    fn allocate(&mut self, width: u32, height: u32) -> Option<SvgAtlasRegion> {
        let padded_w = width + PADDING;
        let padded_h = height + PADDING;

        // Find best shelf (smallest height that fits, lowest Y)
        let mut best_shelf: Option<usize> = None;
        let mut best_y = u32::MAX;

        for (i, shelf) in self.shelves.iter().enumerate() {
            if shelf.height >= padded_h && shelf.x + padded_w <= self.width && shelf.y < best_y {
                best_y = shelf.y;
                best_shelf = Some(i);
            }
        }

        if let Some(idx) = best_shelf {
            let shelf = &mut self.shelves[idx];
            let region = SvgAtlasRegion {
                x: shelf.x,
                y: shelf.y,
                width,
                height,
            };
            shelf.x += padded_w;
            return Some(region);
        }

        // Create new shelf
        let new_y = self.shelves.last().map(|s| s.y + s.height).unwrap_or(0);
        if new_y + padded_h > self.height {
            return None;
        }

        let region = SvgAtlasRegion {
            x: 0,
            y: new_y,
            width,
            height,
        };

        self.shelves.push(Shelf {
            y: new_y,
            height: padded_h,
            x: padded_w,
        });

        Some(region)
    }

    /// Blit RGBA pixel data into the atlas at the given region
    fn write_pixels(&mut self, region: &SvgAtlasRegion, rgba: &[u8]) {
        let row_bytes = region.width as usize * 4;
        for y in 0..region.height {
            let src_offset = y as usize * row_bytes;
            let dst_offset =
                ((region.y + y) as usize * self.width as usize + region.x as usize) * 4;
            if src_offset + row_bytes <= rgba.len() && dst_offset + row_bytes <= self.pixels.len() {
                self.pixels[dst_offset..dst_offset + row_bytes]
                    .copy_from_slice(&rgba[src_offset..src_offset + row_bytes]);
            }
        }
        self.dirty = true;
    }

    /// Double atlas dimensions, copy old pixels into top-left quadrant.
    /// Creates a new GPU texture. Returns false if already at max size.
    fn grow(&mut self, device: &wgpu::Device) -> bool {
        let new_w = (self.width * 2).min(MAX_SIZE);
        let new_h = (self.height * 2).min(MAX_SIZE);

        if new_w == self.width && new_h == self.height {
            return false;
        }

        let mut new_pixels = vec![0u8; (new_w * new_h * 4) as usize];
        for y in 0..self.height {
            let src_start = (y * self.width * 4) as usize;
            let src_end = src_start + (self.width * 4) as usize;
            let dst_start = (y * new_w * 4) as usize;
            let dst_end = dst_start + (self.width * 4) as usize;
            new_pixels[dst_start..dst_end].copy_from_slice(&self.pixels[src_start..src_end]);
        }

        self.pixels = new_pixels;
        self.width = new_w;
        self.height = new_h;

        let (texture, view) = create_atlas_texture(device, new_w, new_h);
        self.texture = texture;
        self.view = view;
        self.dirty = true;

        tracing::info!(
            "SVG atlas grew to {}x{} ({:.1} MB)",
            new_w,
            new_h,
            (new_w as f64 * new_h as f64 * 4.0) / (1024.0 * 1024.0)
        );
        true
    }
}

fn create_atlas_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("SVG Atlas"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}
