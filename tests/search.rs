//! Pager search prompt, match counts, wrapping, disclosure reveal, and resize.
//!
//! The modules are compiled through their source paths because wiring into
//! `src/main.rs` belongs to the integration bead. These tests use explicit
//! geometry, so no terminal is required.

#[path = "../src/navigation.rs"]
#[allow(dead_code)]
mod navigation;
#[path = "../src/search.rs"]
#[allow(dead_code)]
mod search;
#[path = "../src/text.rs"]
#[allow(dead_code)]
mod text;
#[path = "../src/viewport.rs"]
#[allow(dead_code)]
mod viewport;

use blitz_dom::{BaseDocument, DocumentConfig, NodeId, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport as BlitzViewport};
use navigation::Navigation;
use search::{Search, SearchStatus};
use text::extract;
use viewport::{ResizeOutcome, Viewport};

fn fixture_html() -> String {
    let mut html = String::from(
        "<!doctype html><html><head><style>
         html, body { margin: 0; padding: 0; background: #ffffff; color: #101010; }
         body { font: 16px monospace; }
         h1, p { margin: 0 0 8px 0; }
         summary { display: block; }
         </style></head><body>
         <h1 id=\"top\">Search report</h1>
         <p id=\"split\">Search <strong>across</strong> <em>styled</em> <span>bound</span><span>aries</span>.</p>
         <p id=\"alpha\">Alpha wrapmark block.</p>
         <p id=\"beta\">Beta wrapmark block.</p>
         <p id=\"gamma\">Gamma wrapmark block.</p>",
    );
    for index in 0..20 {
        html.push_str(&format!(
            "<p>Filler paragraph {index} keeps search targets below the first viewport.</p>"
        ));
    }
    html.push_str(
        "<details id=\"outer\"><summary>Outer evidence</summary>\
         <p id=\"outer-lead\">Lead evidence paragraph.</p>\
         <details id=\"inner\"><summary>Inner evidence</summary>\
         <p id=\"hidden-target\">Collapsed evidence mentions needlepoint inside a nested disclosure.</p>\
         </details></details>\
         <p>Trailing paragraph keeps the document taller than the viewport.</p></body></html>",
    );
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

fn is_open(viewport: &Viewport, id: NodeId) -> bool {
    viewport
        .document()
        .get_node(id)
        .is_some_and(|node| node.data.has_attr(local_name!("open")))
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

fn commit(
    search: &mut Search,
    viewport: &mut Viewport,
    navigation: &mut Navigation,
) -> SearchStatus {
    let text = extract(viewport.document());
    search.commit(&text, viewport, navigation)
}

#[test]
fn prompt_edit_commit_and_cancel_retain_the_previous_query() {
    let mut viewport = viewport(200, 120);
    let mut navigation = Navigation::new();
    let mut search = Search::new();

    assert_eq!(search.status(), &SearchStatus::Idle);
    assert!(!search.is_editing());

    search.begin();
    assert!(search.is_editing());
    assert_eq!(
        search.status(),
        &SearchStatus::Prompt {
            text: String::new()
        }
    );

    for ch in "wrap".chars() {
        assert!(search.push_char(ch));
    }
    assert!(
        !search.push_char('\n'),
        "control characters stay with the pager"
    );
    assert!(search.backspace());
    assert!(search.push_char('p'));
    for ch in "mark".chars() {
        assert!(search.push_char(ch));
    }
    assert_eq!(search.prompt_text(), "wrapmark");
    assert_eq!(
        search.status(),
        &SearchStatus::Prompt {
            text: "wrapmark".to_string()
        }
    );

    assert_eq!(
        search.commit(
            &extract(viewport.document()),
            &mut viewport,
            &mut navigation
        ),
        SearchStatus::Match {
            query: "wrapmark".to_string(),
            current: 1,
            total: 3,
        }
    );
    assert!(!search.is_editing());
    assert_eq!(search.query(), "wrapmark");
    let matches = search.match_nodes().to_vec();
    let index = search.current_index();
    let status = search.status().clone();

    search.begin();
    for ch in "absent".chars() {
        search.push_char(ch);
    }
    assert_eq!(search.cancel(), status);
    assert!(!search.is_editing());
    assert_eq!(search.prompt_text(), "");
    assert_eq!(search.query(), "wrapmark");
    assert_eq!(search.match_nodes(), matches);
    assert_eq!(search.current_index(), index);
}

#[test]
fn cancel_without_a_prior_query_returns_to_idle() {
    let mut search = Search::new();
    search.begin();
    search.push_char('x');
    assert_eq!(search.cancel(), SearchStatus::Idle);
    assert_eq!(search.query(), "");
    assert_eq!(search.match_count(), 0);
}

#[test]
fn split_inline_text_matches_case_insensitively_as_one_block() {
    let mut viewport = viewport(220, 130);
    let mut navigation = Navigation::new();
    let split = element(&viewport, "split");
    let mut search = Search::new();

    search.begin();
    for ch in "across styled boundaries".chars() {
        search.push_char(ch);
    }
    let status = commit(&mut search, &mut viewport, &mut navigation);

    assert_eq!(
        status,
        SearchStatus::Match {
            query: "across styled boundaries".to_string(),
            current: 1,
            total: 1,
        }
    );
    assert_eq!(search.match_nodes(), &[split]);

    search.begin();
    for ch in "ACROSS STYLED".chars() {
        search.push_char(ch);
    }
    let status = commit(&mut search, &mut viewport, &mut navigation);

    assert_eq!(
        status,
        SearchStatus::Match {
            query: "ACROSS STYLED".to_string(),
            current: 1,
            total: 1,
        }
    );
    assert_eq!(search.match_nodes(), &[split]);
}

#[test]
fn counts_and_both_direction_wrap_are_reported() {
    let mut viewport = viewport(200, 120);
    let mut navigation = Navigation::new();
    let alpha = element(&viewport, "alpha");
    let beta = element(&viewport, "beta");
    let gamma = element(&viewport, "gamma");
    let mut search = Search::new();

    search.begin();
    for ch in "wrapmark".chars() {
        search.push_char(ch);
    }
    let status = commit(&mut search, &mut viewport, &mut navigation);

    assert_eq!(search.match_count(), 3);
    assert_eq!(search.match_nodes(), &[alpha, beta, gamma]);
    assert_eq!(search.position(), Some((1, 3)));
    assert_eq!(
        status,
        SearchStatus::Match {
            query: "wrapmark".to_string(),
            current: 1,
            total: 3,
        }
    );

    assert_eq!(
        search.next(&mut viewport, &mut navigation).unwrap().node,
        beta
    );
    assert_eq!(search.position(), Some((2, 3)));
    assert_eq!(
        search.next(&mut viewport, &mut navigation).unwrap().node,
        gamma
    );
    assert_eq!(
        search.next(&mut viewport, &mut navigation).unwrap().node,
        alpha
    );
    assert_eq!(search.position(), Some((1, 3)));

    assert_eq!(
        search
            .previous(&mut viewport, &mut navigation)
            .unwrap()
            .node,
        gamma
    );
    assert_eq!(search.position(), Some((3, 3)));
    assert_eq!(
        search
            .previous(&mut viewport, &mut navigation)
            .unwrap()
            .node,
        beta
    );
    assert_eq!(search.position(), Some((2, 3)));
}

#[test]
fn commit_reveals_a_match_inside_closed_disclosures() {
    let mut viewport = viewport(200, 120);
    let mut navigation = Navigation::new();
    let outer = element(&viewport, "outer");
    let inner = element(&viewport, "inner");
    let target = element(&viewport, "hidden-target");
    let mut search = Search::new();

    assert!(!is_open(&viewport, outer));
    assert!(!is_open(&viewport, inner));

    search.begin();
    for ch in "needlepoint".chars() {
        search.push_char(ch);
    }
    let status = commit(&mut search, &mut viewport, &mut navigation);

    assert_eq!(
        status,
        SearchStatus::Match {
            query: "needlepoint".to_string(),
            current: 1,
            total: 1,
        }
    );
    assert_eq!(search.current_match(), Some(target));
    assert!(is_open(&viewport, outer));
    assert!(is_open(&viewport, inner));
    assert!(target_visible(&mut viewport, target));

    let outcome = search
        .reveal_current(&mut viewport, &mut navigation)
        .expect("current match reveals");
    assert_eq!(outcome.node, target);
    assert!(outcome.aligned);
}

#[test]
fn empty_and_unmatched_queries_leave_the_viewer_usable() {
    let mut viewport = viewport(200, 120);
    let mut navigation = Navigation::new();
    let mut search = Search::new();

    search.begin();
    for ch in "absentneedle".chars() {
        search.push_char(ch);
    }
    assert_eq!(
        commit(&mut search, &mut viewport, &mut navigation),
        SearchStatus::NoMatches {
            query: "absentneedle".to_string()
        }
    );
    assert_eq!(search.match_count(), 0);
    assert_eq!(search.current_index(), None);
    assert_eq!(search.position(), None);
    assert_eq!(search.next(&mut viewport, &mut navigation), None);
    assert_eq!(
        search.status(),
        &SearchStatus::NoMatches {
            query: "absentneedle".to_string()
        }
    );

    assert!(viewport.scroll_by(40.0), "scrolling still works");
    assert!(viewport.scroll_y() > 0.0);

    search.begin();
    assert_eq!(
        commit(&mut search, &mut viewport, &mut navigation),
        SearchStatus::EmptyQuery
    );
    assert_eq!(search.query(), "");
    assert_eq!(search.match_count(), 0);
    assert_eq!(search.next(&mut viewport, &mut navigation), None);

    search.begin();
    for ch in "wrapmark".chars() {
        search.push_char(ch);
    }
    assert_eq!(
        commit(&mut search, &mut viewport, &mut navigation),
        SearchStatus::Match {
            query: "wrapmark".to_string(),
            current: 1,
            total: 3,
        }
    );
}

#[test]
fn query_matches_and_position_survive_viewport_resize() {
    let mut viewport = viewport(220, 130);
    let mut navigation = Navigation::new();
    let mut search = Search::new();

    search.begin();
    for ch in "wrapmark".chars() {
        search.push_char(ch);
    }
    commit(&mut search, &mut viewport, &mut navigation);
    let matches = search.match_nodes().to_vec();
    let status = search.status().clone();

    let ResizeOutcome::Applied(_) = viewport.resize(150, 140, 1.0) else {
        panic!("valid narrow resize was skipped");
    };
    let ResizeOutcome::Applied(_) = viewport.resize(240, 160, 1.0) else {
        panic!("valid wide resize was skipped");
    };

    assert_eq!(search.query(), "wrapmark");
    assert_eq!(search.match_nodes(), matches);
    assert_eq!(search.match_count(), 3);
    assert_eq!(search.current_index(), Some(0));
    assert_eq!(search.status(), &status);

    assert_eq!(
        search
            .previous(&mut viewport, &mut navigation)
            .unwrap()
            .node,
        element(&viewport, "gamma")
    );
    assert_eq!(
        search.next(&mut viewport, &mut navigation).unwrap().node,
        element(&viewport, "alpha")
    );
}
