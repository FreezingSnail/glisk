//! Explicit-geometry viewport painting, scrolling, and resize anchoring.
//!
//! The viewport owns one live [`BaseDocument`]. It paints only the visible
//! rectangle at a bounded physical size, clamps vertical scroll to the
//! document range, and anchors a resize to the top visible content block so
//! the reading position survives reflow. Focus, open disclosures, and every
//! other DOM state stay on the same live document because nothing here
//! rebuilds it.
//!
//! Geometry is passed in explicitly (physical pixels, scale, and the derived
//! CSS viewport). Wiring code maps the terminal owner's `Geometry` into this
//! API; tests can exercise painting and scrolling without a terminal.
//!
//! Integration note: this module is not wired into `src/main.rs` yet. See
//! `docs/viewport.md` for the exact calls the wiring bead adds.

use anyrender_vello_cpu::VelloCpuScenePainter;
use blitz_dom::{BaseDocument, NodeId};
use blitz_traits::shell::{ColorScheme, Viewport as BlitzViewport};

/// Smallest physical raster edge accepted for painting.
pub const MIN_RASTER_EDGE: u32 = 2;
/// Provisional v0 raster cap from `FEASIBILITY.md`.
pub const MAX_RASTER_PIXELS: u32 = 8_388_608;
/// Provisional v0 encoded PNG cap from `FEASIBILITY.md`.
pub const MAX_PNG_BYTES: usize = 4 * 1024 * 1024;

/// Physical, CSS, and scale budgets for one viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportLimits {
    pub max_raster_pixels: u32,
    pub max_png_bytes: usize,
}

impl Default for ViewportLimits {
    fn default() -> Self {
        Self {
            max_raster_pixels: MAX_RASTER_PIXELS,
            max_png_bytes: MAX_PNG_BYTES,
        }
    }
}

/// Rejection reason for a physical raster before any allocation.
#[derive(Clone, Debug, PartialEq)]
pub enum GeometryProblem {
    /// A raster edge is zero.
    Empty { width: u32, height: u32 },
    /// A raster edge is below [`MIN_RASTER_EDGE`].
    TooSmall {
        width: u32,
        height: u32,
        minimum: u32,
    },
    /// A raster edge cannot be represented by the painter's `u16` surface.
    DimensionTooLarge { width: u32, height: u32 },
    /// Physical pixels exceed the configured cap.
    RasterBudget {
        width: u32,
        height: u32,
        pixels: u64,
        limit: u32,
    },
    /// Scale is not finite and positive.
    InvalidScale { scale: f64 },
    /// Derived CSS viewport is not finite and positive.
    InvalidCss { css_width: f64, css_height: f64 },
}

impl std::fmt::Display for GeometryProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty { width, height } => {
                write!(f, "empty raster {width}x{height}")
            }
            Self::TooSmall {
                width,
                height,
                minimum,
            } => write!(
                f,
                "raster {width}x{height} is below the {minimum}px minimum edge"
            ),
            Self::DimensionTooLarge { width, height } => {
                write!(f, "raster {width}x{height} exceeds the u16 surface limit")
            }
            Self::RasterBudget {
                width,
                height,
                pixels,
                limit,
            } => write!(
                f,
                "raster {width}x{height} ({pixels} pixels) exceeds {limit} pixels"
            ),
            Self::InvalidScale { scale } => write!(f, "invalid device scale {scale}"),
            Self::InvalidCss {
                css_width,
                css_height,
            } => write!(f, "invalid CSS viewport {css_width}x{css_height}"),
        }
    }
}

impl std::error::Error for GeometryProblem {}

/// Validated explicit geometry: physical raster, derived CSS viewport, scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewGeometry {
    physical_width: u32,
    physical_height: u32,
    css_width: f64,
    css_height: f64,
    scale: f64,
}

