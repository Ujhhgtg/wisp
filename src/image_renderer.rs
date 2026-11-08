use std::collections::{HashMap, HashSet};

use bytemuck::{Pod, Zeroable};

use crate::graphics::{GraphicsSnapshot, ImageLayer};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ImageVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

type Quad = [[f32; 2]; 4];
type ClippedQuad = (Quad, Quad);

struct TextureEntry {
    bind_group: wgpu::BindGroup,
}

struct ImageDraw {
    generation: u64,
    layer: ImageLayer,
    vertices: std::ops::Range<u32>,
}

pub struct ImageRenderer {
    pipeline: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    textures: HashMap<u64, TextureEntry>,
    vertices: Vec<ImageVertex>,
    buffer: wgpu::Buffer,
    buffer_size: u64,
    draws: Vec<ImageDraw>,
    uploads: u64,
}

impl ImageRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wisp image texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wisp image pipeline layout"),
            bind_group_layouts: &[Some(&texture_layout)],
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wisp image shader"),
            source: wgpu::ShaderSource::Wgsl(IMAGE_SHADER.into()),
        });
        let targets = [Some(wgpu::ColorTargetState {
            format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ImageVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
            ],
        };
        let entry_point = if format.is_srgb() {
            "fs_linear"
        } else {
            "fs_srgb"
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wisp image pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(vertex_layout)],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wisp image sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let buffer_size = 4096;
        let buffer = vertex_buffer(device, buffer_size);
        Self {
            pipeline,
            texture_layout,
            sampler,
            textures: HashMap::new(),
            vertices: Vec::new(),
            buffer,
            buffer_size,
            draws: Vec::new(),
            uploads: 0,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snapshot: &GraphicsSnapshot,
        surface_size: [u32; 2],
        cell_size: [u32; 2],
        clip: [f32; 4],
    ) -> Result<(), String> {
        self.vertices.clear();
        self.draws.clear();
        let mut retained = HashSet::new();
        let limit = device.limits().max_texture_dimension_2d;
        for placement in snapshot.placements.iter() {
            let image = &placement.image;
            retained.insert(image.generation);
            if image.width == 0 || image.height == 0 || image.width > limit || image.height > limit
            {
                continue;
            }
            if !self.textures.contains_key(&image.generation) {
                let expected = u64::from(image.width) * u64::from(image.height) * 4;
                if image.rgba.len() as u64 != expected {
                    return Err("invalid Kitty image pixel length".to_owned());
                }
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("wisp Kitty image"),
                    size: wgpu::Extent3d {
                        width: image.width,
                        height: image.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &image.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(image.width * 4),
                        rows_per_image: Some(image.height),
                    },
                    texture.size(),
                );
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("wisp Kitty image binding"),
                    layout: &self.texture_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                });
                self.textures
                    .insert(image.generation, TextureEntry { bind_group });
                self.uploads += 1;
            }
            let destination = [
                clip[0]
                    + placement.grid_position[0] as f32 * cell_size[0] as f32
                    + placement.offset[0] as f32,
                clip[1]
                    + placement.grid_position[1] as f32 * cell_size[1] as f32
                    + placement.offset[1] as f32,
                placement.pixel_size[0] as f32,
                placement.pixel_size[1] as f32,
            ];
            let Some((position, uv)) = clipped_quad(
                destination,
                placement.source,
                [image.width, image.height],
                clip,
                surface_size,
            ) else {
                continue;
            };
            let first = self.vertices.len() as u32;
            for index in [0, 1, 2, 0, 2, 3] {
                self.vertices.push(ImageVertex {
                    position: position[index],
                    uv: uv[index],
                });
            }
            self.draws.push(ImageDraw {
                generation: image.generation,
                layer: ImageLayer::from_z(placement.z),
                vertices: first..self.vertices.len() as u32,
            });
        }
        self.textures
            .retain(|generation, _| retained.contains(generation));
        let bytes = bytemuck::cast_slice(&self.vertices);
        if bytes.len() as u64 > self.buffer_size {
            self.buffer_size = (bytes.len() as u64).next_power_of_two();
            self.buffer = vertex_buffer(device, self.buffer_size);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytes);
        }
        Ok(())
    }

    pub fn draw(&self, layer: ImageLayer, pass: &mut wgpu::RenderPass<'_>) {
        if self.draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        for draw in self.draws.iter().filter(|draw| draw.layer == layer) {
            if let Some(texture) = self.textures.get(&draw.generation) {
                pass.set_bind_group(0, &texture.bind_group, &[]);
                pass.draw(draw.vertices.clone(), 0..1);
            }
        }
    }

    pub fn texture_uploads(&self) -> u64 {
        self.uploads
    }
}

