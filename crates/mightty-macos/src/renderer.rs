//! Metal renderer. Every frame is a single instanced draw of quads: cell
//! backgrounds and the cursor first, then decorations and glyphs sampled
//! from a glyph atlas.

use std::collections::HashMap;
use std::env;
use std::ffi::c_void;
use std::mem;
use std::path::PathBuf;

use mightty_render::{Rgb, Theme, boxdraw};
use mightty_term::{Color, Flags, Row, StyledCell, Term};

use crate::ffi::*;
use crate::font::{Font, RasterGlyph, Style};
use crate::snapshot;

/// The glyph atlas starts small and doubles when full, up to the maximum;
/// a full maximum-size atlas is cleared instead. At 4 bytes a pixel, the
/// largest is 16 MiB, while plain text fits in the first 1 MiB.
const ATLAS_MIN: u32 = 512;
const ATLAS_MAX: u32 = 2048;
const PIXEL_FORMAT_BGRA8: usize = 80;
/// Padding around the grid, in points.
const PADDING: f64 = 6.0;

const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Quad {
    float2 pos;      // pixels, top-left origin
    float2 size;
    float2 uv;       // atlas pixels
    float2 uv_size;
    float4 color;
    uint mode;       // 0 solid, 1 coverage mask tinted by color, 2 colour bitmap
};

struct VOut {
    float4 position [[position]];
    float2 uv;
    float4 color;
    uint mode [[flat]];
};

vertex VOut vs(uint vid [[vertex_id]], uint iid [[instance_id]],
               const device Quad *quads [[buffer(0)]],
               constant float2 &viewport [[buffer(1)]]) {
    Quad q = quads[iid];
    float2 corner = float2(vid & 1, vid >> 1);
    float2 p = q.pos + corner * q.size;
    VOut out;
    out.position = float4(p.x / viewport.x * 2.0 - 1.0, 1.0 - p.y / viewport.y * 2.0, 0.0, 1.0);
    out.uv = q.uv + corner * q.uv_size;
    out.color = q.color;
    out.mode = q.mode;
    return out;
}

fragment float4 fs(VOut in [[stage_in]], texture2d<float> atlas [[texture(0)]]) {
    constexpr sampler s(coord::pixel, filter::nearest);
    if (in.mode == 0) {
        return float4(in.color.rgb * in.color.a, in.color.a);
    }
    float4 texel = atlas.sample(s, in.uv);
    if (in.mode == 1) {
        float a = texel.a * in.color.a;
        return float4(in.color.rgb * a, a);
    }
    return texel * in.color.a;
}
"#;

/// Must match `struct Quad` in the shader (64 bytes with padding).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Quad {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 2],
    uv_size: [f32; 2],
    color: [f32; 4],
    mode: u32,
    _pad: [u32; 3],
}

const SOLID: u32 = 0;
const MASK: u32 = 1;
const BITMAP: u32 = 2;

impl Quad {
    fn solid(x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) -> Self {
        Self { pos: [x, y], size: [w, h], color, mode: SOLID, ..Default::default() }
    }
}

/// Inclusive `(line, column)` range in absolute lines (see `Term::line`), start first.
pub type Selection = ((u64, usize), (u64, usize));

/// What a glyph draws: a character, or a grapheme cluster of several.
#[derive(Clone, PartialEq, Eq, Hash)]
enum GlyphKey {
    Char(char),
    Cluster(Box<str>),
}

/// What a frame needs from the terminal, copied out while it is locked so
/// that building the frame doesn't hold the lock against the PTY reader.
#[derive(Default)]
pub struct Snapshot {
    cols: usize,
    rows: usize,
    /// `rows × cols`, row-major, styles looked up, short rows padded with blanks.
    cells: Vec<StyledCell>,
    /// Absolute line number of the first visible row.
    first_line: u64,
    /// Row, column and DECSCUSR shape, when the cursor is visible.
    cursor: Option<(usize, usize, u16)>,
    /// Text of the cells holding grapheme clusters, by index into `cells`.
    clusters: HashMap<usize, Box<str>>,
}