impl ViewGeometry {
    /// Validate every dimension before a frame allocation.
    pub fn new(
        physical_width: u32,
        physical_height: u32,
        scale: f64,
        limits: &ViewportLimits,
    ) -> Result<Self, GeometryProblem> {
        if physical_width == 0 || physical_height == 0 {
            return Err(GeometryProblem::Empty {
                width: physical_width,
                height: physical_height,
            });
        }
        if physical_width < MIN_RASTER_EDGE || physical_height < MIN_RASTER_EDGE {
            return Err(GeometryProblem::TooSmall {
                width: physical_width,
                height: physical_height,
                minimum: MIN_RASTER_EDGE,
            });
        }
        if physical_width > u32::from(u16::MAX) || physical_height > u32::from(u16::MAX) {
            return Err(GeometryProblem::DimensionTooLarge {
                width: physical_width,
                height: physical_height,
            });
        }
        let pixels = u64::from(physical_width) * u64::from(physical_height);
        if pixels > u64::from(limits.max_raster_pixels) {
            return Err(GeometryProblem::RasterBudget {
                width: physical_width,
                height: physical_height,
                pixels,
                limit: limits.max_raster_pixels,
            });
        }
        if !scale.is_finite() || scale <= 0.0 {
            return Err(GeometryProblem::InvalidScale { scale });
        }
        let blitz_scale = scale as f32;
        if !blitz_scale.is_finite() || blitz_scale <= 0.0 {
            return Err(GeometryProblem::InvalidScale { scale });
        }
        let css_width = f64::from(physical_width) / scale;
        let css_height = f64::from(physical_height) / scale;
        if !css_width.is_finite()
            || !css_height.is_finite()
            || css_width <= 0.0
            || css_height <= 0.0
        {
            return Err(GeometryProblem::InvalidCss {
                css_width,
                css_height,
            });
        }
        Ok(Self {
            physical_width,
            physical_height,
            css_width,
            css_height,
            scale,
        })
    }

    pub fn physical_size(self) -> (u32, u32) {
        (self.physical_width, self.physical_height)
    }

    pub fn css_size(self) -> (f64, f64) {
        (self.css_width, self.css_height)
    }

    pub fn scale(self) -> f64 {
        self.scale
    }

    fn blitz(self) -> BlitzViewport {
        BlitzViewport::new(
            self.physical_width,
            self.physical_height,
            self.scale as f32,
            ColorScheme::Dark,
        )
    }
}

/// One encoded viewport frame.
#[derive(Clone, Debug)]
pub struct Frame {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Physical pixels in the raster before encoding.
    pub pixels: u64,
    pub scale: f64,
    /// CSS scroll offset painted into this frame.
    pub scroll_y: f64,
}

/// Why a paint was skipped instead of allocating a raster.
#[derive(Clone, Debug, PartialEq)]
pub enum SkipReason {
    /// No valid geometry is currently available.
    NoGeometry,
    /// The last geometry was rejected; painting waits for a valid one.
    Geometry(GeometryProblem),
    /// The document has no root element to paint.
    EmptyDocument,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGeometry => f.write_str("no viewport geometry"),
            Self::Geometry(problem) => problem.fmt(f),
            Self::EmptyDocument => f.write_str("document has no root element"),
        }
    }
}

/// Paint result: either one bounded PNG frame or an explicit skip.
#[derive(Debug)]
pub enum PaintOutcome {
    Frame(Frame),
    Skipped(SkipReason),
}

/// Failure after a valid raster was allocated.
#[derive(Debug)]
pub enum PaintError {
    /// The encoded PNG exceeded the configured frame budget.
    PngBudget { limit: usize, observed: usize },
    /// The PNG encoder failed.
    Encode(String),
}

impl std::fmt::Display for PaintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PngBudget { limit, observed } => write!(
                f,
                "encoded frame is {observed} bytes, above the {limit}-byte cap"
            ),
            Self::Encode(detail) => write!(f, "PNG encode failed: {detail}"),
        }
    }
}

impl std::error::Error for PaintError {}

/// A block visible at the top of the viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibleBlock {
    pub node: NodeId,
    /// Document-relative CSS top of the block.
    pub top: f64,
    /// `scroll_y - top`; negative when the block starts below the viewport top.
    pub offset: f64,
}

/// Stable resize anchor captured before reflow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    /// Top visible content block, if the document has one.
    pub block: Option<NodeId>,
    /// Nearest surviving preceding heading, used if `block` disappears.
    pub heading: Option<NodeId>,
    /// Signed CSS offset from the block top to the viewport top.
    pub offset: f64,
    /// The viewport was at the bottom; keep it at the bottom after reflow.
    pub at_bottom: bool,
}

