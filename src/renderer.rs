use wasm_bindgen::JsCast;
use web_sys::HtmlCanvasElement;
use wgpu::util::DeviceExt;

use crate::geo::Cap;
use crate::mesh::{CellVertex, Vertex, cap_mesh, cylinder, patch};
use crate::palette;
use crate::scene::{Column, Marker, Scene, Segment};
use crate::terrain::{BORDER_PX, TILE_PX, TileKey};
use crate::view::{LAYER_CELLS, LAYER_FERRIES};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Uncaptured WebGPU errors logged before going quiet.
const MAX_LOGGED_ERRORS: u32 = 20;
/// A new canvas size is applied once it has held for this many frames:
/// each resize rebuilds the browser's swapchain, and Firefox crashed when
/// two rebuilds raced (layout can wobble as a tab returns).
const RESIZE_SETTLE_FRAMES: u32 = 4;
const COLUMN_SIDES: u32 = 16;
/// Slope tiles the GPU keeps (at 512 KiB each); see `terrain::Cache`.
pub const TERRAIN_SLOTS: u32 = 192;
/// Height tiles that can arrive in one frame.
pub const STAGING_LAYERS: usize = 8;
/// Quads per side of a terrain patch: enough to follow the sphere's
/// curve across a zoom-5 tile to within about 15 m.
const PATCH_SEGMENTS: u32 = 32;
const SLOPES_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Water widths, alongside the slopes (normals.wgsl).
const WIDTHS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const STAGING_PX: u32 = TILE_PX + 2 * BORDER_PX;
/// Slope tiles: the tile plus one texel of each neighbor, so linear
/// filtering blends across tile edges (terrain.wgsl maps into it).
const SLOPES_PX: u32 = TILE_PX + 2;
/// The backdrop sits this fraction of the radius down (about 640 m), below
/// its own facets' sag, so the terrain always draws over it.
const BACKDROP_SCALE: f64 = 0.9999;

/// Line overlays, each drawn when its layer bit is set: the hex grid, and
/// the ferry routes.
pub const OVERLAYS: [u32; 2] = [LAYER_CELLS, LAYER_FERRIES];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    /// Toward the sun, fixed over the slice.
    pub light_dir: [f32; 4],
    /// Viewport width and height (physical px), planet radius (m), line width (px).
    pub params: [f32; 4],
    /// Bitmask of visible `view::LAYER_*` flags, unused,
    /// `view::STYLE_*`, then unused.
    pub flags: [u32; 4],
    /// The slice of the planet shown: its center (unit vector) and the
    /// cosine of its angular radius.
    pub slice: [f32; 4],
    /// Terrain vertical exaggeration, ground meters per CSS pixel at the
    /// view's center (for dashes), then unused.
    pub terrain: [f32; 4],
}

/// One terrain region to draw (see `terrain::select`), tightly packed to
/// match `vertex_attr_array!` offsets.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TerrainInstance {
    /// The region's zoom, x and y.
    pub key: [u32; 3],
    /// The cache slot holding its source tile.
    pub slot: u32,
    /// The region's offset (xy) and size (z) within the source tile.
    pub source: [f32; 4],
}

/// A freshly loaded height tile and the cache slot its slopes go to.
pub struct TileUpload {
    /// RGBA, `terrain::decode_tile`'s layout.
    pub rgba: Vec<u8>,
    pub key: TileKey,
    pub slot: u32,
}

/// Builds a shader from common.wgsl, any shared snippets, then the file.
macro_rules! shader {
    ($device:expr, $($file:literal),+) => {
        $device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(concat!($($file, " "),+)),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("shaders/common.wgsl"),
                    $(include_str!(concat!("shaders/", $file))),+
                )
                .into(),
            ),
        })
    };
}

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

impl GpuMesh {
    fn bytes(&self) -> u64 {
        self.vertices.size() + self.indices.size()
    }
}

/// What the last frame drew, for the performance panel.
#[derive(Clone, Copy, Default)]
pub struct FrameCounts {
    pub draw_calls: u32,
    pub triangles: u64,
    pub line_segments: u64,
}