impl Snapshot {
    /// The character at a visible cell.
    pub fn char_at(&self, row: usize, col: usize) -> Option<char> {
        (row < self.rows && col < self.cols).then(|| self.cells[row * self.cols + col].c)
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Copies the visible screen, `scroll_offset` lines up into scrollback.
    pub fn capture(&mut self, term: &Term, scroll_offset: usize) {
        let history = term.scrollback();
        let lines = term.lines();
        let offset = if term.is_alt_screen() { 0 } else { scroll_offset.min(history.len()) };
        let (cols, rows) = (term.cols(), term.rows());
        self.cols = cols;
        self.rows = rows;
        self.cells.clear();
        self.clusters.clear();
        for r in 0..rows {
            let index = history.len() - offset + r;
            let row: &Row = if index < history.len() { &history[index] } else { &lines[index - history.len()] };
            let n = row.cells.len().min(cols);
            let start = self.cells.len();
            self.cells.extend(row.cells[..n].iter().map(|&cell| term.resolve(cell)));
            for (i, &cell) in row.cells[..n].iter().enumerate() {
                if let Some(text) = term.cell_text(cell) {
                    self.clusters.insert(start + i, text.into());
                }
            }
            self.cells.resize(self.cells.len() + cols - n, StyledCell::BLANK);
        }
        self.first_line = term.lines_scrolled() - offset as u64;
        let (cursor_row, cursor_col) = term.cursor();
        let visible = offset == 0 && term.modes().show_cursor;
        self.cursor = visible.then_some((cursor_row, cursor_col, term.modes().cursor_shape));
    }
}

/// Quads for one frame.
pub struct Frame(Vec<Quad>);

#[derive(Clone, Copy)]
struct Glyph {
    uv: [f32; 2],
    size: [f32; 2],
    color: bool,
}

/// Shelf packer over the atlas texture.
struct Atlas {
    texture: Id,
    size: u32,
    x: u32,
    y: u32,
    row_height: u32,
}

/// Vertex buffers reused round-robin. A buffer is refilled only once the
/// GPU has finished the frame that last read it; creating one per frame
/// instead leaves the driver holding tens of MiB of released buffers.
const VERTEX_BUFFERS: usize = 3;

#[derive(Clone, Copy)]
struct VertexBuffer {
    buffer: Id,
    bytes: usize,
    /// The command buffer that last read `buffer` (retained), or NIL.
    commands: Id,
}

pub struct Renderer {
    device: Id,
    queue: Id,
    pipeline: Id,
    layer: Id,
    atlas: Atlas,
    glyphs: HashMap<(GlyphKey, Style), Option<Glyph>>,
    /// Set when the atlas was recycled mid-frame; the frame is rebuilt.
    atlas_reset: bool,
    font: Font,
    font_name: String,
    font_size: f64,
    scale: f64,
    theme: Theme,
    /// Quad lists kept between frames, so drawing doesn't allocate.
    spare: Vec<Quad>,
    front: Vec<Quad>,
    vertices: [VertexBuffer; VERTEX_BUFFERS],
    next_vertices: usize,
    pub focused: bool,
    /// `MIGHTTY_SNAPSHOT`: every frame is also written to this PNG.
    snapshot: Option<PathBuf>,
}

impl Renderer {
    /// `layer` must be a `CAMetalLayer`. `font_size` is in points.
    /// Without `vsync`, frames show as soon as they're ready, possibly torn.
    pub fn new(layer: Id, font_name: &str, font_size: f64, scale: f64, theme: Theme, vsync: bool) -> Self {
        // SAFETY: Metal calls with the documented signatures.
        unsafe {
            let device = MTLCreateSystemDefaultDevice();
            assert!(!device.is_null(), "no Metal device");
            call!(layer, c"setDevice:", device);
            call!(layer, c"setPixelFormat:", PIXEL_FORMAT_BGRA8);
            let snapshot = env::var_os("MIGHTTY_SNAPSHOT").map(PathBuf::from);
            // Snapshots read the drawable back, which framebuffer-only textures forbid.
            call!(layer, c"setFramebufferOnly:", if snapshot.is_some() { NO } else { YES });
            call!(layer, c"setOpaque:", YES);
            // Each drawable is a full window image (~11 MiB at 120x40 on
            // Retina); frames are paced by the display, so two are enough.
            call!(layer, c"setMaximumDrawableCount:", 2usize);
            call!(layer, c"setDisplaySyncEnabled:", if vsync { YES } else { NO });
            // Keep content pinned during live resize instead of stretching it.
            call!(layer, c"setContentsGravity:", kCAGravityTopLeft);

            Self {
                device,
                queue: msg!(device, c"newCommandQueue"),
                pipeline: create_pipeline(device),
                layer,
                atlas: Atlas { texture: create_atlas(device, ATLAS_MIN), size: ATLAS_MIN, x: 0, y: 0, row_height: 0 },
                glyphs: HashMap::new(),
                atlas_reset: false,
                font: Font::new(font_name, font_size * scale),
                font_name: font_name.to_owned(),
                font_size,
                scale,
                theme,
                spare: Vec::new(),
                front: Vec::new(),
                vertices: [VertexBuffer { buffer: NIL, bytes: 0, commands: NIL }; VERTEX_BUFFERS],
                next_vertices: 0,
                focused: true,
                snapshot,
            }
        }
    }

    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// Rebuilds the font for a new backing scale (moving between displays).
    pub fn set_scale(&mut self, scale: f64) {
        self.scale = scale;
        self.font = Font::new(&self.font_name, self.font_size * scale);
        self.reset_atlas();
    }

