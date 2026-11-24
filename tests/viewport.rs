//! Viewport painting, scroll clamping, and resize anchoring.
//!
//! The module is compiled through its source path because wiring into
//! `src/main.rs` belongs to the integration bead. These tests use explicit
//! geometry, so no terminal is required.

#[path = "../src/viewport.rs"]
mod viewport;

use std::time::Duration;

use blitz_dom::{BaseDocument, DocumentConfig, LocalName, Namespace, QualName, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport as BlitzViewport};
use viewport::{
    GeometryProblem, PaintError, PaintOutcome, ResizeOutcome, SkipReason, Viewport, ViewportLimits,
};

const PARAGRAPHS: usize = 24;

fn fixture_html() -> String {
    let mut html = String::from(
        "<!doctype html><html><head><style>
         html, body { margin: 0; padding: 0; background: #ffffff; color: #101010; }
         body { font: 16px monospace; }
         p, h1, h2 { margin: 0 0 8px 0; }
         a { color: #0033aa; }
         summary { display: block; }
         </style></head><body>
         <h1 id=\"top\">Decision report</h1>
         <p><a id=\"link\" href=\"#last\">Jump to the end</a></p>",
    );
    for index in 0..PARAGRAPHS {
        html.push_str(&format!(
            "<p id=\"p{index}\">Paragraph {index} carries enough words to wrap at a \
             narrow terminal width and change height when the viewport is resized.</p>"
        ));
        if index == 11 {
            html.push_str(
                "<h2 id=\"middle-heading\">Middle evidence</h2>\
                 <details id=\"evidence\"><summary>Open evidence</summary>\
                 <p id=\"evidence-body\">Evidence body text.</p></details>",
            );
        }
    }
    html.push_str("<p id=\"last\">Last paragraph.</p></body></html>");
    html
}

fn document(width: u32, height: u32) -> BaseDocument {
    let config = DocumentConfig {
        viewport: Some(BlitzViewport::new(width, height, 1.0, ColorScheme::Dark)),
        ..Default::default()
    };
    let mut document = HtmlDocument::from_html(&fixture_html(), config).into_inner();
    document.resolve(0.0);
    document
}

fn viewport(width: u32, height: u32) -> Viewport {
    Viewport::new(document(width, height), width, height, 1.0)
}

fn document_from(html: &str, width: u32, height: u32) -> BaseDocument {
    let config = DocumentConfig {
        viewport: Some(BlitzViewport::new(width, height, 1.0, ColorScheme::Dark)),
        ..Default::default()
    };
    let mut document = HtmlDocument::from_html(html, config).into_inner();
    document.resolve(0.0);
    document
}

fn node_top(viewport: &Viewport, id: &str) -> f64 {
    let node = viewport
        .document()
        .get_element_by_id(id)
        .unwrap_or_else(|| panic!("fixture must contain #{id}"));
    f64::from(
        viewport
            .document()
            .get_node(node)
            .expect("node")
            .absolute_position(0.0, 0.0)
            .y,
    )
}

fn painted(viewport: &mut Viewport) -> viewport::Frame {
    match viewport.paint().expect("paint succeeds") {
        PaintOutcome::Frame(frame) => frame,
        PaintOutcome::Skipped(reason) => panic!("paint skipped: {reason}"),
    }
}

fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    (
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
    )
}

fn approx(actual: f64, expected: f64, tolerance: f64) -> bool {
    (actual - expected).abs() <= tolerance
}

#[test]
fn css_animations_change_the_rendered_frame_as_the_clock_advances() {
    let html = include_str!("../examples/animated-cache.html");
    let config = DocumentConfig {
        viewport: Some(BlitzViewport::new(420, 360, 1.0, ColorScheme::Dark)),
        ..Default::default()
    };
    let mut document = HtmlDocument::from_html(html, config).into_inner();
    document.resolve(0.0);
    let mut viewport = Viewport::new(document, 420, 360, 1.0);
    let initial = painted(&mut viewport).png;
    assert!(viewport.is_animating());

    viewport.advance_animation(Duration::from_millis(750));
    let later = painted(&mut viewport).png;
    assert_ne!(later, initial, "keyframes should move the diagram markers");
}

#[test]
fn scrolling_clamps_at_both_ends() {
    let mut viewport = viewport(200, 120);
    assert!(viewport.scroll_by(50.0));
    assert!(viewport.scroll_home());
    assert!(!viewport.scroll_by(-50.0));
    assert_eq!(viewport.scroll_y(), 0.0);

    let max = viewport.max_scroll();
    assert!(max > 0.0, "fixture must be taller than the viewport");

    assert!(viewport.scroll_end());
    assert!(approx(viewport.scroll_y(), max, 0.01));
    assert!(!viewport.scroll_by(100.0));
    assert!(approx(viewport.scroll_y(), max, 0.01));
}

#[test]
fn paging_reaches_beginning_and_end_without_blank_overscroll() {
    let mut viewport = viewport(200, 120);
    let max = viewport.max_scroll();

    assert!(viewport.scroll_page(1.0));
    let first_page = viewport.scroll_y();
    assert!(first_page > 0.0 && first_page < max);

    assert!(viewport.scroll_page(-1.0));
    assert!(approx(viewport.scroll_y(), 0.0, 0.01));

    assert!(viewport.scroll_page(1_000.0));
    assert!(approx(viewport.scroll_y(), max, 0.01));
    assert!(!viewport.scroll_page(1_000.0));
    assert!(approx(viewport.scroll_y(), max, 0.01));
}

#[test]
fn resize_keeps_the_top_block_and_its_offset() {
    let mut viewport = viewport(220, 130);
    assert!(viewport.scroll_by(250.0));
    let before = viewport.visible_block().expect("visible block");

    let ResizeOutcome::Applied(info) = viewport.resize(160, 130, 1.0) else {
        panic!("valid resize was skipped");
    };
    assert!(info.anchored);

    let after = viewport
        .visible_block()
        .expect("visible block after resize");
    assert_eq!(after.node, before.node);
    assert!(
        approx(after.offset, before.offset, 0.6),
        "offset changed: before {} after {}",
        before.offset,
        after.offset
    );
}

#[test]
fn resize_at_top_stays_at_top() {
    let mut viewport = viewport(220, 130);
    viewport.scroll_home();

    let ResizeOutcome::Applied(info) = viewport.resize(150, 130, 1.0) else {
        panic!("valid resize was skipped");
    };
    assert!(approx(info.scroll_y, 0.0, 0.01));
    assert!(approx(viewport.scroll_y(), 0.0, 0.01));
}

#[test]
fn resize_at_bottom_stays_at_bottom() {
    let mut viewport = viewport(220, 130);
    viewport.scroll_end();

    let ResizeOutcome::Applied(info) = viewport.resize(150, 130, 1.0) else {
        panic!("valid resize was skipped");
    };
    assert!(info.max_scroll > 0.0);
    assert!(approx(info.scroll_y, info.max_scroll, 0.01));
    assert!(approx(viewport.scroll_y(), viewport.max_scroll(), 0.01));
}

#[test]
fn resize_preserves_focus_and_open_disclosures_on_the_live_dom() {
    let mut viewport = viewport(220, 130);
    let link = viewport.document().get_element_by_id("link").expect("link");
    assert!(viewport.document_mut().set_focus_to(link));
    let details = viewport
        .document()
        .get_element_by_id("evidence")
        .expect("details");
    viewport.document_mut().toggle_details_open(details);
    viewport.document_mut().resolve(0.0);
    let focus_before = viewport.document().get_focussed_node_id();
    assert_eq!(focus_before, Some(link));

    let before_ptr = viewport.document() as *const BaseDocument;
    let ResizeOutcome::Applied(_) = viewport.resize(170, 140, 1.0) else {
        panic!("valid resize was skipped");
    };

    assert_eq!(before_ptr, viewport.document() as *const BaseDocument);
    assert_eq!(viewport.document().get_focussed_node_id(), focus_before);
    let open = viewport
        .document()
        .get_node(details)
        .is_some_and(|node| node.data.has_attr(local_name!("open")));
    assert!(open, "disclosure state was lost across resize");
}

#[test]
fn missing_block_falls_back_to_the_preceding_heading() {
    let mut viewport = viewport(220, 130);
    assert!(viewport.scroll_by(320.0));
    let anchor = viewport.capture_anchor().expect("anchor");
    let block = anchor.block.expect("block anchor");
    let heading = anchor.heading.expect("preceding heading");

    viewport.document_mut().mutate().remove_node(block);
    viewport.document_mut().resolve(0.0);

    let ResizeOutcome::Applied(info) = viewport.resize_with_anchor(160, 130, 1.0, Some(anchor))
    else {
        panic!("valid resize was skipped");
    };
    assert!(info.used_heading_fallback);
    assert!(info.anchored);

    let heading_top = f64::from(
        viewport
            .document()
            .get_node(heading)
            .expect("heading survives")
            .absolute_position(0.0, 0.0)
            .y,
    );
    assert!(
        approx(viewport.scroll_y(), heading_top, 0.5),
        "scroll {} did not land on heading top {heading_top}",
        viewport.scroll_y()
    );
}

#[test]
fn author_anchor_keeps_a_surviving_block_id_across_a_reload() {
    let mut first = viewport(220, 130);
    let old_top = node_top(&first, "p12");
    assert!(first.scroll_to(old_top));
    let anchor = first.capture_author_anchor().expect("author anchor");
    assert_eq!(anchor.block_id.as_deref(), Some("p12"));
    assert!(approx(anchor.block_offset, 0.0, 0.6));

    let changed = fixture_html().replace(
        "<p id=\"p11\">",
        "<p id=\"inserted\">Inserted words above the anchor.</p><p id=\"p11\">",
    );
    let mut second = Viewport::new(document_from(&changed, 220, 130), 220, 130, 1.0);
    let info = second.restore_author_anchor(&anchor);
    assert!(info.anchored && !info.used_heading_fallback);
    let new_top = node_top(&second, "p12");
    assert!(
        new_top > old_top,
        "fixture must move the anchored block down"
    );
    assert!(
        approx(second.scroll_y(), new_top, 0.6),
        "scroll {} did not follow #p12 to {new_top}",
        second.scroll_y()
    );
}

#[test]
fn author_anchor_falls_back_to_a_preceding_heading_id() {
    let mut first = viewport(220, 130);
    let target = node_top(&first, "p12");
    assert!(first.scroll_to(target));
    let anchor = first.capture_author_anchor().expect("author anchor");

    let changed = fixture_html().replace("<p id=\"p12\">", "<p>");
    let mut second = Viewport::new(document_from(&changed, 220, 130), 220, 130, 1.0);
    let info = second.restore_author_anchor(&anchor);
    assert!(info.anchored && info.used_heading_fallback);
    assert!(
        approx(second.scroll_y(), node_top(&second, "middle-heading"), 0.6),
        "scroll {} did not fall back to the heading",
        second.scroll_y()
    );
}

#[test]
fn author_anchor_clamps_when_every_id_is_lost() {
    let mut first = viewport(220, 130);
    let target = node_top(&first, "p12");
    assert!(first.scroll_to(target));
    let anchor = first.capture_author_anchor().expect("author anchor");
    assert!(anchor.block_id.is_some());
    assert!(anchor.heading_id.is_some());

    let changed = fixture_html()
        .replace("<p id=\"p12\">", "<p>")
        .replace("<h2 id=\"middle-heading\">", "<h2>");
    let mut second = Viewport::new(document_from(&changed, 220, 130), 220, 130, 1.0);
    let info = second.restore_author_anchor(&anchor);
    assert!(!info.anchored);
    assert!(!info.used_heading_fallback);
    assert!(approx(second.scroll_y(), anchor.scroll_y, 0.01));
}

#[test]
fn author_anchor_keeps_the_bottom_of_a_grown_revision() {
    let mut first = viewport(220, 130);
    assert!(first.scroll_end());
    let anchor = first.capture_author_anchor().expect("author anchor");
    assert!(anchor.at_bottom);

    let changed = fixture_html().replace(
        "</body>",
        "<p>Additional trailing words after the old end.</p></body>",
    );
    let mut second = Viewport::new(document_from(&changed, 220, 130), 220, 130, 1.0);
    let info = second.restore_author_anchor(&anchor);
    assert!(info.anchored);
    assert!(approx(second.scroll_y(), second.max_scroll(), 0.01));
}

#[test]
fn transient_tiny_window_keeps_the_reading_position() {
    let mut viewport = viewport(220, 130);
    assert!(viewport.scroll_by(260.0));
    let before = viewport.visible_block().expect("visible block");

    let ResizeOutcome::Skipped(GeometryProblem::TooSmall { .. }) = viewport.resize(1, 1, 1.0)
    else {
        panic!("tiny geometry was accepted");
    };
    assert!(matches!(
        viewport.paint().unwrap(),
        PaintOutcome::Skipped(SkipReason::Geometry(GeometryProblem::TooSmall { .. }))
    ));

    let ResizeOutcome::Applied(info) = viewport.resize(160, 130, 1.0) else {
        panic!("valid geometry did not recover the viewport");
    };
    assert!(info.anchored);
    let after = viewport
        .visible_block()
        .expect("visible block after recovery");
    assert_eq!(after.node, before.node);
    assert!(approx(after.offset, before.offset, 0.6));
}

#[test]
fn raster_size_and_limits_stay_constant_as_the_dom_grows() {
    let mut viewport = viewport(200, 120);
    let first = painted(&mut viewport);
    assert_eq!((first.width, first.height), (200, 120));
    assert_eq!(first.pixels, 200 * 120);
    assert_eq!(first.scale, 1.0);
    assert_eq!(first.scroll_y, 0.0);
    assert_eq!(png_size(&first.png), (200, 120));
    assert_eq!(
        viewport.geometry().expect("geometry").css_size(),
        (200.0, 120.0)
    );
    assert_eq!(viewport.geometry().expect("geometry").scale(), 1.0);
    let limits = viewport.limits();

    let body = viewport
        .document()
        .find_body_node()
        .map(|node| node.id)
        .expect("body");
    let mutator_document = viewport.document_mut();
    let mut mutator = mutator_document.mutate();
    for _ in 0..120 {
        let paragraph = mutator.create_element(
            QualName::new(
                None,
                Namespace::from("http://www.w3.org/1999/xhtml"),
                LocalName::from("p"),
            ),
            Vec::new(),
        );
        let text = mutator.create_text_node(
            "Growth paragraph with enough text to occupy rows in the longer document.",
        );
        mutator.append_children(paragraph, &[text]);
        mutator.append_children(body, &[paragraph]);
    }
    drop(mutator);

    let second = painted(&mut viewport);
    assert_eq!((second.width, second.height), (200, 120));
    assert_eq!(second.pixels, first.pixels);
    assert_eq!(png_size(&second.png), (200, 120));
    assert_eq!(viewport.limits(), limits);
    assert_eq!(
        viewport.geometry().expect("geometry").physical_size(),
        (200, 120)
    );
    assert!(viewport.max_scroll() > 0.0);
}

#[test]
fn tiny_invalid_and_oversized_geometry_skip_painting_until_valid() {
    let mut viewport = viewport(200, 120);
    assert!(matches!(viewport.paint().unwrap(), PaintOutcome::Frame(_)));

    let ResizeOutcome::Skipped(GeometryProblem::TooSmall { .. }) = viewport.resize(1, 1, 1.0)
    else {
        panic!("tiny geometry was accepted");
    };
    match viewport.paint().unwrap() {
        PaintOutcome::Skipped(SkipReason::Geometry(GeometryProblem::TooSmall { .. })) => {}
        other => panic!("expected tiny skip, got {other:?}"),
    }

    let ResizeOutcome::Skipped(GeometryProblem::Empty { .. }) = viewport.resize(0, 120, 1.0) else {
        panic!("empty geometry was accepted");
    };

    let ResizeOutcome::Skipped(GeometryProblem::DimensionTooLarge { .. }) =
        viewport.resize(70_000, 10, 1.0)
    else {
        panic!("oversized dimension was accepted");
    };

    let ResizeOutcome::Skipped(GeometryProblem::RasterBudget { .. }) =
        viewport.resize(5_000, 5_000, 1.0)
    else {
        panic!("oversized raster was accepted");
    };

    let ResizeOutcome::Skipped(GeometryProblem::InvalidScale { .. }) =
        viewport.resize(200, 120, f64::NAN)
    else {
        panic!("non-finite scale was accepted");
    };

    let ResizeOutcome::Skipped(GeometryProblem::InvalidScale { .. }) =
        viewport.resize(200, 120, 1.0e300)
    else {
        panic!("scale above the f32 surface range was accepted");
    };

    let ResizeOutcome::Applied(_) = viewport.resize(180, 110, 1.0) else {
        panic!("valid geometry did not recover the viewport");
    };
    let frame = painted(&mut viewport);
    assert_eq!((frame.width, frame.height), (180, 110));
}

#[test]
fn tightened_raster_limit_is_enforced_at_paint_time() {
    let mut viewport = viewport(200, 120).with_limits(ViewportLimits {
        max_raster_pixels: 1_000,
        ..ViewportLimits::default()
    });
    match viewport.paint().unwrap() {
        PaintOutcome::Skipped(SkipReason::Geometry(GeometryProblem::RasterBudget { .. })) => {}
        other => panic!("expected raster budget skip, got {other:?}"),
    }
}

#[test]
fn encoded_frame_budget_is_enforced_before_returning_a_frame() {
    let mut viewport = viewport(200, 120).with_limits(ViewportLimits {
        max_png_bytes: 1,
        ..ViewportLimits::default()
    });
    match viewport.paint() {
        Err(PaintError::PngBudget { limit, observed }) => {
            assert_eq!(limit, 1);
            assert!(observed > limit);
        }
        other => panic!("expected PNG budget error, got {other:?}"),
    }
}
