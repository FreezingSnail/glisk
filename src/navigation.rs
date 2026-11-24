//! Visible control focus, fragment jumps, and disclosure reveal.
//!
//! Bead: `glisk-2hp.5` · spec: `FEASIBILITY.md#navigation-and-search` ·
//! contract: one live [`Viewport`] document from [`crate::viewport`].
//!
//! The module enumerates eligible controls in document order, keeps keyboard
//! focus on the live DOM, and handles fragment jumps and disclosure toggles
//! from Enter or a mouse click. [`show_fragment`] and [`reveal_node`] are the
//! central reveal-and-jump entry points; the later search bead reuses them for
//! matches inside closed disclosures.
//!
//! Only current-document fragments are handled. External links are ignored by
//! enumeration and are never navigated to, matching the ingestion policy in
//! [`crate::document`].
//!
//! Focus and disclosure state live on the `BaseDocument`, so a
//! [`Viewport::resize`] reflow does not reset either. The module keeps only the
//! status string of the last action.
//!
//! Integration note: the wiring bead adds `mod navigation;` next to the other
//! modules. Until then the integration test includes this file with
//! `#[path = "../src/navigation.rs"]` together with the viewport module.

use blitz_dom::{BaseDocument, Node, NodeId, local_name};

use crate::viewport::Viewport;

/// The kind of control reached by Tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControlKind {
    /// `<a href="#...">` targeting the current document.
    FragmentLink,
    /// `<summary>` belonging to a `<details>` disclosure.
    DisclosureSummary,
}

/// One eligible visible control in document order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Control {
    pub node: NodeId,
    pub kind: ControlKind,
    /// Raw fragment without the leading `#`, still percent-encoded.
    fragment: Option<String>,
}

impl Control {
    /// Raw same-document fragment, still percent-encoded.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }
}

/// Document-relative CSS rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn top(self) -> f64 {
        self.y
    }

    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    pub fn left(self) -> f64 {
        self.x
    }

    pub fn right(self) -> f64 {
        self.x + self.width
    }
}

/// App-owned focus indication for the pager's status row or overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusIndicator {
    pub node: NodeId,
    pub kind: ControlKind,
    /// Document-relative CSS bounds of the focused element.
    pub bounds: Rect,
    /// The element's bounds intersect the current viewport rectangle.
    pub visible: bool,
}

/// Result of a successful reveal.
#[derive(Clone, Debug, PartialEq)]
pub struct RevealOutcome {
    pub node: NodeId,
    /// Closed disclosures that were opened, outermost first.
    pub opened: Vec<NodeId>,
    /// CSS scroll offset after the jump.
    pub scroll_y: f64,
    /// The target produced layout and was aligned in the viewport.
    pub aligned: bool,
}

/// A failed reveal: the node is not in the live document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevealError {
    pub node: NodeId,
}

/// Outcome of a same-document fragment jump.
#[derive(Clone, Debug, PartialEq)]
pub enum FragmentJump {
    Jumped(RevealOutcome),
    /// No element carried the decoded id. The view is unchanged.
    UnknownFragment {
        fragment: String,
    },
}

/// Status of the last navigation action, suitable for the status row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NavigationStatus {
    #[default]
    Idle,
    Focused {
        node: NodeId,
    },
    Jumped {
        node: NodeId,
        fragment: String,
        opened: Vec<NodeId>,
    },
    Toggled {
        details: NodeId,
        open: bool,
    },
    UnknownFragment {
        fragment: String,
    },
    NoFocus,
    NotLocalLink,
}

impl std::fmt::Display for NavigationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => f.write_str("ready"),
            Self::Focused { .. } => f.write_str("focus"),
            Self::Jumped { .. } => f.write_str("jumped"),
            Self::Toggled { open: true, .. } => f.write_str("evidence expanded"),
            Self::Toggled { open: false, .. } => f.write_str("evidence collapsed"),
            Self::UnknownFragment { fragment } => write!(f, "unknown fragment #{fragment}"),
            Self::NoFocus => f.write_str("no focus"),
            Self::NotLocalLink => f.write_str("link is not a document fragment"),
        }
    }
}

