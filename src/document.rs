//! Shared bounded document ingestion and offline resource policy.
//!
//! File input, stdin input, and later text/graphics output all enter through
//! this module so one policy governs every document. The contract is one
//! self-contained HTML file with inline CSS, system fonts, static inline SVG,
//! and bounded `data:image/png` images. Network requests, filesystem reads,
//! subdocuments, scripts, form submission, and navigation away from the
//! document are denied.
//!
//! Budgets follow `FEASIBILITY.md#offline-authoring-and-resource-limits`:
//!
//! | Budget | Limit |
//! |---|---:|
//! | HTML input | 4 MiB |
//! | Parsed document | 40,000 nodes and depth 128 |
//! | Embedded PNGs | 8 million pixels per image and 16 million total |
//!
//! Integration note: this module deliberately has no dependency on the
//! spike's terminal code. The wiring bead adds `mod document;` next to the
//! existing `mod` declarations and replaces its `read_to_string` plus
//! `HtmlDocument::from_html` block with [`ingest_file`] or [`ingest_stdin`].

use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::Path,
    sync::{Arc, Mutex},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use blitz_dom::{BaseDocument, DocumentConfig, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::{
    navigation::{NavigationOptions, NavigationProvider},
    net::{Bytes, NetHandler, NetProvider, Request, Url},
    shell::Viewport,
};

/// Input byte cap applied before UTF-8 validation.
pub const MAX_HTML_BYTES: usize = 4 * 1024 * 1024;
/// Maximum live DOM nodes after parsing, checked before layout.
pub const MAX_DOM_NODES: usize = 40_000;
/// Maximum node depth below the document root, checked before layout.
pub const MAX_DOM_DEPTH: usize = 128;
/// Maximum decoded pixels for one embedded PNG.
pub const MAX_IMAGE_PIXELS: u64 = 8_000_000;
/// Maximum decoded pixels for all embedded PNGs in one document.
pub const MAX_TOTAL_IMAGE_PIXELS: u64 = 16_000_000;
/// Synthetic base URL used to resolve relative URLs into a denied scheme.
pub const DOCUMENT_BASE_URL: &str = "glisk://document/index.html";

/// Denial: document-directed network access.
pub const DENIED_NETWORK: &str = "network access denied";
/// Denial: document-directed filesystem access.
pub const DENIED_FILESYSTEM: &str = "filesystem access denied";
/// Denial: a scheme outside the authoring contract.
pub const DENIED_RESOURCE_SCHEME: &str = "resource scheme denied";
/// Denial: a `data:` resource other than `data:image/png`.
pub const DENIED_DATA_TYPE: &str = "unsupported data URL type";
/// Denial: malformed or undecodable `data:` payload.
pub const DENIED_MALFORMED_DATA: &str = "malformed data URL";
/// Denial: PNG header missing or inconsistent.
pub const DENIED_INVALID_PNG: &str = "invalid PNG header";
/// Denial: one image exceeds the per-image pixel cap.
pub const DENIED_IMAGE_BUDGET: &str = "image pixel budget exceeded";
/// Denial: the document image pixel budget is exhausted.
pub const DENIED_TOTAL_BUDGET: &str = "document image pixel budget exceeded";
/// Denial: navigation away from the current document.
pub const DENIED_NAVIGATION: &str = "navigation denied";
/// Denial: an inline SVG reference that is not a local fragment.
pub const DENIED_SVG_REFERENCE: &str = "SVG reference denied";
/// Denial: a subdocument carried inline by `srcdoc`.
pub const DENIED_SUBDOCUMENT: &str = "subdocument denied";

/// Resource caps for one document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub html_bytes: usize,
    pub dom_nodes: usize,
    pub dom_depth: usize,
    pub image_pixels: u64,
    pub total_image_pixels: u64,
}

/// Provisional v0 limits from `FEASIBILITY.md`.
pub const DEFAULT_LIMITS: Limits = Limits {
    html_bytes: MAX_HTML_BYTES,
    dom_nodes: MAX_DOM_NODES,
    dom_depth: MAX_DOM_DEPTH,
    image_pixels: MAX_IMAGE_PIXELS,
    total_image_pixels: MAX_TOTAL_IMAGE_PIXELS,
};

impl Default for Limits {
    fn default() -> Self {
        DEFAULT_LIMITS
    }
}

