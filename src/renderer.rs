use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use glyphon::{
    Cache, Color, ColorMode, Resolution, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::config::{Config, CursorShape};
use crate::cursor::{CursorAnimation, CursorAnimationOptions, CursorShape as AnimatedCursorShape};
use crate::geometry::GridGeometry;
use crate::graphics::{GraphicsSnapshot, ImageLayer};
use crate::image_renderer::ImageRenderer;
use crate::links::LinkTarget;
use crate::terminal::{Rgb, TerminalSnapshot};

#[path = "text_layout.rs"]
mod text_layout;

use text_layout::TextLayout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderOutcome {
    Rendered,
    Reconfigure,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct RectVertex {
    position: [f32; 2],
    color: [f32; 4],
}

pub struct Renderer {
    surface: Option<wgpu::Surface<'static>>,
    window: Option<Arc<Window>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    layout: TextLayout,
    image_renderer: ImageRenderer,
    prepared_graphics: Option<GraphicsSnapshot>,
    glyph_cache: Cache,
    text_atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
    rect_pipeline: wgpu::RenderPipeline,
    rect_buffer: wgpu::Buffer,
    rect_buffer_size: u64,
    rect_vertices: Vec<RectVertex>,
    background_vertex_count: usize,
    snapshot: Option<TerminalSnapshot>,
    config: Config,
    scale_factor: f32,
    cursor_animation: CursorAnimation,
    cursor_activity: Instant,
    cursor_hidden_at: Option<Instant>,
    last_frame: Instant,
    cursor_animating: bool,
    text_prepared: bool,
    prepared_geometry: Option<GridGeometry>,
    hover: Option<LinkTarget>,
    frame_interval: Duration,
    surface_retry: Option<Instant>,
}

impl Renderer {
    pub fn new(
        window: Arc<Window>,
        config: Config,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("wisp device"),
                ..Default::default()
            }))?;
        let mut surface_config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface is not supported by the selected GPU")?;
        if let Some(format) = surface
            .get_capabilities(&adapter)
            .formats
            .into_iter()
            .find(wgpu::TextureFormat::is_srgb)
        {
            surface_config.format = format;
        }
        surface_config.present_mode = wgpu::PresentMode::AutoVsync;
        surface_config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &surface_config);
        let mut renderer = Self::initialize(
            Some(surface),
            device,
            queue,
            surface_config,
            config,
            scale_factor,
        )?;
        if let Some(refresh) = window
            .current_monitor()
            .or_else(|| window.primary_monitor())
            .or_else(|| window.available_monitors().next())
            .and_then(|monitor| monitor.refresh_rate_millihertz())
        {
            renderer.frame_interval = Duration::from_secs_f64(1000.0 / f64::from(refresh.max(1)));
        }
        renderer.window = Some(window);
        Ok(renderer)
    }

    fn initialize(
        surface: Option<wgpu::Surface<'static>>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_config: wgpu::SurfaceConfiguration,
        config: Config,
        scale_factor: f32,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let layout = TextLayout::new(&config, scale_factor)?;
        let image_renderer = ImageRenderer::new(&device, surface_config.format);
        let glyph_cache = Cache::new(&device);
        let mut text_atlas = make_text_atlas(&device, &queue, &glyph_cache, surface_config.format);
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            &device,
            wgpu::MultisampleState::default(),
            None,
        );
        let viewport = Viewport::new(&device, &glyph_cache);
        let rect_pipeline = create_rect_pipeline(&device, surface_config.format);
        let rect_buffer_size = 4096;
        let rect_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wisp rectangle vertices"),
            size: rect_buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            surface,
            window: None,
            device,
            queue,
            size: PhysicalSize::new(surface_config.width, surface_config.height),
            surface_config,
            layout,
            image_renderer,
            prepared_graphics: None,
            glyph_cache,
            text_atlas,
            text_renderer,
            viewport,
            rect_pipeline,
            rect_buffer,
            rect_buffer_size,
            rect_vertices: Vec::new(),
            background_vertex_count: 0,
            snapshot: None,
            config,
            scale_factor,
            cursor_animation: CursorAnimation::new(),
            cursor_activity: Instant::now(),
            cursor_hidden_at: None,
            last_frame: Instant::now(),
            cursor_animating: false,
            text_prepared: false,
            prepared_geometry: None,
            hover: None,
            frame_interval: Duration::from_secs_f64(1.0 / 60.0),
            surface_retry: None,
        })
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.size = size;
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        self.text_prepared = false;
        self.prepared_graphics = None;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.surface_config);
        }
    }

    pub fn set_scale_factor(&mut self, scale_factor: f64) {
        let scale_factor = scale_factor as f32;
        if (self.scale_factor - scale_factor).abs() < f32::EPSILON {
            return;
        }
        match TextLayout::new(&self.config, scale_factor) {
            Ok(layout) => {
                self.layout = layout;
                self.scale_factor = scale_factor;
                self.reset_atlas();
                self.prepared_graphics = None;
                self.cursor_animation.hide();
                if let Some(snapshot) = self.snapshot.clone() {
                    self.set_snapshot(snapshot);
                }
            }
            Err(error) => eprintln!("font layout failed: {error}"),
        }
    }

    pub fn cell_geometry(&self) -> (u32, u32) {
        (self.layout.metrics.width, self.layout.metrics.height)
    }

    pub fn terminal_size(&self) -> (u16, u16) {
        self.geometry().terminal_size()
    }

    pub fn geometry(&self) -> GridGeometry {
        GridGeometry::new(
            (self.size.width, self.size.height),
            self.cell_geometry(),
            f64::from(self.scale_factor),
            self.config.window.margin_width,
        )
    }

    pub fn set_hover(&mut self, hover: Option<LinkTarget>) {
        self.hover = hover;
    }

    pub fn next_frame_deadline(&self) -> Option<Instant> {
        if let Some(retry) = self.surface_retry {
            return Some(retry);
        }
        if self.cursor_animating {
            return Some(self.last_frame + self.frame_interval);
        }
        let cursor = self.snapshot.as_ref()?.cursor;
        if !cursor.visible || !cursor.blinking || !self.config.cursor.blink {
            return None;
        }
        let interval = Duration::from_millis(530);
        let elapsed = self.cursor_activity.elapsed();
        let periods = elapsed.as_millis() / interval.as_millis() + 1;
        Some(self.cursor_activity + Duration::from_millis((periods * 530) as u64))
    }

    pub fn mouse_reporting(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.mouse_reporting)
    }

    pub fn set_snapshot(&mut self, snapshot: TerminalSnapshot) {
        crate::diagnostics::trace("snapshot-layout-start");
        let now = Instant::now();
        if snapshot.cursor.visible {
            if self.cursor_hidden_at.take().is_some_and(|hidden| {
                now.saturating_duration_since(hidden) > Duration::from_millis(250)
            }) {
                self.cursor_animation.hide();
            }
        } else {
            self.cursor_hidden_at.get_or_insert(now);
        }
        if self
            .snapshot
            .as_ref()
            .is_none_or(|previous| previous.cursor != snapshot.cursor)
        {
            self.cursor_activity = now;
            if !self.cursor_animating {
                self.last_frame = now;
            }
            self.cursor_animating = self.config.cursor.smooth && snapshot.cursor.visible;
        }
        let previous_revision = self.layout.revision;
        match self.layout.shape(&snapshot) {
            Ok(true) => self.reset_atlas(),
            Ok(false) => {}
            Err(error) => eprintln!("text shaping failed: {error}"),
        }
        if self.layout.revision != previous_revision {
            self.text_prepared = false;
        }
        self.snapshot = Some(snapshot);
        crate::diagnostics::trace("snapshot-layout-end");
    }

    pub fn render(&mut self) -> RenderOutcome {
        let Some(surface) = &self.surface else {
            return RenderOutcome::Rendered;
        };
        crate::diagnostics::trace("surface-acquire-start");
        let output = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Validation => return RenderOutcome::Reconfigure,
            wgpu::CurrentSurfaceTexture::Timeout => {
                crate::diagnostics::trace("surface-timeout");
                self.surface_retry = Some(Instant::now() + Duration::from_millis(100));
                self.last_frame = Instant::now();
                return RenderOutcome::Rendered;
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                crate::diagnostics::trace("surface-occluded");
                self.surface_retry = None;
                self.cursor_animating = false;
                self.last_frame = Instant::now();
                return RenderOutcome::Rendered;
            }
        };
        self.surface_retry = None;
        crate::diagnostics::trace("surface-acquire-end");
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        if let Err(error) = self.draw_frame(&view, Instant::now()) {
            eprintln!("GPU frame failed: {error}");
        }
        if let Some(window) = &self.window {
            window.pre_present_notify();
        }
        self.queue.present(output);
        crate::diagnostics::trace("frame-presented");
        RenderOutcome::Rendered
    }

    fn reset_atlas(&mut self) {
        self.text_prepared = false;
        self.text_atlas = make_text_atlas(
            &self.device,
            &self.queue,
            &self.glyph_cache,
            self.surface_config.format,
        );
        self.text_renderer = TextRenderer::new(
            &mut self.text_atlas,
            &self.device,
            wgpu::MultisampleState::default(),
            None,
        );
    }

    fn draw_frame(
        &mut self,
        view: &wgpu::TextureView,
        now: Instant,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.build_rectangles(now);
        if !self.rect_vertices.is_empty() {
            let bytes = bytemuck::cast_slice(&self.rect_vertices);
            if bytes.len() as u64 > self.rect_buffer_size {
                self.rect_buffer_size = (bytes.len() as u64).next_power_of_two();
                self.rect_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("wisp rectangle vertices"),
                    size: self.rect_buffer_size,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            self.queue.write_buffer(&self.rect_buffer, 0, bytes);
        }
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.size.width,
                height: self.size.height,
            },
        );
        let geometry = self.geometry();
        let foreground = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.foreground)
            .unwrap_or_else(|| self.config.colors.foreground.into());
        let areas = [TextArea {
            buffer: &self.layout.empty_buffer,
            left: geometry.origin_x as f32,
            top: geometry.origin_y as f32,
            scale: 1.0,
            bounds: TextBounds {
                left: geometry.origin_x as i32,
                top: geometry.origin_y as i32,
                right: (geometry.origin_x + geometry.content_width) as i32,
                bottom: (geometry.origin_y + geometry.content_height) as i32,
            },
            default_color: Color::rgb(foreground.red, foreground.green, foreground.blue),
            custom_glyphs: &self.layout.glyphs,
        }];
        let images = &self.layout.images;
        if !self.text_prepared || self.prepared_geometry != Some(geometry) {
            self.text_atlas.trim();
            self.text_renderer.prepare_with_custom(
                &self.device,
                &self.queue,
                &mut self.layout.font_system,
                &mut self.text_atlas,
                &self.viewport,
                areas,
                &mut self.layout.swash_cache,
                |request| images.get(&request.id).cloned(),
            )?;
            self.text_prepared = true;
            self.prepared_geometry = Some(geometry);
        }
        if let Some(snapshot) = &self.snapshot {
            let changed = self.prepared_graphics.as_ref().is_none_or(|previous| {
                previous.generation != snapshot.graphics.generation
                    || !Arc::ptr_eq(&previous.placements, &snapshot.graphics.placements)
            });
            if changed {
                self.image_renderer.prepare(
                    &self.device,
                    &self.queue,
                    &snapshot.graphics,
                    [self.size.width, self.size.height],
                    [geometry.cell_width, geometry.cell_height],
                    [
                        geometry.origin_x as f32,
                        geometry.origin_y as f32,
                        geometry.content_width as f32,
                        geometry.content_height as f32,
                    ],
                )?;
                self.prepared_graphics = Some(snapshot.graphics.clone());
            }
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wisp frame encoder"),
            });
        let background = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.background)
            .unwrap_or_else(|| self.config.colors.background.into());
        let clear = output_color(background, 1.0, self.surface_config.format.is_srgb());
        let color_attachments = [Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: f64::from(clear[0]),
                    g: f64::from(clear[1]),
                    b: f64::from(clear[2]),
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wisp frame pass"),
            color_attachments: &color_attachments,
            ..Default::default()
        });
        self.image_renderer
            .draw(ImageLayer::BelowBackground, &mut pass);
        if self.background_vertex_count > 0 {
            pass.set_pipeline(&self.rect_pipeline);
            pass.set_vertex_buffer(0, self.rect_buffer.slice(..));
            pass.draw(0..self.background_vertex_count as u32, 0..1);
        }
        self.image_renderer.draw(ImageLayer::BelowText, &mut pass);
        self.text_renderer
            .render(&self.text_atlas, &self.viewport, &mut pass)?;
        self.image_renderer.draw(ImageLayer::AboveText, &mut pass);
        if self.background_vertex_count < self.rect_vertices.len() {
            pass.set_pipeline(&self.rect_pipeline);
            pass.set_vertex_buffer(0, self.rect_buffer.slice(..));
            pass.draw(
                self.background_vertex_count as u32..self.rect_vertices.len() as u32,
                0..1,
            );
        }
        drop(pass);
        self.queue.submit(Some(encoder.finish()));
        Ok(())
    }

    fn build_rectangles(&mut self, now: Instant) {
        self.rect_vertices.clear();
        self.background_vertex_count = 0;
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        let (cell_width, cell_height) = self.cell_geometry();
        let geometry = self.geometry();
        let origin = [geometry.origin_x as f32, geometry.origin_y as f32];
        for row in 0..snapshot.rows {
            for column in 0..snapshot.cols {
                let index = usize::from(row) * usize::from(snapshot.cols) + usize::from(column);
                let Some(cell) = snapshot.cells.get(index) else {
                    continue;
                };
                let background = if cell.selected {
                    Some(self.config.colors.selection_background.into())
                } else {
                    cell.background
                };
                if let Some(color) = background {
                    append_rect(
                        &mut self.rect_vertices,
                        [
                            origin[0] + f32::from(column) * cell_width as f32,
                            origin[1] + f32::from(row) * cell_height as f32,
                            cell_width as f32,
                            cell_height as f32,
                        ],
                        color,
                        1.0,
                        self.size,
                        self.surface_config.format.is_srgb(),
                    );
                }
                if cell.underline {
                    let color = if cell.selected {
                        self.config.colors.selection_foreground.into()
                    } else {
                        cell.foreground
                    };
                    append_rect(
                        &mut self.rect_vertices,
                        [
                            origin[0] + f32::from(column) * cell_width as f32,
                            origin[1]
                                + f32::from(row) * cell_height as f32
                                + self.layout.metrics.baseline
                                + 1.0,
                            cell_width as f32,
                            1.0,
                        ],
                        color,
                        1.0,
                        self.size,
                        self.surface_config.format.is_srgb(),
                    );
                }
            }
        }
        if let Some(hover) = &self.hover {
            for span in &hover.spans {
                let left = geometry.origin_x + u32::from(span.start) * cell_width;
                let right = (geometry.origin_x + u32::from(span.end) * cell_width)
                    .min(geometry.origin_x + geometry.content_width);
                let top = geometry.origin_y
                    + u32::from(span.row) * cell_height
                    + self.layout.metrics.baseline as u32
                    + 1;
                for x in left..right {
                    let phase = (x - left) % 4;
                    let y = top + u32::from(phase == 1 || phase == 2);
                    append_rect(
                        &mut self.rect_vertices,
                        [x as f32, y as f32, 1.0, 1.0],
                        Rgb {
                            red: 0,
                            green: 135,
                            blue: 189,
                        },
                        1.0,
                        self.size,
                        self.surface_config.format.is_srgb(),
                    );
                }
            }
        }
        self.background_vertex_count = self.rect_vertices.len();
        let elapsed = now.saturating_duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        if !snapshot.cursor.visible {
            self.cursor_animating = false;
            return;
        }
        let shape = match snapshot.cursor.shape {
            CursorShape::Block => AnimatedCursorShape::Block,
            CursorShape::Beam => AnimatedCursorShape::Beam,
            CursorShape::Underline => AnimatedCursorShape::Underline,
        };
        let frame = self.cursor_animation.update(
            [
                origin[0] + f32::from(snapshot.cursor.column) * cell_width as f32,
                origin[1] + f32::from(snapshot.cursor.row) * cell_height as f32,
                cell_width as f32,
                cell_height as f32,
            ],
            shape,
            elapsed,
            CursorAnimationOptions {
                smooth: self.config.cursor.smooth,
                trail: self.config.cursor.trail,
                duration_seconds: self.config.cursor.trail_duration_ms as f32 / 1000.0,
                ..Default::default()
            },
        );
        self.cursor_animating = frame.animating;
        if frame.animating {
            crate::diagnostics::trace("cursor-animating");
        }
        let blink_visible = !snapshot.cursor.blinking
            || !self.config.cursor.blink
            || (now
                .saturating_duration_since(self.cursor_activity)
                .as_millis()
                / 530)
                .is_multiple_of(2);
        if blink_visible {
            append_quad(
                &mut self.rect_vertices,
                frame.corners,
                snapshot.cursor_color,
                1.0,
                self.size,
                self.surface_config.format.is_srgb(),
            );
        }
    }
}

