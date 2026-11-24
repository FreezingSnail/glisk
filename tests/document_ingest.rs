//! Tests for the shared bounded ingestion and offline resource policy.
//!
//! The module is compiled through its source path because the spike binary
//! does not declare it yet; the wiring bead adds `mod document;` to
//! `src/main.rs` and these tests then exercise the same code unchanged.

#[path = "../src/document.rs"]
#[allow(dead_code)]
mod document;

use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use blitz_dom::node::SpecialElementData;
use blitz_traits::{
    navigation::{NavigationOptions, NavigationProvider},
    net::{Bytes, NetHandler, NetProvider, Request, Url},
    shell::{ColorScheme, Viewport},
};
use document::*;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/decision-report.html"
);

// A valid 1x1 RGBA PNG (67 bytes).
const ONE_PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn viewport() -> Viewport {
    Viewport::new(420, 900, 1.0, ColorScheme::Dark)
}

/// Minimal PNG containing only a valid IHDR header (no image data).
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

fn data_png(width: u32, height: u32) -> String {
    format!(
        "data:image/png;base64,{}",
        BASE64.encode(png_header(width, height))
    )
}

#[derive(Clone)]
struct Capture(Arc<Mutex<Option<Vec<u8>>>>);

impl NetHandler for Capture {
    fn bytes(self: Box<Self>, _resolved_url: String, bytes: Bytes) {
        *self.0.lock().unwrap() = Some(bytes.to_vec());
    }
}

fn fetch(policy: &Arc<ResourcePolicy>, url: &str) -> Option<Vec<u8>> {
    let slot = Arc::new(Mutex::new(None));
    let provider = PolicyNetProvider(Arc::clone(policy));
    provider.fetch(
        0,
        Request::get(Url::parse(url).unwrap()),
        Box::new(Capture(Arc::clone(&slot))),
    );
    slot.lock().unwrap().clone()
}

fn reasons(policy: &ResourcePolicy) -> Vec<&'static str> {
    policy.denial_reasons()
}

#[test]
fn file_and_reader_sources_open_identically() {
    let limits = Limits::default();
    let from_file = ingest_file(FIXTURE, viewport(), limits).expect("fixture opens from a file");
    let bytes = std::fs::read(FIXTURE).expect("fixture readable");
    let from_reader = ingest_reader(&mut Cursor::new(bytes), viewport(), limits)
        .expect("fixture opens from in-memory stdin-shaped bytes");

    assert_eq!(from_file.stats, from_reader.stats);
    assert_eq!(
        from_file.document.tree().len(),
        from_reader.document.tree().len()
    );
    assert!(from_file.policy.denials().is_empty());
    assert!(from_reader.policy.denials().is_empty());
    assert!(from_file.document.get_element_by_id("summary").is_some());
}