/// A document rejected before it can enter the rendering pipeline.
#[derive(Debug)]
pub enum IngestError {
    /// Reading the input failed.
    Read(io::Error),
    /// Input exceeded the byte cap. `observed` is at least `limit + 1`.
    TooLarge { limit: usize, observed: usize },
    /// Input was not valid UTF-8.
    InvalidUtf8 { valid_up_to: usize },
    /// Parsed DOM exceeded the node cap.
    NodeBudget { limit: usize, observed: usize },
    /// Parsed DOM exceeded the depth cap.
    DepthBudget { limit: usize, observed: usize },
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(err) => write!(f, "cannot read document: {err}"),
            Self::TooLarge { limit, observed } => write!(
                f,
                "document input exceeds {limit}-byte cap (read at least {observed} bytes)"
            ),
            Self::InvalidUtf8 { valid_up_to } => {
                write!(f, "document input is not valid UTF-8 at byte {valid_up_to}")
            }
            Self::NodeBudget { limit, observed } => write!(
                f,
                "document has {observed} nodes, above the {limit}-node cap"
            ),
            Self::DepthBudget { limit, observed } => {
                write!(f, "document depth {observed} exceeds the {limit}-level cap")
            }
        }
    }
}

impl std::error::Error for IngestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for IngestError {
    fn from(err: io::Error) -> Self {
        Self::Read(err)
    }
}

/// Parsed DOM size used for the pre-layout budget check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DomStats {
    /// Live nodes, including the document root and text nodes.
    pub nodes: usize,
    /// Deepest node level below the document root (root is level 0).
    pub depth: usize,
}

/// One denied resource, retained for diagnostics and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeniedResource {
    pub url: String,
    pub reason: &'static str,
}

/// A document that passed the input, DOM, and resource policy.
pub struct IngestedDocument {
    pub document: BaseDocument,
    pub policy: Arc<ResourcePolicy>,
    pub stats: DomStats,
}

/// Per-document policy state shared by the network and navigation providers.
#[derive(Debug)]
pub struct ResourcePolicy {
    limits: Limits,
    state: Mutex<PolicyState>,
}

#[derive(Debug, Default)]
struct PolicyState {
    denials: Vec<DeniedResource>,
    admitted_images: u64,
    admitted_image_pixels: u64,
}

impl ResourcePolicy {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            state: Mutex::new(PolicyState::default()),
        }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Resources denied so far, in encounter order.
    pub fn denials(&self) -> Vec<DeniedResource> {
        self.lock().denials.clone()
    }

    /// Embedded PNGs admitted to the image decoder.
    pub fn admitted_images(&self) -> u64 {
        self.lock().admitted_images
    }

    /// Pixels charged against the document budget.
    pub fn admitted_image_pixels(&self) -> u64 {
        self.lock().admitted_image_pixels
    }

    pub fn denial_reasons(&self) -> Vec<&'static str> {
        self.lock().denials.iter().map(|d| d.reason).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PolicyState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn record(&self, url: &str, reason: &'static str) {
        self.lock().denials.push(DeniedResource {
            url: truncate_url(url),
            reason,
        });
    }

    /// Validate PNG dimensions, then charge the document budget before the
    /// bytes reach Blitz's decoder.
    fn admit_png(&self, bytes: &[u8]) -> Result<(), &'static str> {
        let (width, height) = png_dimensions(bytes).ok_or(DENIED_INVALID_PNG)?;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(DENIED_IMAGE_BUDGET)?;
        if pixels > self.limits.image_pixels {
            return Err(DENIED_IMAGE_BUDGET);
        }
        let mut state = self.lock();
        let total = state
            .admitted_image_pixels
            .checked_add(pixels)
            .ok_or(DENIED_TOTAL_BUDGET)?;
        if total > self.limits.total_image_pixels {
            return Err(DENIED_TOTAL_BUDGET);
        }
        state.admitted_image_pixels = total;
        state.admitted_images += 1;
        Ok(())
    }
}

fn truncate_url(url: &str) -> String {
    const MAX_URL_CHARS: usize = 256;
    if url.len() <= MAX_URL_CHARS {
        return url.to_string();
    }
    let mut end = MAX_URL_CHARS;
    while !url.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &url[..end])
}

/// Network provider that admits only bounded embedded PNGs.
///
/// Statically allowed resources (inline CSS, inline SVG, system fonts) never
/// reach this provider. Every URL that does reach it is either a
/// `data:image/png` payload within budget or a denial.
#[derive(Clone, Debug)]
pub struct PolicyNetProvider(pub Arc<ResourcePolicy>);