impl GpuMesh {
    fn new<V: bytemuck::Pod>(
        device: &wgpu::Device,
        label: &str,
        vertices: &[V],
        indices: &[u32],
    ) -> Self {
        Self {
            vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            }),
            index_count: indices.len() as u32,
        }
    }
}

/// Per-frame instance data in a vertex buffer that grows as needed.
struct InstanceBuffer {
    label: &'static str,
    buffer: wgpu::Buffer,
    count: u32,
}

impl InstanceBuffer {
    fn new(device: &wgpu::Device, label: &'static str) -> Self {
        Self {
            label,
            buffer: Self::allocate(device, label, 4096),
            count: 0,
        }
    }

    fn allocate(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn write<T: bytemuck::Pod>(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[T]) {
        let bytes: &[u8] = bytemuck::cast_slice(data);
        if bytes.len() as u64 > self.buffer.size() {
            let size = (bytes.len() as u64).next_power_of_two();
            self.buffer = Self::allocate(device, self.label, size);
        }
        queue.write_buffer(&self.buffer, 0, bytes);
        self.count = data.len() as u32;
    }
}

pub struct Renderer {
    canvas: HtmlCanvasElement,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The canvas needs configuring before the next frame (see `render`
    /// and `reconfigure`).
    needs_configure: bool,
    /// A canvas size waiting to settle, and for how many frames it has.
    pending_size: Option<((u32, u32), u32)>,
    depth_view: wgpu::TextureView,
    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    /// The backdrop: the slice where no terrain tile has loaded yet.
    globe_pipeline: wgpu::RenderPipeline,
    backdrop: GpuMesh,
    /// Terrain patches.
    terrain_pipeline: wgpu::RenderPipeline,
    patch: GpuMesh,
    terrain: InstanceBuffer,
    /// Height tiles as they arrive, and the pass turning them into slopes.
    staging: wgpu::Texture,
    normals_pipeline: wgpu::ComputePipeline,
    normals_bind_group: wgpu::BindGroup,
    jobs: wgpu::Buffer,
    surface_bind_group: wgpu::BindGroup,
    /// Bytes in the surface bind group's buffers.
    surface_bytes: u64,
    /// What the last frame drew.
    pub counts: FrameCounts,
    /// Line overlays in the order of `OVERLAYS`.
    overlays: Vec<InstanceBuffer>,
    column_pipeline: wgpu::RenderPipeline,
    column_mesh: GpuMesh,
    columns: InstanceBuffer,
    line_pipeline: wgpu::RenderPipeline,
    segments: InstanceBuffer,
    marker_pipeline: wgpu::RenderPipeline,
    markers: InstanceBuffer,
}

impl Renderer {
    pub async fn new(canvas: HtmlCanvasElement, slice: &Cap) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| format!("create surface: {e}"))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("no WebGPU adapter: {e}"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(|e| format!("request device: {e}"))?;
        // WebGPU errors otherwise only surface as a blank or frozen canvas.
        // Log the first few; one per frame would flood the console.
        let errors = std::sync::atomic::AtomicU32::new(0);
        device.on_uncaptured_error(std::sync::Arc::new(move |e| {
            let n = errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n < MAX_LOGGED_ERRORS {
                log::error!("WebGPU error {}: {e}", n + 1);
            }
        }));

        let (width, height) = displayed_size(&canvas);
        let caps = surface.get_capabilities(&adapter);
        // The canvas in its plain (not sRGB) format, with no extra view
        // formats: an sRGB view needs a mutable-format image, which Firefox
        // on Mesa can't share with its compositor; it then falls back to a
        // path that crashed the browser. The shaders encode their linear
        // color to sRGB themselves instead (`display` in common.wgsl).
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: Default::default(),
        };
        surface.configure(&device, &config);

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("globals"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let storage_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let surface_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("surface"),
            entries: &[
                storage_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let globe_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("globe"),
            bind_group_layouts: &[Some(&bind_group_layout), Some(&surface_layout)],
            immediate_size: 0,
        });