    /// Cell size in pixels.
    pub fn cell_size(&self) -> (u32, u32) {
        (self.font.cell_width, self.font.cell_height)
    }

    pub fn padding_px(&self) -> f64 {
        (PADDING * self.scale).round()
    }

    /// Columns and rows that fit in a drawable of `width`×`height` pixels.
    pub fn grid_size(&self, width: f64, height: f64) -> (usize, usize) {
        let pad = 2.0 * self.padding_px();
        let (cw, ch) = self.cell_size();
        let cols = ((width - pad) / cw as f64).floor().max(1.0) as usize;
        let rows = ((height - pad) / ch as f64).floor().max(1.0) as usize;
        (cols, rows)
    }

    /// Builds a frame from a snapshot; needs no terminal lock.
    pub fn prepare(&mut self, snapshot: &Snapshot, selection: Option<Selection>) -> Frame {
        let mut quads = self.build_quads(snapshot, selection);
        // The atlas grew or was cleared mid-frame, dropping glyphs already placed.
        for _ in 0..3 {
            if !mem::take(&mut self.atlas_reset) {
                break;
            }
            self.spare = quads;
            quads = self.build_quads(snapshot, selection);
        }
        Frame(quads)
    }

    /// Draws `frame`; `presented` is called with the drawable once it's on
    /// screen (or dropped).
    pub fn present(&mut self, Frame(mut quads): Frame, presented: Option<&'static Block>) {
        // SAFETY: Metal calls with the documented signatures; objects created
        // with `new...` are released, the rest are autoreleased.
        unsafe {
            let size: CGSize = msg!(self.layer, c"drawableSize");
            let drawable: Id = msg!(self.layer, c"nextDrawable");
            if drawable.is_null() {
                return;
            }
            let pass: Id = msg!(class(c"MTLRenderPassDescriptor"), c"renderPassDescriptor");
            let attachment: Id = msg!(msg!(pass, c"colorAttachments"), c"objectAtIndexedSubscript:", 0usize);
            let [r, g, b, _] = rgba(self.theme.background);
            let texture: Id = msg!(drawable, c"texture");
            call!(attachment, c"setTexture:", texture);
            call!(attachment, c"setLoadAction:", 2usize); // clear
            call!(attachment, c"setStoreAction:", 1usize); // store
            call!(attachment, c"setClearColor:", MTLClearColor {
                red: r as f64,
                green: g as f64,
                blue: b as f64,
                alpha: 1.0
            });

            let commands: Id = msg!(self.queue, c"commandBuffer");
            let encoder: Id = msg!(commands, c"renderCommandEncoderWithDescriptor:", pass);
            let slot = self.next_vertices;
            if !quads.is_empty() {
                self.next_vertices = (slot + 1) % VERTEX_BUFFERS;
                let bytes = quads.len() * mem::size_of::<Quad>();
                let buffer = self.vertex_buffer(slot, bytes);
                let contents: *mut c_void = msg!(buffer, c"contents");
                std::ptr::copy_nonoverlapping(quads.as_ptr() as *const u8, contents as *mut u8, bytes);
                let viewport = [size.width as f32, size.height as f32];
                call!(encoder, c"setRenderPipelineState:", self.pipeline);
                call!(encoder, c"setVertexBuffer:offset:atIndex:", buffer, 0usize, 0usize);
                call!(encoder, c"setVertexBytes:length:atIndex:", viewport.as_ptr() as *const c_void, 8usize, 1usize);
                call!(encoder, c"setFragmentTexture:atIndex:", self.atlas.texture, 0usize);
                // Triangle strip, 4 vertices per quad instance.
                call!(encoder, c"drawPrimitives:vertexStart:vertexCount:instanceCount:", 4usize, 0usize, 4usize, quads.len());
            }
            call!(encoder, c"endEncoding");
            if let Some(block) = presented {
                call!(drawable, c"addPresentedHandler:", block as *const Block);
            }
            call!(commands, c"presentDrawable:", drawable);
            call!(commands, c"commit");
            if let Some(path) = &self.snapshot {
                call!(commands, c"waitUntilCompleted");
                snapshot::save_texture(texture, path);
            }
            if !quads.is_empty() {
                self.vertices[slot].commands = msg!(commands, c"retain");
            }
        }
        quads.clear();
        self.spare = quads;
    }