/// Result of applying a valid resize.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeInfo {
    pub scroll_y: f64,
    pub max_scroll: f64,
    pub anchored: bool,
    /// The block disappeared and the preceding heading was used.
    pub used_heading_fallback: bool,
}

/// Resize outcome: applied, or skipped while geometry is invalid.
#[derive(Clone, Debug, PartialEq)]
pub enum ResizeOutcome {
    Applied(ResizeInfo),
    Skipped(GeometryProblem),
}

#[derive(Clone, Copy, Debug)]
struct Block {
    node: NodeId,
    top: f64,
    height: f64,
    heading: bool,
}

/// One live document plus viewport state.
pub struct Viewport {
    document: BaseDocument,
    geometry: Option<ViewGeometry>,
    problem: Option<GeometryProblem>,
    pending_anchor: Option<Anchor>,
    scroll_y: f64,
    limits: ViewportLimits,
}

impl Viewport {
    /// Attach a live document and apply `width`x`height` at `scale`.
    ///
    /// Invalid or tiny geometry leaves the viewport without a raster: every
    /// paint is skipped until a later [`Viewport::resize`] supplies valid
    /// geometry. The document is never rebuilt.
    pub fn new(document: BaseDocument, width: u32, height: u32, scale: f64) -> Self {
        let mut viewport = Self {
            scroll_y: document.viewport_scroll().y,
            document,
            geometry: None,
            problem: None,
            pending_anchor: None,
            limits: ViewportLimits::default(),
        };
        match ViewGeometry::new(width, height, scale, &viewport.limits) {
            Ok(geometry) => {
                viewport.geometry = Some(geometry);
                viewport.apply_geometry(geometry);
                viewport.set_scroll(viewport.scroll_y);
            }
            Err(problem) => viewport.problem = Some(problem),
        }
        viewport
    }