/// Decode a URL fragment for exact `id` matching.
///
/// `%XX` byte escapes are decoded, then the bytes are interpreted as UTF-8.
/// Invalid sequences become replacement characters, as in a browser URL.
pub fn decode_fragment(raw: &str) -> String {
    let bytes = raw.as_bytes();
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
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Enumerate visible fragment links and disclosure summaries in DOM order.
///
/// The caller must have resolved layout ([`BaseDocument::resolve`]); the
/// [`Navigation`] methods do that. Hidden controls, controls inside a closed
/// disclosure body, non-fragment links, and non-content subtrees are skipped.
pub fn controls(document: &BaseDocument) -> Vec<Control> {
    let mut out = Vec::new();
    visit(document, document.root_node().id, &mut out);
    out
}

fn visit(document: &BaseDocument, id: NodeId, out: &mut Vec<Control>) {
    let Some(node) = document.get_node(id) else {
        return;
    };
    if tag_of(node).is_some_and(is_non_content) {
        return;
    }
    if let Some(control) = visible_control(document, node) {
        out.push(control);
    }
    for &child in &node.children {
        visit(document, child, out);
    }
}

fn visible_control(document: &BaseDocument, node: &Node) -> Option<Control> {
    let (kind, fragment) = match tag_of(node)? {
        "a" => (
            ControlKind::FragmentLink,
            Some(node.attr(local_name!("href"))?.strip_prefix('#')?),
        ),
        "summary" if is_disclosure_summary(document, node) => {
            (ControlKind::DisclosureSummary, None)
        }
        _ => return None,
    };
    is_visible(document, node).then(|| Control {
        node: node.id,
        kind,
        fragment: fragment.map(str::to_owned),
    })
}

/// Follow a same-document fragment, revealing enclosing closed disclosures.
///
/// An unknown fragment returns [`FragmentJump::UnknownFragment`] without
/// scrolling, toggling, or otherwise changing the view.
pub fn show_fragment(viewport: &mut Viewport, fragment: &str) -> FragmentJump {
    let raw = fragment.strip_prefix('#').unwrap_or(fragment);
    let decoded = decode_fragment(raw);
    let Some(target) = viewport.document().get_element_by_id(&decoded) else {
        return FragmentJump::UnknownFragment { fragment: decoded };
    };
    match reveal_node(viewport, target) {
        Ok(outcome) => FragmentJump::Jumped(outcome),
        Err(_) => FragmentJump::UnknownFragment { fragment: decoded },
    }
}

/// Reveal a node and position it at the viewport top.
///
/// This is the shared search/pager hook: closed disclosures that hide the node
/// are opened outermost first, layout is resolved once, and the viewport is
/// scrolled through [`Viewport::scroll_to`] so `Viewport::scroll_y` and the live
/// document stay in sync.
pub fn reveal_node(viewport: &mut Viewport, node: NodeId) -> Result<RevealOutcome, RevealError> {
    if viewport.document().get_node(node).is_none() {
        return Err(RevealError { node });
    }
    viewport.resolve();
    let opened = open_hidden_ancestors(viewport.document_mut(), node);
    if !opened.is_empty() {
        viewport.resolve();
    }
    let top = document_top(viewport.document(), node);
    let aligned = top.is_some();
    let target = top.unwrap_or(viewport.scroll_y());
    viewport.scroll_to(target);
    Ok(RevealOutcome {
        node,
        opened,
        scroll_y: viewport.scroll_y(),
        aligned,
    })
}

/// Focus traversal and activation over one live viewport.
///
/// Focus itself is stored on the `BaseDocument` through
/// [`BaseDocument::set_focus_to`], so resize and reflow retain it. The struct
/// keeps only the last status for the status row.
#[derive(Clone, Debug, Default)]
pub struct Navigation {
    status: NavigationStatus,
}

impl Navigation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self) -> &NavigationStatus {
        &self.status
    }

    /// Eligible visible controls in document order after resolving layout.
    pub fn controls(&self, viewport: &mut Viewport) -> Vec<Control> {
        viewport.resolve();
        controls(viewport.document())
    }

    /// The focused control, if DOM focus currently sits on an eligible one.
    pub fn focused_control(&self, viewport: &mut Viewport) -> Option<Control> {
        let controls = self.controls(viewport);
        let focused = viewport.document().get_focussed_node_id()?;
        controls.into_iter().find(|control| control.node == focused)
    }

    /// Move focus to the next visible control and scroll it into view.
    ///
    /// With no eligible focus this selects the first control. Traversal wraps.
    pub fn focus_next(&mut self, viewport: &mut Viewport) -> Option<Control> {
        self.step(viewport, 1)
    }

    /// Move focus to the previous visible control and scroll it into view.
    ///
    /// With no eligible focus this selects the last control. Traversal wraps.
    pub fn focus_prev(&mut self, viewport: &mut Viewport) -> Option<Control> {
        self.step(viewport, -1)
    }

    fn step(&mut self, viewport: &mut Viewport, direction: isize) -> Option<Control> {
        let controls = self.controls(viewport);
        if controls.is_empty() {
            self.status = NavigationStatus::NoFocus;
            return None;
        }
        let count = controls.len();
        let index = viewport
            .document()
            .get_focussed_node_id()
            .and_then(|focused| controls.iter().position(|control| control.node == focused));
        let index = match (index, direction.is_positive()) {
            (Some(index), true) => (index + 1) % count,
            (Some(index), false) => (index + count - 1) % count,
            (None, true) => 0,
            (None, false) => count - 1,
        };
        let control = controls[index].clone();
        viewport.document_mut().set_focus_to(control.node);
        reveal_nearest(viewport, control.node);
        self.status = NavigationStatus::Focused { node: control.node };
        Some(control)
    }

    /// App-owned focus indicator for the current viewport.
    pub fn focus_indicator(&mut self, viewport: &mut Viewport) -> Option<FocusIndicator> {
        let control = self.focused_control(viewport)?;
        let node = viewport.document().get_node(control.node)?;
        let bounds = bounds_of(node)?;
        let visible = viewport.geometry().is_some_and(|geometry| {
            let (_, height) = geometry.css_size();
            let scroll = viewport.scroll_y();
            bounds.height > 0.0 && bounds.bottom() > scroll && bounds.top() < scroll + height
        });
        Some(FocusIndicator {
            node: control.node,
            kind: control.kind,
            bounds,
            visible,
        })
    }

    /// Activate the focused control: follow a fragment or toggle a disclosure.
    pub fn activate(&mut self, viewport: &mut Viewport) -> NavigationStatus {
        if let Some(control) = self.focused_control(viewport) {
            self.activate_control(viewport, &control);
        } else {
            self.status = NavigationStatus::NoFocus;
        }
        self.status.clone()
    }

    /// Focus and activate the control hit by a viewport-relative CSS point.
    pub fn click(&mut self, viewport: &mut Viewport, x: f32, y: f32) -> bool {
        viewport.resolve();
        let document = viewport.document();
        // Blitz's hit tester takes document coordinates and adjusts for fixed boxes.
        let Some(hit) = document.hit(x, y + viewport.scroll_y() as f32) else {
            return false;
        };
        let control = document
            .node_chain(hit.node_id)
            .into_iter()
            .find_map(|id| visible_control(document, document.get_node(id)?));
        let Some(control) = control else {
            return false;
        };
        viewport.document_mut().set_focus_to(control.node);
        self.activate_control(viewport, &control);
        true
    }

    fn activate_control(&mut self, viewport: &mut Viewport, control: &Control) {
        self.status = match control.kind {
            ControlKind::DisclosureSummary => {
                let Some(details) = enclosing_details(viewport.document(), control.node) else {
                    self.status = NavigationStatus::NotLocalLink;
                    return;
                };
                viewport.document_mut().toggle_details_open(details);
                viewport.resolve();
                let open = is_open(viewport.document(), details);
                NavigationStatus::Toggled { details, open }
            }
            ControlKind::FragmentLink => {
                let Some(fragment) = control.fragment() else {
                    self.status = NavigationStatus::NotLocalLink;
                    return;
                };
                match show_fragment(viewport, fragment) {
                    FragmentJump::Jumped(outcome) => NavigationStatus::Jumped {
                        node: outcome.node,
                        fragment: decode_fragment(fragment),
                        opened: outcome.opened,
                    },
                    FragmentJump::UnknownFragment { fragment } => {
                        NavigationStatus::UnknownFragment { fragment }
                    }
                }
            }
        };
    }

    /// Fragment jump with status tracking; used by the pager and future search.
    pub fn jump_to_fragment(&mut self, viewport: &mut Viewport, fragment: &str) -> FragmentJump {
        let outcome = show_fragment(viewport, fragment);
        self.status = match &outcome {
            FragmentJump::Jumped(reveal) => NavigationStatus::Jumped {
                node: reveal.node,
                fragment: decode_fragment(fragment.strip_prefix('#').unwrap_or(fragment)),
                opened: reveal.opened.clone(),
            },
            FragmentJump::UnknownFragment { fragment } => NavigationStatus::UnknownFragment {
                fragment: fragment.clone(),
            },
        };
        outcome
    }

    /// Search-compatible reveal with no focus change.
    pub fn reveal(
        &mut self,
        viewport: &mut Viewport,
        node: NodeId,
    ) -> Result<RevealOutcome, RevealError> {
        reveal_node(viewport, node)
    }
}