impl NetProvider for PolicyNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url;
        match decode_embedded_png(&url) {
            Ok(bytes) => match self.0.admit_png(&bytes) {
                Ok(()) => handler.bytes(url.to_string(), Bytes::from(bytes)),
                Err(reason) => self.0.record(url.as_str(), reason),
            },
            Err(reason) => self.0.record(url.as_str(), reason),
        }
    }

    /// The only admitted resource is decoded synchronously here, so Blitz must
    /// not hold the document open waiting on a callback. Denied stylesheets
    /// therefore never block the first paint.
    fn is_noop(&self) -> bool {
        true
    }
}

/// Navigation provider that denies every document-directed navigation.
///
/// Local fragment links are handled inside Blitz and never reach this
/// provider; external links, form submission, and subdocument navigation do.
#[derive(Clone, Debug)]
pub struct PolicyNavigationProvider(pub Arc<ResourcePolicy>);

impl NavigationProvider for PolicyNavigationProvider {
    fn navigate_to(&self, options: NavigationOptions) {
        self.0.record(options.url.as_str(), DENIED_NAVIGATION);
    }
}

/// The synthetic base URL every document resolves against.
pub fn document_base_url() -> Url {
    Url::parse(DOCUMENT_BASE_URL).expect("DOCUMENT_BASE_URL is a valid URL")
}

/// Whether `target` differs from `base` only by its fragment.
pub fn is_local_fragment(base: &Url, target: &Url) -> bool {
    target.fragment().is_some()
        && base.scheme() == target.scheme()
        && base.host_str() == target.host_str()
        && base.port_or_known_default() == target.port_or_known_default()
        && base.path() == target.path()
        && base.query() == target.query()
}

/// Read at most `limit` bytes from `reader`, then validate UTF-8.
///
/// Reads `limit + 1` bytes so an oversized input is reported without reading
/// the entire source.
pub fn read_bounded(reader: &mut impl Read, limit: usize) -> Result<String, IngestError> {
    let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::new();
    reader.by_ref().take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(IngestError::TooLarge {
            limit,
            observed: bytes.len(),
        });
    }
    String::from_utf8(bytes).map_err(|err| IngestError::InvalidUtf8 {
        valid_up_to: err.utf8_error().valid_up_to(),
    })
}

/// Read one file through the bounded, UTF-8-validating input path.
pub fn read_html_file(path: impl AsRef<Path>, limits: &Limits) -> Result<String, IngestError> {
    let mut file = File::open(path)?;
    read_bounded(&mut file, limits.html_bytes)
}

/// Compute node count and depth for a parsed document.
pub fn dom_stats(document: &BaseDocument) -> DomStats {
    let nodes = document.tree().len();
    let mut depth = 0;
    let mut stack = vec![(document.root_node().id, 0usize)];
    while let Some((id, level)) = stack.pop() {
        depth = depth.max(level);
        if let Some(node) = document.get_node(id) {
            for &child in &node.children {
                stack.push((child, level + 1));
            }
        }
    }
    DomStats { nodes, depth }
}

/// Enforce the DOM caps before style, layout, or semantic walks.
pub fn check_dom_budget(document: &BaseDocument, limits: &Limits) -> Result<DomStats, IngestError> {
    let stats = dom_stats(document);
    if stats.nodes > limits.dom_nodes {
        return Err(IngestError::NodeBudget {
            limit: limits.dom_nodes,
            observed: stats.nodes,
        });
    }
    if stats.depth > limits.dom_depth {
        return Err(IngestError::DepthBudget {
            limit: limits.dom_depth,
            observed: stats.depth,
        });
    }
    Ok(stats)
}

/// Parse, budget-check, sanitize, and lay out one in-memory HTML document.
pub fn open_document(
    html: &str,
    viewport: Viewport,
    limits: Limits,
) -> Result<IngestedDocument, IngestError> {
    let policy = Arc::new(ResourcePolicy::new(limits));
    let config = DocumentConfig {
        viewport: Some(viewport),
        base_url: Some(DOCUMENT_BASE_URL.to_string()),
        net_provider: Some(Arc::new(PolicyNetProvider(Arc::clone(&policy)))),
        navigation_provider: Some(Arc::new(PolicyNavigationProvider(Arc::clone(&policy)))),
        subdocument_depth: 0,
        ..Default::default()
    };
    let mut document = HtmlDocument::from_html(html, config).into_inner();
    let stats = check_dom_budget(&document, &limits)?;
    deny_external_svg_references(&mut document, &policy);
    deny_inline_subdocuments(&mut document, &policy);
    document.resolve(0.0);
    Ok(IngestedDocument {
        document,
        policy,
        stats,
    })
}

/// Read and open one file source.
pub fn ingest_file(
    path: impl AsRef<Path>,
    viewport: Viewport,
    limits: Limits,
) -> Result<IngestedDocument, IngestError> {
    let mut file = File::open(path)?;
    ingest_reader(&mut file, viewport, limits)
}