        let mesh_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        };

        let cell_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<CellVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Uint32],
        };
        let globe_shader = shader!(device, "surface.wgsl", "globe.wgsl");
        let globe_pipeline = create_pipeline(
            &device,
            &globe_layout,
            "globe",
            &globe_shader,
            &[Some(cell_vertex_layout.clone())],
            format,
            Some(wgpu::Face::Back),
            true,
            None,
        );

        // Patches are only seen from outside anyway, so no culling.
        let terrain_shader = shader!(device, "surface.wgsl", "styles.wgsl", "terrain.wgsl");
        let terrain_buffers = [
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<[f32; 2]>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2],
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<TerrainInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![1 => Uint32x3, 2 => Uint32, 3 => Float32x4],
            }),
        ];
        let terrain_pipeline = create_pipeline(
            &device,
            &globe_layout,
            "terrain",
            &terrain_shader,
            &terrain_buffers,
            format,
            None,
            true,
            None,
        );
        let (normals_pipeline, normals_bind_group, staging, slopes, widths, jobs) =
            create_normals_pass(&device);

        let column_shader = shader!(device, "columns.wgsl");
        let column_pipeline = create_pipeline(
            &device,
            &layout,
            "columns",
            &column_shader,
            &[
                Some(mesh_layout),
                Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Column>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        2 => Float32x3, 3 => Float32, 4 => Float32x3, 5 => Float32, 6 => Float32x4
                    ],
                }),
            ],
            format,
            None,
            true,
            None,
        );

        let line_shader = shader!(device, "lines.wgsl");
        let line_pipeline = create_pipeline(
            &device,
            &layout,
            "lines",
            &line_shader,
            &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Segment>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x3,
                    1 => Float32x3,
                    2 => Float32x4,
                    3 => Float32,
                    4 => Float32x2,
                    5 => Float32
                ],
            })],
            format,
            None,
            false,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );

        // Markers sit in front of everything (see markers.wgsl), so they
        // neither test against nor write depth that matters.
        let marker_shader = shader!(device, "markers.wgsl");
        let marker_pipeline = create_pipeline(
            &device,
            &layout,
            "markers",
            &marker_shader,
            &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Marker>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x3,
                    1 => Float32x3,
                    2 => Float32x2,
                    3 => Float32x4,
                    4 => Float32x4,
                    5 => Float32x4,
                    6 => Float32x4
                ],
            })],
            format,
            None,
            false,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );

        let (backdrop_vertices, backdrop_indices) = cap_mesh(slice, BACKDROP_SCALE);
        let backdrop = GpuMesh::new(&device, "backdrop", &backdrop_vertices, &backdrop_indices);
        let column = cylinder(COLUMN_SIDES);
        let (surface_bind_group, surface_bytes) =
            create_surface_bind_group(&device, &surface_layout, &slopes, &widths);
        let (patch_vertices, patch_indices) = patch(PATCH_SEGMENTS);
        Ok(Self {
            depth_view: create_depth_view(&device, width, height),
            backdrop,
            surface_bind_group,
            surface_bytes,
            counts: FrameCounts::default(),
            terrain_pipeline,
            patch: GpuMesh::new(&device, "patch", &patch_vertices, &patch_indices),
            terrain: InstanceBuffer::new(&device, "terrain instances"),
            staging,
            normals_pipeline,
            normals_bind_group,
            jobs,
            overlays: OVERLAYS
                .iter()
                .map(|_| InstanceBuffer::new(&device, "overlay"))
                .collect(),
            column_mesh: GpuMesh::new(&device, "column", &column.vertices, &column.indices),
            columns: InstanceBuffer::new(&device, "column instances"),
            segments: InstanceBuffer::new(&device, "segment instances"),
            marker_pipeline,
            markers: InstanceBuffer::new(&device, "marker instances"),
            canvas,
            surface,
            device,
            queue,
            config,
            needs_configure: false,
            pending_size: None,
            globals_buffer,
            globals_bind_group,
            globe_pipeline,
            column_pipeline,
            line_pipeline,
        })
    }

    /// Estimated GPU memory we have allocated: meshes, per-cell data, the
    /// terrain tiles, the depth buffer and instance buffers. Excludes the
    /// browser's own canvas buffers.
    pub fn gpu_bytes(&self) -> u64 {
        let depth = self.config.width as u64 * self.config.height as u64 * 4;
        let instances: u64 = self
            .overlays
            .iter()
            .chain([&self.columns, &self.segments, &self.markers, &self.terrain])
            .map(|b| b.buffer.size())
            .sum();
        // Slopes at 8 bytes a texel, widths at 4.
        let slopes = u64::from(SLOPES_PX * SLOPES_PX * TERRAIN_SLOTS) * (8 + 4);
        let staging = u64::from(STAGING_PX * STAGING_PX) * STAGING_LAYERS as u64 * 4;
        self.backdrop.bytes()
            + self.patch.bytes()
            + slopes
            + staging
            + self.column_mesh.bytes()
            + self.surface_bytes
            + self.globals_buffer.size()
            + depth
            + instances
    }

    /// Calls `f` with a description if the GPU device is lost (a driver
    /// reset or crash, or the browser reclaiming it), after which nothing
    /// more can be drawn.
    pub fn on_device_lost(&self, f: impl Fn(String) + Send + 'static) {
        self.device
            .set_device_lost_callback(move |reason, message| {
                f(format!("The GPU device was lost ({reason:?}): {message}"));
            });
    }

    /// Resize the surface to the canvas's displayed size if it changed, and
    /// return the size in physical pixels.
    pub fn sync_size(&mut self) -> (u32, u32) {
        let wanted = displayed_size(&self.canvas);
        let current = (self.config.width, self.config.height);
        if wanted == current {
            self.pending_size = None;
        } else {
            let frames = match self.pending_size {
                Some((size, frames)) if size == wanted => frames + 1,
                _ => 1,
            };
            self.pending_size = Some((wanted, frames));
            if frames >= RESIZE_SETTLE_FRAMES {
                let (width, height) = wanted;
                self.config.width = width;
                self.config.height = height;
                self.depth_view = create_depth_view(&self.device, width, height);
                self.needs_configure = true;
                self.pending_size = None;
            }
        }
        if self.needs_configure {
            reconfigure(&self.canvas, &self.surface, &self.device, &self.config);
            self.needs_configure = false;
        }
        (self.config.width, self.config.height)
    }

    /// Whether the canvas needs frames to settle a resize or reconfigure.
    pub fn needs_frame(&self) -> bool {
        self.needs_configure
            || self.pending_size.is_some()
            || displayed_size(&self.canvas) != (self.config.width, self.config.height)
    }

    /// Replace the lines of the overlay drawn for `layer` (one of
    /// `OVERLAYS`).
    pub fn set_overlay(&mut self, layer: u32, segments: &[Segment]) {
        if let Some(i) = OVERLAYS.iter().position(|&l| l == layer) {
            self.overlays[i].write(&self.device, &self.queue, segments);
        }
    }

    /// Copies freshly loaded height tiles into the staging layers and turns
    /// each into slopes in its cache slot. At most `STAGING_LAYERS` a call.
    pub fn upload_tiles(&mut self, tiles: &[TileUpload]) {
        assert!(tiles.len() <= STAGING_LAYERS);
        if tiles.is_empty() {
            return;
        }
        let size = wgpu::Extent3d {
            width: STAGING_PX,
            height: STAGING_PX,
            depth_or_array_layers: 1,
        };
        let mut jobs = Vec::with_capacity(tiles.len());
        for (layer, tile) in tiles.iter().enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.staging,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &tile.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(STAGING_PX * 4),
                    rows_per_image: Some(STAGING_PX),
                },
                size,
            );
            // Must match Job in normals.wgsl.
            jobs.push([layer as u32, tile.slot, u32::from(tile.key.z), tile.key.y]);
        }
        self.queue
            .write_buffer(&self.jobs, 0, bytemuck::cast_slice(&jobs));
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.normals_pipeline);
            pass.set_bind_group(0, &self.normals_bind_group, &[]);
            // 8x8 workgroups over each slope tile.
            let groups = SLOPES_PX.div_ceil(8);
            pass.dispatch_workgroups(groups, groups, tiles.len() as u32);
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Draws a frame: the terrain `regions`, then the scene's markers and
    /// lines, and the overlays switched on in `globals`.
    pub fn render(&mut self, globals: &Globals, scene: &Scene, regions: &[TerrainInstance]) {
        self.queue
            .write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(globals));
        self.terrain.write(&self.device, &self.queue, regions);
        self.columns
            .write(&self.device, &self.queue, &scene.columns);
        self.segments
            .write(&self.device, &self.queue, &scene.segments);
        self.markers
            .write(&self.device, &self.queue, &scene.markers);

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // Configure on the next frame, not now: returning to the tab
            // already has the browser rebuilding the canvas, and two
            // configures in a row crashed Firefox.
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.needs_configure = true;
                return;
            }
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // Near-black space, already sRGB (the shaders
                        // encode, the clear doesn't go through one).
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.052,
                            g: 0.061,
                            b: 0.086,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.globals_bind_group, &[]);

            pass.set_bind_group(1, &self.surface_bind_group, &[]);
            let mut counts = FrameCounts::default();
            if self.terrain.count > 0 {
                pass.set_pipeline(&self.terrain_pipeline);
                pass.set_vertex_buffer(0, self.patch.vertices.slice(..));
                pass.set_vertex_buffer(1, self.terrain.buffer.slice(..));
                pass.set_index_buffer(self.patch.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..self.patch.index_count, 0, 0..self.terrain.count);
                counts.draw_calls += 1;
                counts.triangles += self.patch.index_count as u64 / 3 * self.terrain.count as u64;
            }
            // The backdrop after the terrain: it sits below it, so the
            // depth test skips it wherever terrain has drawn, rather than
            // shading the whole slice twice.
            pass.set_pipeline(&self.globe_pipeline);
            pass.set_vertex_buffer(0, self.backdrop.vertices.slice(..));
            pass.set_index_buffer(self.backdrop.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.backdrop.index_count, 0, 0..1);
            counts.draw_calls += 1;
            counts.triangles += self.backdrop.index_count as u64 / 3;
            if self.columns.count > 0 {
                pass.set_pipeline(&self.column_pipeline);
                pass.set_vertex_buffer(0, self.column_mesh.vertices.slice(..));
                pass.set_vertex_buffer(1, self.columns.buffer.slice(..));
                pass.set_index_buffer(
                    self.column_mesh.indices.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                pass.draw_indexed(0..self.column_mesh.index_count, 0, 0..self.columns.count);
                counts.draw_calls += 1;
                counts.triangles +=
                    self.column_mesh.index_count as u64 / 3 * self.columns.count as u64;
            }

            // Lines last: they test depth but don't write it, and blend.
            pass.set_pipeline(&self.line_pipeline);
            let visible = OVERLAYS
                .iter()
                .zip(&self.overlays)
                .filter(|(layer, _)| globals.flags[0] & **layer != 0)
                .map(|(_, lines)| lines)
                .chain([&self.segments]);
            for lines in visible.filter(|l| l.count > 0) {
                pass.set_vertex_buffer(0, lines.buffer.slice(..));
                pass.draw(0..6, 0..lines.count);
                counts.draw_calls += 1;
                // Each segment is a quad of two triangles.
                counts.line_segments += lines.count as u64;
                counts.triangles += 2 * lines.count as u64;
            }
            // Markers over everything, lines included.
            if self.markers.count > 0 {
                pass.set_pipeline(&self.marker_pipeline);
                pass.set_vertex_buffer(0, self.markers.buffer.slice(..));
                pass.draw(0..6, 0..self.markers.count);
                counts.draw_calls += 1;
                counts.triangles += 2 * self.markers.count as u64;
            }
            self.counts = counts;
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }
}