fn make_text_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cache: &Cache,
    format: wgpu::TextureFormat,
) -> TextAtlas {
    TextAtlas::with_color_mode(
        device,
        queue,
        cache,
        format,
        if format.is_srgb() {
            ColorMode::Accurate
        } else {
            ColorMode::Web
        },
    )
}

fn output_color(color: Rgb, alpha: f32, srgb_surface: bool) -> [f32; 4] {
    let channel = |value: u8| {
        let value = f32::from(value) / 255.0;
        if !srgb_surface {
            value
        } else if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    [
        channel(color.red),
        channel(color.green),
        channel(color.blue),
        alpha,
    ]
}

fn append_rect(
    vertices: &mut Vec<RectVertex>,
    rect: [f32; 4],
    color: Rgb,
    alpha: f32,
    size: PhysicalSize<u32>,
    srgb: bool,
) {
    let [left, top, width, height] = rect;
    append_quad(
        vertices,
        [
            [left, top],
            [left + width, top],
            [left + width, top + height],
            [left, top + height],
        ],
        color,
        alpha,
        size,
        srgb,
    );
}

fn append_quad(
    vertices: &mut Vec<RectVertex>,
    corners: [[f32; 2]; 4],
    color: Rgb,
    alpha: f32,
    size: PhysicalSize<u32>,
    srgb: bool,
) {
    let color = output_color(color, alpha, srgb);
    for index in [0, 1, 2, 0, 2, 3] {
        vertices.push(RectVertex {
            position: [
                corners[index][0] / size.width as f32 * 2.0 - 1.0,
                1.0 - corners[index][1] / size.height as f32 * 2.0,
            ],
            color,
        });
    }
}

fn create_rect_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("wisp rectangle shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(
            "struct VertexOutput { @builtin(position) position: vec4<f32>, @location(0) color: vec4<f32> }\n@vertex fn vs_main(@location(0) position: vec2<f32>, @location(1) color: vec4<f32>) -> VertexOutput { var output: VertexOutput; output.position = vec4<f32>(position, 0.0, 1.0); output.color = color; return output; }\n@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> { return input.color; }",
        )),
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("wisp rectangle pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<RectVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{CellSnapshot, CursorSnapshot};
    use std::time::Duration;

    fn offscreen(config: Config) -> Renderer {
        offscreen_with_format(config, wgpu::TextureFormat::Rgba8UnormSrgb)
    }

    fn offscreen_with_format(mut config: Config, format: wgpu::TextureFormat) -> Renderer {
        config.window.margin_width = crate::config::Margins::uniform(0.0);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("GPU adapter required for rendering regression tests");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("GPU device");
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: 512,
            height: 128,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        Renderer::initialize(None, device, queue, surface_config, config, 1.0)
            .expect("offscreen renderer")
    }

    fn capture(renderer: &mut Renderer) -> Vec<u8> {
        let texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wisp regression target"),
            size: wgpu::Extent3d {
                width: renderer.size.width,
                height: renderer.size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: renderer.surface_config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        renderer
            .draw_frame(
                &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                Instant::now(),
            )
            .expect("draw production frame");
        let stride = (renderer.size.width * 4).div_ceil(256) * 256;
        let output = renderer.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wisp regression readback"),
            size: u64::from(stride) * u64::from(renderer.size.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = renderer
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &output,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        renderer.queue.submit(Some(encoder.finish()));
        let (sender, receiver) = std::sync::mpsc::channel();
        output.map_async(wgpu::MapMode::Read, .., move |result| {
            sender.send(result).expect("map signal")
        });
        renderer
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(10)),
            })
            .expect("GPU readback completion");
        receiver
            .recv()
            .expect("GPU readback signal")
            .expect("GPU readback map");
        let mapped = output.get_mapped_range(..).expect("mapped readback range");
        let mut rgba = Vec::new();
        for row in mapped.chunks_exact(stride as usize) {
            rgba.extend_from_slice(&row[..renderer.size.width as usize * 4]);
        }
        drop(mapped);
        output.unmap();
        rgba
    }

    fn snapshot(config: &Config, cols: u16, rows: u16) -> TerminalSnapshot {
        TerminalSnapshot {
            cols,
            rows,
            cells: vec![
                CellSnapshot {
                    foreground: config.colors.foreground.into(),
                    ..Default::default()
                };
                usize::from(cols) * usize::from(rows)
            ],
            background: config.colors.background.into(),
            foreground: config.colors.foreground.into(),
            cursor_color: config.colors.cursor.into(),
            cursor: CursorSnapshot {
                visible: false,
                ..Default::default()
            },
            mouse_reporting: false,
            graphics: Default::default(),
        }
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = (y * width + x) as usize * 4;
        rgba[offset..offset + 4].try_into().expect("RGBA pixel")
    }

    #[test]
    fn gpu_readback_preserves_config_background_palette_and_selection() {
        let mut config = Config::default();
        config.colors.background = crate::config::Color::new(40, 44, 52);
        config.colors.selection_background = crate::config::Color::new(123, 75, 141);
        let mut renderer = offscreen(config.clone());
        let mut snapshot = snapshot(&config, 18, 1);
        for (index, color) in config.colors.ansi_palette().iter().enumerate() {
            snapshot.cells[index].background = Some((*color).into());
        }
        snapshot.cells[16].selected = true;
        renderer.set_snapshot(snapshot);
        let rgba = capture(&mut renderer);
        let (cell_width, cell_height) = renderer.cell_geometry();
        let (origin_x, origin_y) = renderer.geometry().content_origin();
        for (index, color) in config.colors.ansi_palette().iter().enumerate() {
            assert_eq!(
                pixel(
                    &rgba,
                    renderer.size.width,
                    origin_x + index as u32 * cell_width + cell_width / 2,
                    origin_y + cell_height / 2
                ),
                [color.red, color.green, color.blue, 255]
            );
        }
        assert_eq!(
            pixel(
                &rgba,
                renderer.size.width,
                origin_x + cell_width * 16 + cell_width / 2,
                origin_y + cell_height / 2
            ),
            [123, 75, 141, 255]
        );
        assert_eq!(
            pixel(&rgba, renderer.size.width, 500, 100),
            [40, 44, 52, 255]
        );
    }

    #[test]
    fn gpu_glyph_colors_preserve_palette_selection_and_truecolor() {
        for format in [
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Rgba8Unorm,
        ] {
            let mut config = Config::default();
            config.font.family = "DejaVu Sans Mono".to_owned();
            config.colors.background = crate::config::Color::new(40, 44, 52);
            config.colors.selection_foreground = crate::config::Color::new(82, 161, 213);
            let mut renderer = offscreen_with_format(config.clone(), format);
            let mut snapshot = snapshot(&config, 18, 1);
            let mut expected = config
                .colors
                .ansi_palette()
                .into_iter()
                .map(Rgb::from)
                .collect::<Vec<_>>();
            expected.push(config.colors.selection_foreground.into());
            expected.push(Rgb {
                red: 128,
                green: 96,
                blue: 64,
            });
            for (index, color) in expected.iter().copied().enumerate() {
                snapshot.cells[index].text = "█".to_owned();
                snapshot.cells[index].foreground = color;
            }
            snapshot.cells[16].foreground = Rgb {
                red: 255,
                green: 0,
                blue: 255,
            };
            snapshot.cells[16].selected = true;
            renderer.set_snapshot(snapshot);
            let rgba = capture(&mut renderer);
            let (cell_width, cell_height) = renderer.cell_geometry();
            let (origin_x, origin_y) = renderer.geometry().content_origin();
            for (index, color) in expected.iter().enumerate() {
                let expected_pixel = [color.red, color.green, color.blue, 255];
                let found = (0..cell_height).any(|row| {
                    (0..cell_width).any(|offset| {
                        pixel(
                            &rgba,
                            renderer.size.width,
                            origin_x + index as u32 * cell_width + offset,
                            origin_y + row,
                        ) == expected_pixel
                    })
                });
                assert!(
                    found,
                    "glyph color at column {index} on {format:?} must include an opaque {expected_pixel:?} pixel"
                );
            }
        }
    }

    #[test]
    fn gpu_glyphs_and_cursor_share_grid_after_wide_and_combining_text() {
        let mut config = Config::default();
        config.font.family = "FiraCode Nerd Font".to_owned();
        config.cursor.blink = false;
        config.cursor.smooth = false;
        config.cursor.trail = false;
        let mut renderer = offscreen(config.clone());
        let mut snapshot = snapshot(&config, 24, 1);
        for (column, text, width) in [
            (0, "M", 1),
            (1, "漢", 2),
            (3, "e\u{301}", 1),
            (4, "=", 1),
            (5, ">", 1),
            (6, "M", 1),
            (20, "M", 1),
        ] {
            snapshot.cells[column].text = text.to_owned();
            snapshot.cells[column].width = width;
        }
        snapshot.cells[2].width = 0;
        snapshot.cursor = CursorSnapshot {
            column: 21,
            row: 0,
            visible: true,
            blinking: false,
            shape: CursorShape::Beam,
        };
        renderer.set_snapshot(snapshot);
        let rgba = capture(&mut renderer);
        let (cell_width, cell_height) = renderer.cell_geometry();
        let (origin_x, origin_y) = renderer.geometry().content_origin();
        let first = (0..cell_height)
            .flat_map(|row| (0..cell_width).map(move |column| (row, column)))
            .map(|(row, column)| {
                pixel(
                    &rgba,
                    renderer.size.width,
                    origin_x + column,
                    origin_y + row,
                )
            })
            .collect::<Vec<_>>();
        for column in [6, 20] {
            let other = (0..cell_height)
                .flat_map(|row| (0..cell_width).map(move |offset| (row, offset)))
                .map(|(row, offset)| {
                    pixel(
                        &rgba,
                        renderer.size.width,
                        origin_x + column * cell_width + offset,
                        origin_y + row,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                first, other,
                "M at column {column} must stay on the exact cell grid"
            );
        }
        assert!(
            first.iter().any(|pixel| pixel[0] > 128),
            "configured font must rasterize visible glyphs"
        );
        assert_eq!(
            pixel(
                &rgba,
                renderer.size.width,
                origin_x + 21 * cell_width,
                origin_y + cell_height / 2
            ),
            [255, 255, 255, 255]
        );
    }

    #[test]
    fn gpu_margin_translates_text_cursor_and_image_on_the_same_grid() {
        let mut config = Config::default();
        config.font.family = "FiraCode Nerd Font".to_owned();
        config.cursor.blink = false;
        config.cursor.smooth = false;
        config.cursor.trail = false;
        let mut renderer = offscreen(config.clone());
        renderer.config.window.margin_width = crate::config::Margins::default();
        renderer.resize(PhysicalSize::new(512, 256));
        let geometry = renderer.geometry();
        assert!(geometry.origin_x >= 29 && geometry.origin_y >= 29);
        let mut snapshot = snapshot(&config, geometry.cols, geometry.rows);
        snapshot.cells[0].text = "M".to_owned();
        snapshot.cells[0].background = Some(Rgb {
            red: 19,
            green: 37,
            blue: 61,
        });
        snapshot.cursor = CursorSnapshot {
            column: 1,
            row: 0,
            visible: true,
            blinking: false,
            shape: CursorShape::Beam,
        };
        snapshot.graphics = GraphicsSnapshot {
            generation: 1,
            placements: vec![crate::graphics::ImagePlacement {
                image: Arc::new(crate::graphics::ImagePixels {
                    generation: 1,
                    width: 1,
                    height: 1,
                    rgba: Arc::from([211, 37, 89, 255]),
                }),
                image_id: 1,
                placement_id: 1,
                z: 1,
                grid_position: [3, 0],
                offset: [0, 0],
                pixel_size: [8, 8],
                source: [0.0, 0.0, 1.0, 1.0],
            }]
            .into(),
        };
        renderer.set_snapshot(snapshot);
        let rgba = capture(&mut renderer);
        assert_eq!(pixel(&rgba, 512, 10, 10), [0, 0, 0, 255]);
        assert_eq!(
            pixel(&rgba, 512, geometry.origin_x, geometry.origin_y),
            [19, 37, 61, 255]
        );
        let (cursor_x, cursor_y) = geometry.cell_origin(1, 0);
        assert_eq!(
            pixel(&rgba, 512, cursor_x, cursor_y + geometry.cell_height / 2),
            [255, 255, 255, 255]
        );
        let (image_x, image_y) = geometry.cell_origin(3, 0);
        assert_eq!(
            pixel(&rgba, 512, image_x + 4, image_y + 4),
            [211, 37, 89, 255]
        );
        assert_eq!(renderer.image_renderer.texture_uploads(), 1);
        renderer.resize(PhysicalSize::new(640, 256));
        let geometry = renderer.geometry();
        let rgba = capture(&mut renderer);
        let (image_x, image_y) = geometry.cell_origin(3, 0);
        assert_eq!(
            pixel(&rgba, 640, image_x + 4, image_y + 4),
            [211, 37, 89, 255]
        );
        assert_eq!(renderer.image_renderer.texture_uploads(), 1);
    }

    #[test]
    fn cursor_only_updates_do_not_reshape_text_and_idle_has_no_frame_deadline() {
        let mut config = Config::default();
        config.cursor.blink = false;
        config.cursor.smooth = false;
        let mut renderer = offscreen(config.clone());
        let mut snapshot = snapshot(&config, 20, 1);
        snapshot.cells[0].text = "M".to_owned();
        renderer.set_snapshot(snapshot.clone());
        capture(&mut renderer);
        let revision = renderer.layout.revision;
        snapshot.cursor.column = 5;
        renderer.set_snapshot(snapshot);
        capture(&mut renderer);
        assert_eq!(renderer.layout.revision, revision);
        assert_eq!(renderer.layout.shaped_rows, 0);
        assert!(renderer.next_frame_deadline().is_none());
        assert!(renderer.text_prepared);
    }

    #[test]
    fn cursor_movement_after_idle_does_not_advance_through_old_idle_time() {
        let mut config = Config::default();
        config.cursor.blink = false;
        let mut renderer = offscreen(config.clone());
        let mut snapshot = snapshot(&config, 20, 1);
        snapshot.cursor = CursorSnapshot {
            visible: true,
            shape: CursorShape::Beam,
            ..Default::default()
        };
        renderer.set_snapshot(snapshot.clone());
        renderer.build_rectangles(Instant::now());
        renderer.last_frame = Instant::now() - Duration::from_secs(10);
        snapshot.cursor.column = 1;
        renderer.set_snapshot(snapshot);
        renderer.build_rectangles(Instant::now());
        assert!(
            renderer.cursor_animating,
            "new movement must not consume prior idle time"
        );
    }

    #[test]
    fn ncurses_temporary_hide_does_not_cancel_mouse_cursor_animation() {
        let mut config = Config::default();
        config.cursor.blink = false;
        let mut renderer = offscreen(config.clone());
        let mut snapshot = snapshot(&config, 40, 4);
        snapshot.cursor = CursorSnapshot {
            visible: true,
            shape: CursorShape::Beam,
            ..Default::default()
        };
        renderer.set_snapshot(snapshot.clone());
        renderer.build_rectangles(Instant::now());
        snapshot.cursor.visible = false;
        renderer.set_snapshot(snapshot.clone());
        renderer.build_rectangles(Instant::now());
        snapshot.cursor.visible = true;
        snapshot.cursor.column = 13;
        snapshot.cursor.row = 2;
        renderer.set_snapshot(snapshot);
        renderer.build_rectangles(Instant::now());
        assert!(
            renderer.cursor_animating,
            "nano redraw hide/show must preserve the previous cursor corners"
        );
    }
}