/// Read and open stdin as one source.
pub fn ingest_stdin(viewport: Viewport, limits: Limits) -> Result<IngestedDocument, IngestError> {
    let stdin = io::stdin();
    let mut lock = stdin.lock();
    ingest_reader(&mut lock, viewport, limits)
}

/// Read and open any byte source through the shared entry point.
pub fn ingest_reader(
    reader: &mut impl Read,
    viewport: Viewport,
    limits: Limits,
) -> Result<IngestedDocument, IngestError> {
    let html = read_bounded(reader, limits.html_bytes)?;
    open_document(&html, viewport, limits)
}

/// Strip every non-fragment `href` from inline SVG `image`, `use`, and
/// `feImage` elements. Inline SVG may reference fragments in the current SVG;
/// the pinned renderer is not allowed to resolve anything else.
fn deny_external_svg_references(document: &mut BaseDocument, policy: &ResourcePolicy) {
    let targets: Vec<_> = document
        .tree()
        .iter()
        .filter_map(|(id, node)| {
            let element = node.element_data()?;
            let local = &element.name.local;
            if !(local == &local_name!("image")
                || local == &local_name!("use")
                || local == &local_name!("feImage"))
            {
                return None;
            }
            let names: Vec<_> = element
                .attrs
                .iter()
                .filter(|attr| attr.name.local == local_name!("href"))
                .map(|attr| attr.name.clone())
                .collect();
            (!names.is_empty()).then_some((id, names))
        })
        .collect();

    for (id, names) in targets {
        let Some(element) = document
            .get_node_mut(id)
            .and_then(|node| node.element_data_mut())
        else {
            continue;
        };
        for name in names {
            let Some(value) = element
                .attrs
                .iter()
                .find(|attr| attr.name == name)
                .map(|attr| attr.value.clone())
            else {
                continue;
            };
            if !value.starts_with('#') {
                policy.record(&value, DENIED_SVG_REFERENCE);
                element.attrs.remove(&name);
            }
        }
    }
}

/// Remove `srcdoc` so an `<iframe>` cannot carry an inline subdocument past
/// the network denial.
fn deny_inline_subdocuments(document: &mut BaseDocument, policy: &ResourcePolicy) {
    let targets: Vec<_> = document
        .tree()
        .iter()
        .filter_map(|(id, node)| {
            let element = node.element_data()?;
            let local = &element.name.local;
            if local != &local_name!("iframe") && local != &local_name!("frame") {
                return None;
            }
            element
                .attrs
                .iter()
                .find(|attr| attr.name.local == local_name!("srcdoc"))
                .map(|attr| (id, attr.name.clone()))
        })
        .collect();

    for (id, name) in targets {
        let Some(element) = document
            .get_node_mut(id)
            .and_then(|node| node.element_data_mut())
        else {
            continue;
        };
        policy.record("<srcdoc>", DENIED_SUBDOCUMENT);
        element.attrs.remove(&name);
    }
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Read PNG dimensions from a checked IHDR header without decoding pixels.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || bytes[..8] != PNG_SIGNATURE {
        return None;
    }
    let length = u32::from_be_bytes(bytes[8..12].try_into().ok()?);
    if length != 13 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// Decode an allowed `data:image/png` URL, or return the denial reason.
fn decode_embedded_png(url: &Url) -> Result<Vec<u8>, &'static str> {
    if url.scheme() != "data" {
        return Err(match url.scheme() {
            "http" | "https" | "ws" | "wss" => DENIED_NETWORK,
            "file" => DENIED_FILESYSTEM,
            _ => DENIED_RESOURCE_SCHEME,
        });
    }
    let raw = url.as_str();
    let rest = raw
        .split_once(':')
        .map(|(_, rest)| rest)
        .ok_or(DENIED_MALFORMED_DATA)?;
    let (meta, payload) = rest.split_once(',').ok_or(DENIED_MALFORMED_DATA)?;
    let mut parts = meta.split(';');
    let mime = parts.next().unwrap_or_default().trim();
    if !mime.eq_ignore_ascii_case("image/png") {
        return Err(DENIED_DATA_TYPE);
    }
    let is_base64 = parts.any(|part| part.eq_ignore_ascii_case("base64"));
    let payload = percent_decode(payload);
    if !is_base64 {
        return Ok(payload);
    }
    let cleaned: Vec<u8> = payload
        .into_iter()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    BASE64.decode(cleaned).map_err(|_| DENIED_MALFORMED_DATA)
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            out.push((high << 4) | low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