#[allow(clippy::too_many_arguments)]
fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    label: &str,
    shader: &wgpu::ShaderModule,
    buffers: &[Option<wgpu::VertexBufferLayout>],
    format: wgpu::TextureFormat,
    cull_mode: Option<wgpu::Face>,
    depth_write: bool,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(depth_write),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_surface_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    slopes: &wgpu::TextureView,
    widths: &wgpu::TextureView,
) -> (wgpu::BindGroup, u64) {
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("terrain tiles"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let storage = |label, contents: &[u8]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage: wgpu::BufferUsages::STORAGE,
        })
    };
    let palettes = storage("palettes", bytemuck::cast_slice(&palette::gpu_table()));
    let bytes = palettes.size();
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("surface"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 1,
                resource: palettes.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(slopes),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(widths),
            },
        ],
    });
    (group, bytes)
}

/// The compute pass turning height tiles into slopes (normals.wgsl): its
/// pipeline and bind group, the staging array heights arrive in, the
/// slopes and widths cache arrays (views for sampling) and the jobs
/// buffer.
fn create_normals_pass(
    device: &wgpu::Device,
) -> (
    wgpu::ComputePipeline,
    wgpu::BindGroup,
    wgpu::Texture,
    wgpu::TextureView,
    wgpu::TextureView,
    wgpu::Buffer,
) {
    let array = |label, size, layers, format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let staging = array(
        "terrain staging",
        STAGING_PX,
        STAGING_LAYERS as u32,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let slopes = array(
        "terrain slopes",
        SLOPES_PX,
        TERRAIN_SLOTS,
        SLOPES_FORMAT,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let widths = array(
        "terrain widths",
        SLOPES_PX,
        TERRAIN_SLOTS,
        WIDTHS_FORMAT,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let array_view = |t: &wgpu::Texture| {
        t.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        })
    };
    let jobs = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("terrain jobs"),
        size: (STAGING_LAYERS * std::mem::size_of::<[u32; 4]>()) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("normals"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: SLOPES_FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: WIDTHS_FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                },
                count: None,
            },
        ],
    });
    let slopes_view = array_view(&slopes);
    let widths_view = array_view(&widths);
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("normals"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&array_view(&staging)),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&slopes_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: jobs.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&widths_view),
            },
        ],
    });
    // Standalone: no common.wgsl, as the pass has no globals.
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("normals.wgsl"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/normals.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("normals"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("normals"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    (
        pipeline,
        bind_group,
        staging,
        slopes_view,
        widths_view,
        jobs,
    )
}

/// Canvas size in physical pixels, never zero.
fn displayed_size(canvas: &HtmlCanvasElement) -> (u32, u32) {
    let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
    let width = ((canvas.client_width() as f64 * dpr) as u32).max(1);
    let height = ((canvas.client_height() as f64 * dpr) as u32).max(1);
    (width, height)
}

/// Configures the canvas again, at `config`'s size. Resizing a configured
/// canvas rebuilds the browser's swapchain, once for the width and again
/// for the height, and on Mesa Firefox's fallback crashed the whole browser
/// when rebuilds came in pairs like that (moving the window between
/// monitors). Unconfigured, the canvas can be resized freely, and the
/// configure (which sets the size first) builds one swapchain.
fn reconfigure(
    canvas: &HtmlCanvasElement,
    surface: &wgpu::Surface<'static>,
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) {
    // The context wgpu configured (getContext returns the same one), and
    // its `unconfigure`, which web-sys only has behind unstable APIs.
    if let Ok(Some(context)) = canvas.get_context("webgpu")
        && let Ok(f) = js_sys::Reflect::get(&context, &"unconfigure".into())
        && let Some(f) = f.dyn_ref::<js_sys::Function>()
    {
        let _ = f.call0(&context);
    }
    surface.configure(device, config);
}

fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}