fn reveal_nearest(viewport: &mut Viewport, node: NodeId) {
    viewport.resolve();
    let Some(target) = viewport.document().get_node(node) else {
        return;
    };
    let Some(bounds) = bounds_of(target) else {
        return;
    };
    let (top, bottom) = (bounds.top(), bounds.bottom());
    let Some(geometry) = viewport.geometry() else {
        return;
    };
    let height = geometry.css_size().1;
    let scroll = viewport.scroll_y();
    let target = if top < scroll {
        top
    } else if bottom > scroll + height {
        bottom - height
    } else {
        return;
    };
    viewport.scroll_to(target);
}

fn open_hidden_ancestors(document: &mut BaseDocument, node: NodeId) -> Vec<NodeId> {
    let mut closed: Vec<NodeId> = document
        .node_chain(node)
        .into_iter()
        .filter(|id| {
            is_details(document, *id)
                && !is_open(document, *id)
                && !inside_summary_branch(document, *id, node)
        })
        .collect();
    closed.reverse();
    for details in &closed {
        document.toggle_details_open(*details);
    }
    closed
}

fn inside_summary_branch(document: &BaseDocument, details: NodeId, node: NodeId) -> bool {
    let Some(summary) = first_summary_child(document, details) else {
        return false;
    };
    let mut current = Some(node);
    while let Some(id) = current {
        if id == summary {
            return true;
        }
        if id == details {
            return false;
        }
        current = document.get_node(id).and_then(|node| node.parent);
    }
    false
}