#[test]
fn html_byte_cap_accepts_exactly_the_limit() {
    let limits = Limits {
        html_bytes: 64,
        ..Limits::default()
    };
    let at_limit = "a".repeat(64);
    let accepted = read_bounded(&mut Cursor::new(at_limit.as_bytes()), limits.html_bytes)
        .expect("64 bytes fit a 64-byte cap");
    assert_eq!(accepted.len(), 64);

    let over_limit = "a".repeat(65);
    match read_bounded(&mut Cursor::new(over_limit.as_bytes()), limits.html_bytes) {
        Err(IngestError::TooLarge { limit, observed }) => {
            assert_eq!(limit, 64);
            assert_eq!(observed, 65);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn invalid_utf8_is_rejected_before_parsing() {
    let error = read_bounded(&mut Cursor::new(b"ok\xfftail".as_slice()), 64)
        .expect_err("invalid UTF-8 must fail");
    match error {
        IngestError::InvalidUtf8 { valid_up_to } => assert_eq!(valid_up_to, 2),
        other => panic!("expected InvalidUtf8, got {other:?}"),
    }
}

#[test]
fn dom_node_budget_reports_the_limit() {
    let limits = Limits {
        dom_nodes: 12,
        ..Limits::default()
    };
    let html = format!(
        "<html><body>{}</body></html>",
        "<p>paragraph</p>".repeat(20)
    );
    match open_document(&html, viewport(), limits) {
        Err(IngestError::NodeBudget { limit, observed }) => {
            assert_eq!(limit, 12);
            assert!(observed > limit);
        }
        _ => panic!("expected a node budget error"),
    }
}

#[test]
fn dom_depth_boundary_and_budget() {
    let exact = "<html><body><div><div></div></div></body></html>";
    let stats = dom_stats(
        &open_document(exact, viewport(), Limits::default())
            .expect("exact-depth document opens")
            .document,
    );
    assert_eq!(stats.depth, 4);

    let at_limit = Limits {
        dom_depth: stats.depth,
        ..Limits::default()
    };
    open_document(exact, viewport(), at_limit).expect("depth equal to the cap is accepted");

    let one_deeper = "<html><body><div><div><div></div></div></div></body></html>";
    let too_deep = Limits {
        dom_depth: stats.depth,
        ..Limits::default()
    };
    match open_document(one_deeper, viewport(), too_deep) {
        Err(IngestError::DepthBudget { limit, observed }) => {
            assert_eq!(limit, 4);
            assert_eq!(observed, 5);
        }
        _ => panic!("expected a depth budget error"),
    }
}

#[test]
fn png_dimensions_come_from_the_checked_header() {
    assert_eq!(png_dimensions(&png_header(10, 20)), Some((10, 20)));
    assert_eq!(png_dimensions(b"not a png"), None);
    assert_eq!(png_dimensions(&png_header(0, 20)), None);
    let mut bad_chunk = png_header(10, 20);
    bad_chunk[12..16].copy_from_slice(b"IDAT");
    assert_eq!(png_dimensions(&bad_chunk), None);
}

#[test]
fn embedded_png_pixel_budgets_are_enforced_before_decode() {
    let limits = Limits {
        image_pixels: 100,
        total_image_pixels: 1_000,
        ..Limits::default()
    };
    let policy = Arc::new(ResourcePolicy::new(limits));

    assert!(fetch(&policy, &data_png(10, 10)).is_some(), "100 px fits");
    assert_eq!(policy.admitted_images(), 1);
    assert_eq!(policy.admitted_image_pixels(), 100);

    assert!(
        fetch(&policy, &data_png(10, 11)).is_none(),
        "110 px is too big"
    );
    assert_eq!(policy.denial_reasons(), vec![DENIED_IMAGE_BUDGET]);

    assert!(
        fetch(&policy, &data_png(4_000, 3_000)).is_none(),
        "12 million px is denied"
    );
    assert_eq!(
        policy.denial_reasons(),
        vec![DENIED_IMAGE_BUDGET, DENIED_IMAGE_BUDGET]
    );
    assert_eq!(policy.admitted_images(), 1);
}

#[test]
fn document_image_pixel_budget_is_cumulative() {
    let limits = Limits {
        image_pixels: 1_000,
        total_image_pixels: 200,
        ..Limits::default()
    };
    let policy = Arc::new(ResourcePolicy::new(limits));

    assert!(fetch(&policy, &data_png(10, 10)).is_some());
    assert!(fetch(&policy, &data_png(10, 10)).is_some());
    assert!(fetch(&policy, &data_png(10, 10)).is_none());
    assert_eq!(policy.admitted_images(), 2);
    assert_eq!(policy.admitted_image_pixels(), 200);
    assert_eq!(policy.denial_reasons(), vec![DENIED_TOTAL_BUDGET]);

    let below_boundary = Limits {
        image_pixels: 1_000,
        total_image_pixels: 199,
        ..Limits::default()
    };
    let policy = Arc::new(ResourcePolicy::new(below_boundary));
    assert!(fetch(&policy, &data_png(10, 10)).is_some());
    assert!(fetch(&policy, &data_png(10, 10)).is_none());
}

#[test]
fn external_and_unsupported_resources_are_denied() {
    let policy = Arc::new(ResourcePolicy::new(Limits::default()));

    assert!(fetch(&policy, "https://example.invalid/chart.png").is_none());
    assert!(fetch(&policy, "file:///etc/passwd").is_none());
    assert!(fetch(&policy, "about:blank").is_none());
    assert!(fetch(&policy, "data:image/svg+xml,<svg/>").is_none());
    assert!(fetch(&policy, "data:text/css,body%7Bcolor:red%7D").is_none());
    assert!(fetch(&policy, "data:image/png;base64,!!!!").is_none());

    assert_eq!(
        reasons(&policy),
        vec![
            DENIED_NETWORK,
            DENIED_FILESYSTEM,
            DENIED_RESOURCE_SCHEME,
            DENIED_DATA_TYPE,
            DENIED_DATA_TYPE,
            DENIED_MALFORMED_DATA,
        ]
    );
    assert_eq!(policy.admitted_images(), 0);
}

#[test]
fn valid_embedded_png_is_decoded_and_the_text_stays_readable() {
    let html = format!(
        r#"<html><body><p id="readable">hello</p>
        <img id="pixel" alt="one pixel" src="data:image/png;base64,{ONE_PIXEL_PNG}">
        </body></html>"#
    );
    let ingested = open_document(&html, viewport(), Limits::default()).expect("document opens");
    assert!(ingested.policy.denials().is_empty());
    assert_eq!(ingested.policy.admitted_images(), 1);
    assert_eq!(ingested.policy.admitted_image_pixels(), 1);
    assert!(ingested.document.get_element_by_id("readable").is_some());

    let image = ingested
        .document
        .get_element_by_id("pixel")
        .expect("image element exists");
    let element = ingested
        .document
        .get_node(image)
        .and_then(|node| node.element_data())
        .expect("image element data");
    assert!(
        matches!(&element.special_data, SpecialElementData::Image(_)),
        "the embedded PNG decoded into image data"
    );
}

#[test]
fn blocked_optional_resources_leave_a_readable_document() {
    let html = r#"<html><head>
        <style>
          @import url("https://example.invalid/import.css");
          body { color: #222; }
        </style>
        <link rel="stylesheet" href="https://example.invalid/site.css">
        </head><body>
        <p id="readable">The report body is still readable.</p>
        <img src="https://example.invalid/pixel.png" alt="blocked network image">
        <img src="file:///etc/passwd" alt="blocked file image">
        <iframe src="https://example.invalid/frame.html" title="blocked frame"></iframe>
        <iframe srcdoc="&lt;p&gt;inline&lt;/p&gt;" title="blocked inline frame"></iframe>
        </body></html>"#;
    let ingested =
        open_document(html, viewport(), Limits::default()).expect("document still opens");

    assert!(ingested.document.get_element_by_id("readable").is_some());
    assert_eq!(ingested.policy.admitted_images(), 0);
    let reasons = reasons(&ingested.policy);
    assert!(reasons.contains(&DENIED_NETWORK), "reasons: {reasons:?}");
    assert!(reasons.contains(&DENIED_FILESYSTEM), "reasons: {reasons:?}");
    assert!(
        reasons.contains(&DENIED_SUBDOCUMENT),
        "reasons: {reasons:?}"
    );
    let inline_frame = ingested
        .document
        .query_selector("iframe[srcdoc]")
        .expect("selector parses");
    assert!(inline_frame.is_none(), "srcdoc is removed before layout");
    assert!(
        ingested
            .policy
            .denials()
            .iter()
            .any(|denial| denial.url.contains("example.invalid/import.css")),
        "the CSS import was intercepted"
    );
}

#[test]
fn non_fragment_svg_references_are_stripped() {
    let html = r##"<html><body>
        <svg viewBox="0 0 10 10" width="10" height="10">
          <image href="https://example.invalid/overlay.png" width="10" height="10"/>
          <use href="#local-shape"/>
          <circle id="local-shape" cx="5" cy="5" r="4"/>
        </svg>
        <p id="readable">caption</p>
        </body></html>"##;
    let ingested = open_document(html, viewport(), Limits::default()).expect("document opens");

    assert!(ingested.document.get_element_by_id("readable").is_some());
    assert!(reasons(&ingested.policy).contains(&DENIED_SVG_REFERENCE));
    assert!(
        ingested
            .policy
            .denials()
            .iter()
            .any(|denial| denial.url == "https://example.invalid/overlay.png")
    );
    let image = ingested
        .document
        .query_selector("image")
        .expect("selector parses")
        .expect("image element survives");
    let element = ingested
        .document
        .get_node(image)
        .and_then(|node| node.element_data())
        .expect("image element data");
    assert!(element.attr(blitz_dom::local_name!("href")).is_none());
}

#[test]
fn external_navigation_is_denied_but_fragments_stay_local() {
    let policy = Arc::new(ResourcePolicy::new(Limits::default()));
    let navigation = PolicyNavigationProvider(Arc::clone(&policy));
    navigation.navigate_to(NavigationOptions::new(
        Url::parse("https://example.invalid/away").unwrap(),
        None,
        0,
    ));
    assert_eq!(policy.denial_reasons(), vec![DENIED_NAVIGATION]);

    let base = document_base_url();
    let local = Url::parse(&format!("{DOCUMENT_BASE_URL}#comparison")).unwrap();
    let external = Url::parse("https://example.invalid/#comparison").unwrap();
    assert!(is_local_fragment(&base, &local));
    assert!(!is_local_fragment(&base, &external));

    let mut ingested = ingest_file(FIXTURE, viewport(), Limits::default())
        .expect("fixture opens through the shared entry point");
    assert!(ingested.policy.denials().is_empty());
    assert!(
        ingested
            .document
            .get_fragment_target("comparison")
            .is_some(),
        "fragment targets resolve inside the document"
    );
    assert!(ingested.document.scroll_to_fragment("comparison"));
    assert!(
        ingested.document.get_fragment_target("not-there").is_none(),
        "a missing fragment does not navigate away"
    );
}