    /// Override raster and encoded-frame budgets.
    pub fn with_limits(mut self, limits: ViewportLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn document(&self) -> &BaseDocument {
        &self.document
    }

    /// Mutable access for the pager's DOM actions (focus, disclosures, links).
    /// Reflow state is retained; call [`Viewport::resize`] after a mutation
    /// that changes layout, or let the next paint resolve it.
    pub fn document_mut(&mut self) -> &mut BaseDocument {
        &mut self.document
    }

    pub fn limits(&self) -> ViewportLimits {
        self.limits
    }

    pub fn geometry(&self) -> Option<ViewGeometry> {
        self.geometry
    }

    /// Current CSS vertical scroll offset.
    pub fn scroll_y(&self) -> f64 {
        self.scroll_y
    }

    /// Maximum CSS vertical scroll offset for the current layout.
    pub fn max_scroll(&mut self) -> f64 {
        match self.geometry {
            Some(geometry) => {
                self.document.resolve(0.0);
                self.max_scroll_for(geometry)
            }
            None => 0.0,
        }
    }

    /// Scroll by a CSS-pixel delta, clamped to the document range.
    pub fn scroll_by(&mut self, delta_css: f64) -> bool {
        if !delta_css.is_finite() {
            return false;
        }
        let target = self.scroll_y + delta_css;
        self.scroll_to(target)
    }

    /// Scroll by whole or fractional CSS viewport heights.
    pub fn scroll_page(&mut self, pages: f64) -> bool {
        let Some(geometry) = self.geometry else {
            return false;
        };
        if !pages.is_finite() {
            return false;
        }
        self.scroll_by(geometry.css_height * pages)
    }

    pub fn scroll_home(&mut self) -> bool {
        self.scroll_to(0.0)
    }

    pub fn scroll_end(&mut self) -> bool {
        let target = self.max_scroll();
        self.scroll_to(target)
    }

    /// Scroll so the viewport top sits at `target_css`, clamped.
    pub fn scroll_to(&mut self, target_css: f64) -> bool {
        if !target_css.is_finite() {
            return false;
        }
        let Some(geometry) = self.geometry else {
            return false;
        };
        self.document.resolve(0.0);
        self.set_scroll(target_css.clamp(0.0, self.max_scroll_for(geometry)))
    }

    /// Resize and anchor to the currently visible block.
    pub fn resize(&mut self, width: u32, height: u32, scale: f64) -> ResizeOutcome {
        let anchor = self.pending_anchor.take().or_else(|| self.capture_anchor());
        self.resize_with_anchor(width, height, scale, anchor)
    }

    /// Resize with an already captured anchor.
    ///
    /// A caller that mutates the DOM between capture and reflow (for example
    /// removing content) passes the captured value here so the fallback policy
    /// can run. Invalid geometry stores the anchor until a valid resize.
    pub fn resize_with_anchor(
        &mut self,
        width: u32,
        height: u32,
        scale: f64,
        anchor: Option<Anchor>,
    ) -> ResizeOutcome {
        let geometry = match ViewGeometry::new(width, height, scale, &self.limits) {
            Ok(geometry) => geometry,
            Err(problem) => {
                self.pending_anchor = anchor;
                self.geometry = None;
                self.problem = Some(problem.clone());
                return ResizeOutcome::Skipped(problem);
            }
        };
        self.geometry = Some(geometry);
        self.problem = None;
        self.pending_anchor = None;
        self.apply_geometry(geometry);
        let info = self.apply_anchor(anchor, geometry);
        ResizeOutcome::Applied(info)
    }

    /// Capture the top visible content block and its offset for a later reflow.
    pub fn capture_anchor(&mut self) -> Option<Anchor> {
        let geometry = self.geometry?;
        self.document.resolve(0.0);
        let blocks = collect_blocks(&self.document);
        let selected = self.select_visible_block(&blocks)?;
        let index = blocks
            .iter()
            .position(|block| block.node == selected.node)?;
        let heading = blocks[..index]
            .iter()
            .rev()
            .find(|block| block.heading)
            .map(|block| block.node);
        let max_scroll = self.max_scroll_for(geometry);
        Some(Anchor {
            block: Some(selected.node),
            heading,
            offset: self.scroll_y - selected.top,
            at_bottom: max_scroll > 0.0 && self.scroll_y >= max_scroll - 0.5,
        })
    }

    /// The top visible content block for the current layout.
    pub fn visible_block(&mut self) -> Option<VisibleBlock> {
        self.geometry?;
        self.document.resolve(0.0);
        let blocks = collect_blocks(&self.document);
        self.select_visible_block(&blocks)
    }

    /// Paint the current viewport into one bounded PNG.
    pub fn paint(&mut self) -> Result<PaintOutcome, PaintError> {
        let Some(geometry) = self.geometry else {
            return Ok(PaintOutcome::Skipped(
                self.problem
                    .clone()
                    .map(SkipReason::Geometry)
                    .unwrap_or(SkipReason::NoGeometry),
            ));
        };
        // Re-validate the stored geometry against the current limits: a caller
        // may tighten limits after construction, and allocation must still be
        // bounded by the policy in force now.
        let (width, height) = geometry.physical_size();
        let geometry = match ViewGeometry::new(width, height, geometry.scale, &self.limits) {
            Ok(geometry) => geometry,
            Err(problem) => {
                return Ok(PaintOutcome::Skipped(SkipReason::Geometry(problem)));
            }
        };
        if self.document.try_root_element().is_none() {
            return Ok(PaintOutcome::Skipped(SkipReason::EmptyDocument));
        }

        self.document.resolve(0.0);
        self.set_scroll(self.scroll_y.clamp(0.0, self.max_scroll_for(geometry)));

        let (width, height) = geometry.physical_size();
        let mut painter = VelloCpuScenePainter::new(width as u16, height as u16);
        blitz_paint::paint_scene(
            &mut painter,
            &mut self.document,
            geometry.scale,
            width,
            height,
            0,
            0,
        );
        let pixmap = painter.finish();
        let png = pixmap
            .into_png()
            .map_err(|err| PaintError::Encode(format!("{err:?}")))?;
        if png.len() > self.limits.max_png_bytes {
            return Err(PaintError::PngBudget {
                limit: self.limits.max_png_bytes,
                observed: png.len(),
            });
        }
        Ok(PaintOutcome::Frame(Frame {
            png,
            width,
            height,
            pixels: u64::from(width) * u64::from(height),
            scale: geometry.scale,
            scroll_y: self.scroll_y,
        }))
    }

    fn apply_geometry(&mut self, geometry: ViewGeometry) {
        self.document.set_viewport(geometry.blitz());
        self.document.resolve(0.0);
    }

    fn apply_anchor(&mut self, anchor: Option<Anchor>, geometry: ViewGeometry) -> ResizeInfo {
        let max_scroll = self.max_scroll_for(geometry);
        let mut target = self.scroll_y;
        let mut anchored = false;
        let mut used_heading_fallback = false;

        if let Some(anchor) = anchor {
            if anchor.at_bottom {
                target = max_scroll;
                anchored = true;
            } else if let Some(block) = anchor.block
                && let Some(top) = document_top(&self.document, block)
            {
                target = top + anchor.offset;
                anchored = true;
            } else if let Some(heading) = anchor.heading
                && let Some(top) = document_top(&self.document, heading)
            {
                target = top;
                anchored = true;
                used_heading_fallback = true;
            }
        }

        let target = if target.is_finite() {
            target.clamp(0.0, max_scroll)
        } else {
            0.0
        };
        self.set_scroll(target);
        ResizeInfo {
            scroll_y: self.scroll_y,
            max_scroll,
            anchored,
            used_heading_fallback,
        }
    }

    fn select_visible_block(&self, blocks: &[Block]) -> Option<VisibleBlock> {
        let scroll = self.scroll_y;
        let selected = blocks
            .iter()
            .find(|block| block.top + block.height > scroll + f64::EPSILON)
            .or_else(|| blocks.last())?;
        Some(VisibleBlock {
            node: selected.node,
            top: selected.top,
            offset: scroll - selected.top,
        })
    }

    fn max_scroll_for(&self, geometry: ViewGeometry) -> f64 {
        let Some(root) = self.document.try_root_element() else {
            return 0.0;
        };
        let layout = root.final_layout();
        let content_height = f64::from(
            layout
                .size
                .height
                .max(layout.scrollable_overflow_rect.bottom),
        );
        if !content_height.is_finite() {
            return 0.0;
        }
        (content_height - geometry.css_height).max(0.0)
    }

    fn set_scroll(&mut self, scroll: f64) -> bool {
        let scroll = if scroll.is_finite() {
            scroll.max(0.0)
        } else {
            0.0
        };
        let changed = (scroll - self.scroll_y).abs() > f64::EPSILON;
        self.scroll_y = scroll;
        self.document
            .set_viewport_scroll(blitz_dom::Point { x: 0.0, y: scroll });
        changed
    }
}

fn document_top(document: &BaseDocument, node: NodeId) -> Option<f64> {
    let node = document.get_node(node)?;
    if !node.flags.is_in_document() || node.parent.is_none() {
        return None;
    }
    Some(f64::from(node.absolute_position(0.0, 0.0).y))
}

fn collect_blocks(document: &BaseDocument) -> Vec<Block> {
    let Some(root) = document.try_root_element() else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    visit(document, root.id, &mut blocks);
    blocks
}

/// Post-order walk: a block is a leaf candidate, so a wrapper never competes
/// with the paragraph or heading it contains. Returns whether this subtree
/// contains a candidate.
fn visit(document: &BaseDocument, id: NodeId, blocks: &mut Vec<Block>) -> bool {
    let Some(node) = document.get_node(id) else {
        return false;
    };
    let children = node.children.clone();
    let mut child_block = false;
    for child in children {
        child_block |= visit(document, child, blocks);
    }

    let Some(tag) = node
        .element_data()
        .map(|element| element.name.local.as_ref())
    else {
        return false;
    };
    let candidate = is_block_tag(tag);
    if candidate && !child_block {
        let top = f64::from(node.absolute_position(0.0, 0.0).y);
        let height = f64::from(node.final_layout().size.height);
        if top.is_finite() && height.is_finite() {
            blocks.push(Block {
                node: id,
                top,
                height,
                heading: is_heading(tag),
            });
        }
    }
    candidate || child_block
}

fn is_heading(tag: &str) -> bool {
    matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "body"
            | "main"
            | "article"
            | "section"
            | "aside"
            | "header"
            | "footer"
            | "nav"
            | "div"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "dt"
            | "dd"
            | "ul"
            | "ol"
            | "pre"
            | "blockquote"
            | "figure"
            | "figcaption"
            | "table"
            | "caption"
            | "td"
            | "th"
            | "summary"
            | "details"
    )
}