fn vertex_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("wisp image vertices"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn clipped_quad(
    destination: [f32; 4],
    source: [f32; 4],
    image_size: [u32; 2],
    clip: [f32; 4],
    surface: [u32; 2],
) -> Option<ClippedQuad> {
    let [x, y, width, height] = destination;
    if width <= 0.0 || height <= 0.0 || surface.contains(&0) {
        return None;
    }
    let left = x.max(clip[0]).max(0.0);
    let top = y.max(clip[1]).max(0.0);
    let right = (x + width).min(clip[0] + clip[2]).min(surface[0] as f32);
    let bottom = (y + height).min(clip[1] + clip[3]).min(surface[1] as f32);
    if right <= left || bottom <= top {
        return None;
    }
    let [source_x, source_y, source_width, source_height] = source;
    let u0 = (source_x + (left - x) / width * source_width) / image_size[0] as f32;
    let u1 = (source_x + (right - x) / width * source_width) / image_size[0] as f32;
    let v0 = (source_y + (top - y) / height * source_height) / image_size[1] as f32;
    let v1 = (source_y + (bottom - y) / height * source_height) / image_size[1] as f32;
    let left = left / surface[0] as f32 * 2.0 - 1.0;
    let right = right / surface[0] as f32 * 2.0 - 1.0;
    let top = 1.0 - top / surface[1] as f32 * 2.0;
    let bottom = 1.0 - bottom / surface[1] as f32 * 2.0;
    Some((
        [[left, top], [right, top], [right, bottom], [left, bottom]],
        [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
    ))
}

const IMAGE_SHADER: &str = r#"
struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

@vertex
fn vs_main(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> Output {
    var output: Output;
    output.position = vec4<f32>(position, 0.0, 1.0);
    output.uv = uv;
    return output;
}

@fragment
fn fs_linear(input: Output) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, input.uv);
}

@fragment
fn fs_srgb(input: Output) -> @location(0) vec4<f32> {
    let value = textureSample(image, image_sampler, input.uv);
    let encoded = select(value.rgb * 12.92, 1.055 * pow(value.rgb, vec3<f32>(1.0 / 2.4)) - 0.055, value.rgb > vec3<f32>(0.0031308));
    return vec4<f32>(encoded, value.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{ImagePixels, ImagePlacement};
    use std::sync::Arc;
    use std::time::Duration;

    fn device() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("GPU adapter required for image regression tests");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap()
    }

    fn capture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &ImageRenderer,
        format: wgpu::TextureFormat,
    ) -> Vec<u8> {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Kitty image test target"),
            size: wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let attachments = [Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })];
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &attachments,
                ..Default::default()
            });
            for layer in [
                ImageLayer::BelowBackground,
                ImageLayer::BelowText,
                ImageLayer::AboveText,
            ] {
                renderer.draw(layer, &mut pass);
            }
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Kitty image test readback"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        queue.submit(Some(encoder.finish()));
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            sender.send(result).unwrap()
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(5)),
            })
            .unwrap();
        receiver.recv().unwrap().unwrap();
        let mapped = buffer.get_mapped_range(..).unwrap();
        mapped
            .as_chunks::<256>()
            .0
            .iter()
            .flat_map(|row| row[..64].iter().copied())
            .collect()
    }

    fn pixel(bytes: &[u8], x: usize, y: usize) -> &[u8] {
        &bytes[(y * 16 + x) * 4..(y * 16 + x + 1) * 4]
    }

    fn snapshot(generation: u64, rgba: [u8; 4], z: i32) -> GraphicsSnapshot {
        GraphicsSnapshot {
            generation,
            placements: vec![ImagePlacement {
                image: Arc::new(ImagePixels {
                    generation,
                    width: 1,
                    height: 1,
                    rgba: rgba.to_vec().into(),
                }),
                image_id: 7,
                placement_id: 1,
                z,
                grid_position: [0, -1],
                offset: [0, 0],
                pixel_size: [8, 16],
                source: [0.0, 0.0, 1.0, 1.0],
            }]
            .into(),
        }
    }

    #[test]
    fn image_gpu_pixels_clip_to_content_margin_and_cache_unchanged_uploads() {
        let (device, queue) = device();
        for format in [
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Rgba8Unorm,
        ] {
            let mut renderer = ImageRenderer::new(&device, format);
            let image = snapshot(42, [128, 96, 64, 255], 0);
            renderer
                .prepare(
                    &device,
                    &queue,
                    &image,
                    [16, 16],
                    [8, 8],
                    [3.0, 3.0, 8.0, 8.0],
                )
                .unwrap();
            let pixels = capture(&device, &queue, &renderer, format);
            assert_eq!(pixel(&pixels, 4, 4), &[128, 96, 64, 255]);
            assert_eq!(pixel(&pixels, 2, 4), &[0, 0, 0, 255]);
            assert_eq!(pixel(&pixels, 11, 4), &[0, 0, 0, 255]);
            assert_eq!(pixel(&pixels, 4, 11), &[0, 0, 0, 255]);
            assert_eq!(renderer.texture_uploads(), 1);
            renderer
                .prepare(
                    &device,
                    &queue,
                    &image,
                    [16, 16],
                    [8, 8],
                    [3.0, 3.0, 8.0, 8.0],
                )
                .unwrap();
            assert_eq!(renderer.texture_uploads(), 1);
            let replaced = snapshot(43, [25, 51, 102, 255], -1);
            renderer
                .prepare(
                    &device,
                    &queue,
                    &replaced,
                    [16, 16],
                    [8, 8],
                    [3.0, 3.0, 8.0, 8.0],
                )
                .unwrap();
            assert_eq!(renderer.texture_uploads(), 2);
            assert_eq!(
                pixel(&capture(&device, &queue, &renderer, format), 4, 4),
                &[25, 51, 102, 255]
            );
        }
    }

    #[test]
    fn image_gpu_layers_and_alpha_blend_in_z_order() {
        let (device, queue) = device();
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = ImageRenderer::new(&device, format);
        let below = snapshot(1, [255, 0, 0, 255], i32::MIN);
        let above = snapshot(2, [0, 0, 255, 128], 0);
        let images = GraphicsSnapshot {
            generation: 3,
            placements: vec![below.placements[0].clone(), above.placements[0].clone()].into(),
        };
        renderer
            .prepare(
                &device,
                &queue,
                &images,
                [16, 16],
                [8, 8],
                [0.0, 8.0, 16.0, 8.0],
            )
            .unwrap();
        let pixels = capture(&device, &queue, &renderer, format);
        let blended = pixel(&pixels, 2, 10);
        assert!((i16::from(blended[0]) - 187).abs() <= 1);
        assert_eq!(blended[1], 0);
        assert!((i16::from(blended[2]) - 188).abs() <= 1);
        assert_eq!(blended[3], 255);
    }

    #[test]
    fn image_gpu_source_crop_samples_the_requested_quadrant() {
        let (device, queue) = device();
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = ImageRenderer::new(&device, format);
        let image = GraphicsSnapshot {
            generation: 17,
            placements: vec![ImagePlacement {
                image: Arc::new(ImagePixels {
                    generation: 17,
                    width: 4,
                    height: 4,
                    rgba: [
                        [255, 0, 0, 255],
                        [255, 0, 0, 255],
                        [0, 255, 0, 255],
                        [0, 255, 0, 255],
                        [255, 0, 0, 255],
                        [255, 0, 0, 255],
                        [0, 255, 0, 255],
                        [0, 255, 0, 255],
                        [0, 0, 255, 255],
                        [0, 0, 255, 255],
                        [255, 255, 255, 255],
                        [255, 255, 255, 255],
                        [0, 0, 255, 255],
                        [0, 0, 255, 255],
                        [255, 255, 255, 255],
                        [255, 255, 255, 255],
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .into(),
                }),
                image_id: 17,
                placement_id: 3,
                z: 0,
                grid_position: [0, 0],
                offset: [0, 0],
                pixel_size: [8, 8],
                source: [2.0, 0.0, 2.0, 2.0],
            }]
            .into(),
        };
        renderer
            .prepare(
                &device,
                &queue,
                &image,
                [16, 16],
                [8, 8],
                [3.0, 3.0, 8.0, 8.0],
            )
            .unwrap();
        let pixels = capture(&device, &queue, &renderer, format);
        for y in 5..8 {
            for x in 5..9 {
                assert_eq!(pixel(&pixels, x, y), &[0, 255, 0, 255]);
            }
        }
    }
}
