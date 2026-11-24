//! Keyboard navigation, fragment jumps, disclosure reveal, and focus state.
//!
//! The modules are compiled through their source paths because wiring into
//! `src/main.rs` belongs to the integration bead. These tests use explicit
//! geometry, so no terminal is required.

#[path = "../src/navigation.rs"]
mod navigation;
#[path = "../src/viewport.rs"]
mod viewport;

use blitz_dom::{BaseDocument, DocumentConfig, NodeId, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport as BlitzViewport};
use navigation::{ControlKind, FragmentJump, Navigation, NavigationStatus, RevealOutcome};
use viewport::{ResizeOutcome, Viewport};

fn fixture_html() -> String {
    let mut html = String::from(
        "<!doctype html><html><head><style>
         html, body { margin: 0; padding: 0; background: #ffffff; color: #101010; }
         body { font: 16px monospace; }
         p, h1, h2 { margin: 0 0 8px 0; }
         summary { display: block; }
         </style></head><body>
         <h1 id=\"top\">Decision report</h1>
         <p><a id=\"first\" href=\"#target%20id\">First section link</a></p>
         <p><a id=\"external\" href=\"https://example.com/report\">External link</a></p>
         <p><a id=\"hidden-link\" href=\"#top\" hidden>Hidden link</a></p>
         <p><a id=\"styled-hidden\" href=\"#top\" style=\"display:none\">Styled hidden</a></p>
         <p><a id=\"missing-link\" href=\"#missing%20id\">Missing target</a></p>",
    );
    for index in 0..8 {
        html.push_str(&format!(
            "<p>Lead paragraph {index} carries enough words to wrap at a narrow width.</p>"
        ));
    }
    html.push_str(
        "<details id=\"outer\"><summary id=\"outer-summary\">Outer evidence</summary>\
         <p><a id=\"inside-outer\" href=\"#target%20id\">Link inside outer evidence</a></p>\
         <details id=\"inner\"><summary id=\"inner-summary\">Inner evidence</summary>\
         <p id=\"target id\">Target paragraph inside nested disclosures, with enough text \
         to occupy several rows when the viewport narrows.</p></details></details>",
    );
    for index in 0..20 {
        html.push_str(&format!(
            "<p>Trailing paragraph {index} keeps the document taller than the viewport.</p>"
        ));
    }
    html.push_str("<p><a id=\"last\" href=\"#top\">Last section link</a></p></body></html>");
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

fn element(viewport: &Viewport, id: &str) -> NodeId {
    viewport
        .document()
        .get_element_by_id(id)
        .unwrap_or_else(|| panic!("fixture must contain #{id}"))
}

fn control_ids(nav: &Navigation, viewport: &mut Viewport) -> Vec<NodeId> {
    nav.controls(viewport)
        .into_iter()
        .map(|control| control.node)
        .collect()
}

fn is_open(viewport: &Viewport, id: NodeId) -> bool {
    viewport
        .document()
        .get_node(id)
        .is_some_and(|node| node.data.has_attr(local_name!("open")))
}

fn click_point(viewport: &mut Viewport, node: NodeId) -> (f32, f32) {
    viewport.resolve();
    let rect = viewport
        .document()
        .get_client_bounding_rect(node)
        .expect("rendered control rect");
    (
        (rect.x + rect.width / 2.0) as f32,
        (rect.y + rect.height / 2.0) as f32,
    )
}

fn scroll_node_near_top(viewport: &mut Viewport, node: NodeId) {
    viewport.resolve();
    let top = viewport
        .document()
        .get_node(node)
        .expect("target node")
        .absolute_position(0.0, 0.0)
        .y as f64;
    viewport.scroll_to((top - 8.0).max(0.0));
}

fn target_visible(viewport: &mut Viewport, target: NodeId) -> bool {
    let (_, height) = viewport.geometry().expect("geometry").css_size();
    let top = f64::from(
        viewport
            .document()
            .get_node(target)
            .expect("target node")
            .absolute_position(0.0, 0.0)
            .y,
    );
    let scroll = viewport.scroll_y();
    top + f64::EPSILON >= scroll && top < scroll + height
}

#[test]
fn controls_follow_document_order_and_skip_ineligible_links() {
    let mut viewport = viewport(200, 120);
    let first = element(&viewport, "first");
    let missing = element(&viewport, "missing-link");
    let outer = element(&viewport, "outer-summary");
    let last = element(&viewport, "last");

    let controls = Navigation::new().controls(&mut viewport);
    assert_eq!(
        controls
            .iter()
            .map(|control| control.node)
            .collect::<Vec<_>>(),
        vec![first, missing, outer, last]
    );
    assert_eq!(controls[0].kind, ControlKind::FragmentLink);
    assert_eq!(controls[2].kind, ControlKind::DisclosureSummary);
    assert_eq!(controls[0].fragment(), Some("target%20id"));
    assert_eq!(controls[2].fragment(), None);
}

#[test]
fn tab_and_shift_tab_traverse_forward_and_reverse_with_wrap() {
    let mut viewport = viewport(200, 120);
    let first = element(&viewport, "first");
    let missing = element(&viewport, "missing-link");
    let outer = element(&viewport, "outer-summary");
    let last = element(&viewport, "last");
    let mut nav = Navigation::new();

    assert_eq!(nav.focus_next(&mut viewport).unwrap().node, first);
    assert_eq!(nav.focus_next(&mut viewport).unwrap().node, missing);
    assert_eq!(nav.focus_next(&mut viewport).unwrap().node, outer);
    assert_eq!(nav.focus_next(&mut viewport).unwrap().node, last);
    assert_eq!(nav.focus_next(&mut viewport).unwrap().node, first);

    assert_eq!(nav.focus_prev(&mut viewport).unwrap().node, last);
    assert_eq!(nav.focus_prev(&mut viewport).unwrap().node, outer);
    assert_eq!(nav.focus_prev(&mut viewport).unwrap().node, missing);
    assert_eq!(nav.focus_prev(&mut viewport).unwrap().node, first);

    let indicator = nav.focus_indicator(&mut viewport).expect("indicator");
    assert_eq!(indicator.node, first);
    assert!(indicator.visible);
    assert!(indicator.bounds.width > 0.0 && indicator.bounds.height > 0.0);
}

#[test]
fn shift_tab_from_no_focus_selects_the_last_control() {
    let mut viewport = viewport(200, 120);
    let last = element(&viewport, "last");
    let mut nav = Navigation::new();
    assert_eq!(nav.focus_prev(&mut viewport).unwrap().node, last);
}

#[test]
fn hidden_and_non_fragment_controls_are_excluded_until_revealed() {
    let mut viewport = viewport(200, 120);
    let first = element(&viewport, "first");
    let missing = element(&viewport, "missing-link");
    let outer_summary = element(&viewport, "outer-summary");
    let inside_outer = element(&viewport, "inside-outer");
    let inner_summary = element(&viewport, "inner-summary");
    let last = element(&viewport, "last");
    let nav = Navigation::new();

    assert_eq!(
        control_ids(&nav, &mut viewport),
        vec![first, missing, outer_summary, last]
    );

    let outer = element(&viewport, "outer");
    viewport.document_mut().toggle_details_open(outer);
    viewport.document_mut().resolve(0.0);
    assert_eq!(
        control_ids(&nav, &mut viewport),
        vec![
            first,
            missing,
            outer_summary,
            inside_outer,
            inner_summary,
            last
        ]
    );

    let inner = element(&viewport, "inner");
    viewport.document_mut().toggle_details_open(inner);
    viewport.document_mut().resolve(0.0);
    assert_eq!(
        control_ids(&nav, &mut viewport),
        vec![
            first,
            missing,
            outer_summary,
            inside_outer,
            inner_summary,
            last
        ]
    );
}

#[test]
fn enter_toggles_a_disclosure_and_keeps_focus_on_the_live_dom() {
    let mut viewport = viewport(200, 120);
    let outer = element(&viewport, "outer");
    let outer_summary = element(&viewport, "outer-summary");
    let mut nav = Navigation::new();

    nav.focus_next(&mut viewport);
    nav.focus_next(&mut viewport);
    let focused = nav.focus_next(&mut viewport).expect("outer summary");
    assert_eq!(focused.node, outer_summary);

    assert_eq!(
        nav.activate(&mut viewport),
        NavigationStatus::Toggled {
            details: outer,
            open: true
        }
    );
    assert!(is_open(&viewport, outer));
    assert_eq!(
        viewport.document().get_focussed_node_id(),
        Some(outer_summary)
    );
    assert!(nav.focus_indicator(&mut viewport).unwrap().visible);

    assert_eq!(
        nav.activate(&mut viewport),
        NavigationStatus::Toggled {
            details: outer,
            open: false
        }
    );
    assert!(!is_open(&viewport, outer));
}

#[test]
fn clicks_activate_rendered_controls_and_ignore_other_targets() {
    let mut viewport = viewport(200, 120);
    let external = element(&viewport, "external");
    let first = element(&viewport, "first");
    let outer = element(&viewport, "outer");
    let outer_summary = element(&viewport, "outer-summary");
    let mut nav = Navigation::new();

    let (x, y) = click_point(&mut viewport, external);
    assert!(
        !nav.click(&mut viewport, x, y),
        "external links are ignored"
    );
    assert_eq!(nav.status(), &NavigationStatus::Idle);

    assert!(
        !nav.click(&mut viewport, 5.0, 4.0),
        "ordinary content has no control"
    );
    assert_eq!(nav.status(), &NavigationStatus::Idle);

    scroll_node_near_top(&mut viewport, outer_summary);
    let (x, y) = click_point(&mut viewport, outer_summary);
    assert!(nav.click(&mut viewport, x, y));
    assert!(is_open(&viewport, outer));
    assert_eq!(
        nav.status(),
        &NavigationStatus::Toggled {
            details: outer,
            open: true
        }
    );
    assert_eq!(
        viewport.document().get_focussed_node_id(),
        Some(outer_summary),
        "the clicked summary keeps keyboard focus"
    );

    let (x, y) = click_point(&mut viewport, outer_summary);
    assert!(nav.click(&mut viewport, x, y));
    assert!(!is_open(&viewport, outer));
    assert_eq!(
        nav.status(),
        &NavigationStatus::Toggled {
            details: outer,
            open: false
        }
    );

    // Enter continues from the control the click focused.
    assert_eq!(
        nav.activate(&mut viewport),
        NavigationStatus::Toggled {
            details: outer,
            open: true
        }
    );

    viewport.scroll_home();
    let (x, y) = click_point(&mut viewport, first);
    assert!(nav.click(&mut viewport, x, y));
    let NavigationStatus::Jumped { node, fragment, .. } = nav.status() else {
        panic!("expected a fragment jump, got {:?}", nav.status());
    };
    assert_eq!(*node, element(&viewport, "target id"));
    assert_eq!(fragment, "target id");
    assert_eq!(viewport.document().get_focussed_node_id(), Some(first));
}

#[test]
fn click_positions_follow_reflow_after_resize() {
    let mut viewport = viewport(220, 130);
    let outer = element(&viewport, "outer");
    let outer_summary = element(&viewport, "outer-summary");
    let mut nav = Navigation::new();

    scroll_node_near_top(&mut viewport, outer_summary);
    let (x, y) = click_point(&mut viewport, outer_summary);
    assert!(nav.click(&mut viewport, x, y));
    assert!(is_open(&viewport, outer));

    let ResizeOutcome::Applied(_) = viewport.resize(150, 140, 1.0) else {
        panic!("valid resize was skipped");
    };
    scroll_node_near_top(&mut viewport, outer_summary);
    let (x, y) = click_point(&mut viewport, outer_summary);
    assert!(nav.click(&mut viewport, x, y));
    assert!(!is_open(&viewport, outer));
}

#[test]
fn fragment_links_decode_ids_and_reveal_nested_closed_disclosures() {
    let mut viewport = viewport(200, 120);
    let first = element(&viewport, "first");
    let outer = element(&viewport, "outer");
    let inner = element(&viewport, "inner");
    let target = element(&viewport, "target id");
    let mut nav = Navigation::new();

    nav.focus_next(&mut viewport);
    assert_eq!(nav.focused_control(&mut viewport).unwrap().node, first);
    let status = nav.activate(&mut viewport);

    let NavigationStatus::Jumped {
        node,
        fragment,
        opened,
    } = status
    else {
        panic!("expected a fragment jump, got {status:?}");
    };
    assert_eq!(node, target);
    assert_eq!(fragment, "target id");
    assert_eq!(opened, vec![outer, inner]);
    assert!(is_open(&viewport, outer));
    assert!(is_open(&viewport, inner));
    assert!(target_visible(&mut viewport, target));
    assert!(
        viewport.scroll_y() > 0.0,
        "the target is below the first viewport"
    );
}

#[test]
fn unknown_fragment_reports_status_without_changing_the_view() {
    let mut viewport = viewport(200, 120);
    let missing = element(&viewport, "missing-link");
    let outer = element(&viewport, "outer");
    let mut nav = Navigation::new();

    viewport.scroll_to(260.0);
    assert!(viewport.scroll_y() > 0.0);

    nav.focus_next(&mut viewport);
    nav.focus_next(&mut viewport);
    assert_eq!(nav.focused_control(&mut viewport).unwrap().node, missing);
    let scroll_before = viewport.scroll_y();

    assert_eq!(
        nav.activate(&mut viewport),
        NavigationStatus::UnknownFragment {
            fragment: "missing id".to_string()
        }
    );
    assert_eq!(viewport.scroll_y(), scroll_before);
    assert!(!is_open(&viewport, outer));

    match navigation::show_fragment(&mut viewport, "#still%20missing") {
        FragmentJump::UnknownFragment { fragment } => assert_eq!(fragment, "still missing"),
        other => panic!("expected unknown fragment, got {other:?}"),
    }
    assert_eq!(viewport.scroll_y(), scroll_before);
    assert!(!is_open(&viewport, outer));
}

#[test]
fn search_style_reveal_opens_enclosing_disclosures_and_positions_the_node() {
    let mut viewport = viewport(200, 120);
    let outer = element(&viewport, "outer");
    let inner = element(&viewport, "inner");
    let target = element(&viewport, "target id");
    let mut nav = Navigation::new();

    let outcome = nav.reveal(&mut viewport, target).expect("reveal");
    assert_eq!(outcome.node, target);
    assert_eq!(outcome.opened, vec![outer, inner]);
    assert!(outcome.aligned);
    assert!(is_open(&viewport, outer));
    assert!(is_open(&viewport, inner));
    assert!(target_visible(&mut viewport, target));

    match navigation::reveal_node(&mut viewport, target) {
        Ok(RevealOutcome { node, scroll_y, .. }) => {
            assert_eq!(node, target);
            assert!((scroll_y - viewport.scroll_y()).abs() < 0.01);
        }
        other => panic!("expected reveal, got {other:?}"),
    }
}

#[test]
fn resize_retains_focus_and_open_disclosures_for_useful_continuation() {
    let mut viewport = viewport(220, 130);
    let first = element(&viewport, "first");
    let outer = element(&viewport, "outer");
    let missing = element(&viewport, "missing-link");
    let mut nav = Navigation::new();

    nav.focus_next(&mut viewport);
    assert_eq!(nav.focused_control(&mut viewport).unwrap().node, first);

    viewport.document_mut().toggle_details_open(outer);
    viewport.document_mut().resolve(0.0);
    assert!(is_open(&viewport, outer));

    let ResizeOutcome::Applied(_) = viewport.resize(150, 140, 1.0) else {
        panic!("valid resize was skipped");
    };

    assert_eq!(viewport.document().get_focussed_node_id(), Some(first));
    assert_eq!(nav.focused_control(&mut viewport).unwrap().node, first);
    assert!(is_open(&viewport, outer));

    let next = nav.focus_next(&mut viewport).expect("continuation");
    assert_eq!(next.node, missing);
    let indicator = nav.focus_indicator(&mut viewport).expect("indicator");
    assert_eq!(indicator.node, missing);
    assert!(indicator.visible);
    assert!(is_open(&viewport, outer));
}