    /// Vertex buffer `slot`, holding at least `bytes`, once the GPU is done with it.
    ///
    /// # Safety
    /// Main thread, with `self.device` valid.
    unsafe fn vertex_buffer(&mut self, slot: usize, bytes: usize) -> Id {
        let vb = &mut self.vertices[slot];
        // SAFETY: Metal calls with the documented signatures; `commands`
        // and `buffer` are objects this renderer retained.
        unsafe {
            if !vb.commands.is_null() {
                const COMPLETED: usize = 4; // MTLCommandBufferStatusCompleted
                let status: usize = msg!(vb.commands, c"status");
                if status < COMPLETED {
                    call!(vb.commands, c"waitUntilCompleted");
                }
                call!(vb.commands, c"release");
                vb.commands = NIL;
            }
            if vb.bytes < bytes {
                if !vb.buffer.is_null() {
                    call!(vb.buffer, c"release");
                }
                vb.bytes = bytes.next_power_of_two();
                // Shared storage: written by the CPU, read by the GPU.
                vb.buffer = msg!(self.device, c"newBufferWithLength:options:", vb.bytes, 0usize);
            }
            vb.buffer
        }
    }

    fn build_quads(&mut self, snapshot: &Snapshot, selection: Option<Selection>) -> Vec<Quad> {
        let (cw, ch) = (self.font.cell_width as f32, self.font.cell_height as f32);
        let pad = self.padding_px() as f32;
        let thickness = (self.scale as f32).round().max(1.0);
        let baseline = self.font.baseline as f32;

        let (cols, rows) = (snapshot.cols, snapshot.rows);

        let mut back = mem::take(&mut self.spare);
        let mut front = mem::take(&mut self.front);
        back.clear();
        front.clear();
        for r in 0..rows {
            let y = pad + r as f32 * ch;
            let line = snapshot.first_line + r as u64;

            for c in 0..cols {
                let cell = &snapshot.cells[r * cols + c];
                let x = pad + c as f32 * cw;
                let wide = cell.flags.contains(Flags::WIDE);
                let w = if wide { 2.0 * cw } else { cw };
                let theme = &self.theme;
                let mut fg = rgba(theme.resolve(cell.fg, theme.foreground));
                let mut bg = rgba(theme.resolve(cell.bg, theme.background));
                let inverse = cell.flags.contains(Flags::INVERSE);
                if inverse {
                    mem::swap(&mut fg, &mut bg);
                }
                let selected = selection.is_some_and(|(start, end)| start <= (line, c) && (line, c) <= end);
                if selected {
                    bg = rgba(theme.selection_background);
                    if let Some(color) = theme.selection_foreground {
                        fg = rgba(color);
                    }
                }
                if selected || inverse || cell.bg != Color::Default {
                    back.push(Quad::solid(x, y, w, ch, bg));
                }

                if let Some((cursor_row, cursor_col, cursor_shape)) = snapshot.cursor
                    && r == cursor_row
                    && c == cursor_col
                {
                    let cursor = theme.cursor.map_or(fg, rgba);
                    match (self.focused, cursor_shape) {
                        (true, 0..=2) => {
                            back.push(Quad::solid(x, y, w, ch, cursor));
                            fg = theme.cursor_text.map_or(bg, rgba);
                        }
                        (true, 3 | 4) => {
                            back.push(Quad::solid(x, y + ch - 2.0 * thickness, w, 2.0 * thickness, cursor));
                        }
                        (true, _) => back.push(Quad::solid(x, y, 2.0 * thickness, ch, cursor)),
                        (false, _) => {
                            // Hollow block when the window isn't focused.
                            front.push(Quad::solid(x, y, w, thickness, cursor));
                            front.push(Quad::solid(x, y + ch - thickness, w, thickness, cursor));
                            front.push(Quad::solid(x, y, thickness, ch, cursor));
                            front.push(Quad::solid(x + w - thickness, y, thickness, ch, cursor));
                        }
                    }
                }

                if cell.flags.contains(Flags::DIM) {
                    fg[3] = 0.5;
                }
                if cell.flags.contains(Flags::UNDERLINE) {
                    front.push(Quad::solid(x, y + baseline + thickness, w, thickness, fg));
                }
                if cell.flags.contains(Flags::STRIKE) {
                    front.push(Quad::solid(x, y + (ch / 2.0).round(), w, thickness, fg));
                }

                let blank = cell.c == ' '
                    || cell.flags.contains(Flags::HIDDEN)
                    || cell.flags.contains(Flags::WIDE_SPACER)
                    || cell.flags.contains(Flags::WRAP_SPACER);
                if blank {
                    continue;
                }
                let style = Style::new(cell.flags.contains(Flags::BOLD), cell.flags.contains(Flags::ITALIC));
                let key = match snapshot.clusters.get(&(r * cols + c)) {
                    Some(text) => GlyphKey::Cluster(text.clone()),
                    None => GlyphKey::Char(cell.c),
                };
                if let Some(g) = self.glyph(key, style, if wide { 2 } else { 1 }) {
                    front.push(Quad {
                        pos: [x, y],
                        size: g.size,
                        uv: g.uv,
                        uv_size: g.size,
                        color: if g.color { [1.0, 1.0, 1.0, fg[3]] } else { fg },
                        mode: if g.color { BITMAP } else { MASK },
                        _pad: [0; 3],
                    });
                }
            }
        }
        back.append(&mut front);
        self.front = front;
        back
    }