fn first_summary_child(document: &BaseDocument, details: NodeId) -> Option<NodeId> {
    let node = document.get_node(details)?;
    node.children.iter().copied().find(|id| {
        document
            .get_node(*id)
            .and_then(Node::element_data)
            .is_some_and(|element| {
                let local: &str = element.name.local.as_ref();
                local == "summary"
            })
    })
}

fn enclosing_details(document: &BaseDocument, node: NodeId) -> Option<NodeId> {
    document
        .node_chain(node)
        .into_iter()
        .find(|id| is_details(document, *id))
}

fn is_disclosure_summary(document: &BaseDocument, node: &Node) -> bool {
    let Some(parent) = node.parent else {
        return false;
    };
    if !is_details(document, parent) {
        return false;
    }
    first_summary_child(document, parent) == Some(node.id)
}

fn is_details(document: &BaseDocument, id: NodeId) -> bool {
    document.get_node(id).and_then(tag_of) == Some("details")
}

fn is_open(document: &BaseDocument, id: NodeId) -> bool {
    document
        .get_node(id)
        .is_some_and(|node| node.data.has_attr(local_name!("open")))
}

fn tag_of(node: &Node) -> Option<&str> {
    node.element_data()
        .map(|element| element.name.local.as_ref())
}

fn is_non_content(tag: &str) -> bool {
    matches!(
        tag,
        "head"
            | "script"
            | "style"
            | "template"
            | "title"
            | "meta"
            | "link"
            | "base"
            | "noscript"
            | "iframe"
            | "object"
            | "embed"
            | "canvas"
            | "source"
            | "track"
    )
}