    fn glyph(&mut self, key: GlyphKey, style: Style, cells: u32) -> Option<Glyph> {
        if let Some(&glyph) = self.glyphs.get(&(key.clone(), style)) {
            return glyph;
        }
        let c = match &key {
            GlyphKey::Char(c) => *c,
            GlyphKey::Cluster(text) => {
                // An atlas overflow returns early, uncached, as below.
                let glyph = match self.font.rasterize_cluster(text, style, cells) {
                    Some(raster) => Some(self.upload(&raster)?),
                    None => None,
                };
                self.glyphs.insert((key, style), glyph);
                return glyph;
            }
        };
        let (cw, ch) = self.cell_size();
        let stroke = (self.scale.round() as u32).max(1);
        // Box drawing and blocks are drawn from geometry so they join across cells.
        let raster = match boxdraw::draw(c, cw * cells, ch, stroke) {
            Some(mask) => Some(RasterGlyph {
                pixels: mask.iter().flat_map(|&a| [a; 4]).collect(),
                width: cw * cells,
                height: ch,
                color: false,
            }),
            None => self.font.rasterize(c, style, cells),
        };
        let glyph = match raster {
            Some(raster) => Some(self.upload(&raster)?),
            None => None,
        };
        self.glyphs.insert((key, style), glyph);
        glyph
    }