/// Whether an element would be visible to the reader: no explicit hiding, no
/// computed hiding on itself or an ancestor, not under a closed disclosure body,
/// and possessing a nonzero layout box.
fn is_visible(document: &BaseDocument, node: &Node) -> bool {
    if bounds_of(node).is_none_or(|bounds| bounds.width <= 0.0 || bounds.height <= 0.0) {
        return false;
    }
    let mut current = Some(node.id);
    while let Some(id) = current {
        let Some(ancestor) = document.get_node(id) else {
            return false;
        };
        if explicitly_hidden(ancestor) || computed_hidden(ancestor) {
            return false;
        }
        if id != node.id
            && is_details(document, id)
            && !is_open(document, id)
            && !inside_summary_branch(document, id, node.id)
        {
            return false;
        }
        current = ancestor.parent;
    }
    true
}

fn explicitly_hidden(node: &Node) -> bool {
    if node.data.has_attr(local_name!("hidden")) {
        return true;
    }
    if node
        .attr(local_name!("aria-hidden"))
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    node.attr(local_name!("style")).is_some_and(style_hides)
}

fn style_hides(style: &str) -> bool {
    let compact: String = style
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    compact.split(';').any(|declaration| {
        declaration.starts_with("display:none")
            || declaration.starts_with("visibility:hidden")
            || declaration.starts_with("visibility:collapse")
    })
}

fn computed_hidden(node: &Node) -> bool {
    if let Some(styles) = node.primary_styles() {
        let visibility = format!("{:?}", styles.clone_visibility());
        if visibility == "Hidden" || visibility == "Collapse" {
            return true;
        }
    }
    format!("{:?}", node.taffy_display()) == "None"
}

/// Border-box bounds in document CSS pixels. Non-atomic inline elements have no
/// layout box of their own, so their per-line fragment boxes are unioned instead.
fn bounds_of(node: &Node) -> Option<Rect> {
    if let Some(fragments) = node.inline_fragment_boxes() {
        let root = node.inline_root_ancestor()?;
        let origin = root.absolute_position(0.0, 0.0);
        let mut rect: Option<Rect> = None;
        for fragment in fragments {
            let left = f64::from(origin.x) + f64::from(fragment.left);
            let top = f64::from(origin.y) + f64::from(fragment.top);
            let right = f64::from(origin.x) + f64::from(fragment.right);
            let bottom = f64::from(origin.y) + f64::from(fragment.bottom);
            rect = Some(match rect {
                Some(existing) => Rect {
                    x: existing.x.min(left),
                    y: existing.y.min(top),
                    width: existing.right().max(right) - existing.x.min(left),
                    height: existing.bottom().max(bottom) - existing.y.min(top),
                },
                None => Rect {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                },
            });
        }
        return rect;
    }
    let position = node.absolute_position(0.0, 0.0);
    let size = node.final_layout().size;
    Some(Rect {
        x: f64::from(position.x),
        y: f64::from(position.y),
        width: f64::from(size.width),
        height: f64::from(size.height),
    })
}

fn document_top(document: &BaseDocument, node: NodeId) -> Option<f64> {
    let node = document.get_node(node)?;
    if !node.flags.is_in_document() || node.parent.is_none() {
        return None;
    }
    let top = f64::from(node.absolute_position(0.0, 0.0).y);
    top.is_finite().then_some(top)
}