    /// Copies a tile into the atlas. On overflow the atlas is recycled and `None` returned.
    fn upload(&mut self, raster: &RasterGlyph) -> Option<Glyph> {
        let atlas = &mut self.atlas;
        if atlas.x + raster.width > atlas.size {
            atlas.x = 0;
            atlas.y += atlas.row_height;
            atlas.row_height = 0;
        }
        if atlas.y + raster.height > atlas.size {
            if atlas.size < ATLAS_MAX {
                atlas.size *= 2;
                // SAFETY: command buffers in flight keep their own reference.
                unsafe {
                    call!(atlas.texture, c"release");
                    atlas.texture = create_atlas(self.device, atlas.size);
                }
            }
            self.reset_atlas();
            self.atlas_reset = true;
            return None;
        }
        let (x, y) = (atlas.x, atlas.y);
        let region = MTLRegion {
            origin: MTLOrigin { x: x as usize, y: y as usize, z: 0 },
            size: MTLSize { width: raster.width as usize, height: raster.height as usize, depth: 1 },
        };
        // SAFETY: the region lies inside the texture and `pixels` holds width*height BGRA pixels.
        unsafe {
            call!(
                atlas.texture,
                c"replaceRegion:mipmapLevel:withBytes:bytesPerRow:",
                region,
                0usize,
                raster.pixels.as_ptr() as *const c_void,
                raster.width as usize * 4
            );
        }
        atlas.x += raster.width;
        atlas.row_height = atlas.row_height.max(raster.height);
        Some(Glyph {
            uv: [x as f32, y as f32],
            size: [raster.width as f32, raster.height as f32],
            color: raster.color,
        })
    }

    fn reset_atlas(&mut self) {
        self.glyphs.clear();
        self.atlas.x = 0;
        self.atlas.y = 0;
        self.atlas.row_height = 0;
    }
}

unsafe fn create_pipeline(device: Id) -> Id {
    unsafe {
        let mut error = NIL;
        let library: Id = msg!(device, c"newLibraryWithSource:options:error:", nsstring(SHADER), NIL, &mut error as *mut Id);
        if library.is_null() {
            panic!("shader compilation failed: {}", string_from_ns(msg!(error, c"localizedDescription")));
        }
        let vertex: Id = msg!(library, c"newFunctionWithName:", nsstring("vs"));
        let fragment: Id = msg!(library, c"newFunctionWithName:", nsstring("fs"));

        let desc = new_object(c"MTLRenderPipelineDescriptor");
        call!(desc, c"setVertexFunction:", vertex);
        call!(desc, c"setFragmentFunction:", fragment);
        let attachment: Id = msg!(msg!(desc, c"colorAttachments"), c"objectAtIndexedSubscript:", 0usize);
        call!(attachment, c"setPixelFormat:", PIXEL_FORMAT_BGRA8);
        // Premultiplied alpha: src + dst * (1 - src.a).
        call!(attachment, c"setBlendingEnabled:", YES);
        call!(attachment, c"setSourceRGBBlendFactor:", 1usize);
        call!(attachment, c"setDestinationRGBBlendFactor:", 5usize);
        call!(attachment, c"setSourceAlphaBlendFactor:", 1usize);
        call!(attachment, c"setDestinationAlphaBlendFactor:", 5usize);

        let pipeline: Id = msg!(device, c"newRenderPipelineStateWithDescriptor:error:", desc, &mut error as *mut Id);
        if pipeline.is_null() {
            panic!("pipeline creation failed: {}", string_from_ns(msg!(error, c"localizedDescription")));
        }
        for object in [desc, vertex, fragment, library] {
            call!(object, c"release");
        }
        pipeline
    }
}

unsafe fn create_atlas(device: Id, size: u32) -> Id {
    unsafe {
        let desc: Id = msg!(
            class(c"MTLTextureDescriptor"),
            c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:",
            PIXEL_FORMAT_BGRA8,
            size as usize,
            size as usize,
            NO
        );
        call!(desc, c"setUsage:", 1usize); // shader read
        msg!(device, c"newTextureWithDescriptor:", desc)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: objects this renderer created or retained, released once;
        // command buffers still in flight keep their own references.
        unsafe {
            for vb in self.vertices {
                for object in [vb.commands, vb.buffer] {
                    if !object.is_null() {
                        call!(object, c"release");
                    }
                }
            }
            for object in [self.atlas.texture, self.pipeline, self.queue, self.device] {
                call!(object, c"release");
            }
        }
    }
}

fn rgba((r, g, b): Rgb) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}
