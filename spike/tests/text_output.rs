//! Tests for semantic text extraction and plain `--text` rendering.
//!
//! Bead `glisk-2hp.6` owns this module and `spike/src/text.rs`. The wiring
//! beads add `mod text;` to the binary and reuse the same block list for search
//! and text output.

#[path = "../src/document.rs"]
#[allow(dead_code)]
mod document;
#[path = "../src/text.rs"]
#[allow(dead_code)]
mod text;

use blitz_dom::BaseDocument;
use blitz_traits::shell::{ColorScheme, Viewport};
use document::{Limits, open_document};
use text::{BlockKind, extract, extract_plain_text};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../examples/decision-report.html"
);

fn load(html: &str) -> BaseDocument {
    open_document(
        html,
        Viewport::new(420, 900, 1.0, ColorScheme::Dark),
        Limits::default(),
    )
    .expect("document opens")
    .document
}

fn fixture() -> BaseDocument {
    let html = std::fs::read_to_string(FIXTURE).expect("fixture readable");
    load(&html)
}

#[test]
fn reference_report_has_readable_structure_and_evidence() {
    let document = fixture();
    let extracted = extract(&document);
    let plain = extracted.plain_text();

    assert!(plain.contains("# Glisk renderer decision report"));
    assert!(plain.contains("## Summary"));
    assert!(plain.contains("### Recommended path"));
    assert!(plain.contains(
        "Blitz with the Vello CPU rasterizer keeps the document model and the renderer in one Rust process."
    ));

    // The headered table keeps row and header order.
    assert!(plain.contains("| Option | Layout fidelity | Weight and latency | Verdict |"));
    assert!(plain.contains(
        "| Blitz + Vello CPU | Embedded HTML/CSS, flex, grid, partial tables, inline SVG |"
    ));
    assert!(plain.contains("| Headless Chrome | Full browser behavior |"));

    // The inline SVG title and description become a readable chart summary,
    // followed by the authored caption and summary list.
    assert!(plain.contains(
        "Chart: Startup latency comparison. Bar chart of cold startup in milliseconds: Blitz spike target 120, headless Chrome warm 520, headless Chrome cold 1230."
    ));
    assert!(plain.contains("Figure 1. Cold startup comparison"));
    assert!(plain.contains("- The chart is decorative once the text summary is present"));

    // Both disclosure bodies are present even though the fixture leaves the
    // <details> elements closed.
    assert!(plain.contains("Evidence 1: how the renderer was exercised"));
    assert!(
        plain.contains("The pinned prototype loads this same file with external fetches denied.")
    );
    assert!(plain.contains("Evidence 2: known gaps and resolution conditions"));
    assert!(plain.contains(
        "Blitz has no JavaScript and supports only part of the table and overflow model."
    ));
    assert!(plain.contains("If a narrow viewport clips the table"));

    // Authored CSS is not document text.
    assert!(!plain.contains("color-scheme"));
    assert!(!plain.contains("box-sizing"));
    assert!(!plain.contains("border-radius"));

    // Order follows the semantic DOM.
    let evidence = plain.find("## Evidence").expect("evidence heading");
    let method = plain.find("Evidence 1").expect("first summary");
    let gaps = plain.find("Evidence 2").expect("second summary");
    assert!(evidence < method && method < gaps);
}

#[test]
fn inline_styling_is_combined_into_one_searchable_block() {
    let document = load(
        r#"<html><body>
            <p id="search">Search <strong>across</strong> <em>styled</em> <span>bound</span><span>aries</span>.</p>
        </body></html>"#,
    );
    let extracted = extract(&document);
    let block = extracted
        .blocks()
        .iter()
        .find(|block| block.kind == BlockKind::Paragraph)
        .expect("paragraph block");

    assert_eq!(block.text, "Search across styled boundaries.");
    assert_eq!(block.search_text, "Search across styled boundaries.");
    assert_eq!(block.node, document.get_element_by_id("search").unwrap());
    assert!(block.matches("ACROSS STYLED"));
    assert!(block.matches("boundaries"));

    let matches: Vec<_> = extracted.matching_blocks("styled boundaries").collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].node, block.node);
}

#[test]
fn hidden_and_non_content_elements_are_skipped() {
    let document = load(
        r#"<html>
            <head>
                <title>head title marker</title>
                <style>.gone { display: none; }</style>
            </head>
            <body>
                <script>script marker</script>
                <style>style marker</style>
                <template><p>template marker</p></template>
                <div hidden>hidden attribute marker</div>
                <div style="display: none">inline display marker</div>
                <div style="visibility: hidden">inline visibility marker</div>
                <div class="gone">css class marker</div>
                <div aria-hidden="true">aria marker</div>
                <p>visible marker</p>
            </body>
        </html>"#,
    );
    let plain = extract_plain_text(&document);

    assert!(plain.contains("visible marker"));
    for marker in [
        "head title marker",
        "script marker",
        "style marker",
        "template marker",
        "hidden attribute marker",
        "inline display marker",
        "inline visibility marker",
        "css class marker",
        "aria marker",
    ] {
        assert!(
            !plain.contains(marker),
            "unexpected {marker:?} in {plain:?}"
        );
    }
}

#[test]
fn closed_disclosure_bodies_are_extracted_in_order() {
    let document = load(
        r#"<html><body>
            <details id="method">
                <summary>Method summary</summary>
                <p>Closed body evidence</p>
                <ul><li>Nested evidence</li></ul>
            </details>
            <details id="gaps" open>
                <summary>Gaps summary</summary>
                <p>Open body evidence</p>
            </details>
        </body></html>"#,
    );
    let extracted = extract(&document);
    let plain = extracted.plain_text();

    for expected in [
        "Method summary",
        "Closed body evidence",
        "- Nested evidence",
        "Gaps summary",
        "Open body evidence",
    ] {
        assert!(
            plain.contains(expected),
            "missing {expected:?} in {plain:?}"
        );
    }
    let method = plain.find("Method summary").unwrap();
    let body = plain.find("Closed body evidence").unwrap();
    let nested = plain.find("- Nested evidence").unwrap();
    let gaps = plain.find("Gaps summary").unwrap();
    assert!(method < body && body < nested && nested < gaps);

    let summaries: Vec<_> = extracted
        .blocks()
        .iter()
        .filter(|block| block.kind == BlockKind::Summary)
        .map(|block| block.text.as_str())
        .collect();
    assert_eq!(summaries, vec!["Method summary", "Gaps summary"]);
    let summary_node = document
        .get_element_by_id("method")
        .and_then(|details| document.get_node(details))
        .and_then(|details| {
            details
                .children
                .iter()
                .find(|child| {
                    document
                        .get_node(**child)
                        .and_then(|node| node.element_data())
                        .is_some_and(|element| {
                            let local: &str = element.name.local.as_ref();
                            local == "summary"
                        })
                })
                .copied()
        });
    assert_eq!(
        extracted
            .blocks()
            .iter()
            .find(|block| block.kind == BlockKind::Summary)
            .map(|block| block.node),
        summary_node
    );
}

#[test]
fn lists_tables_and_code_keep_useful_indentation() {
    let document = load(
        r#"<html><body>
            <ul>
                <li>Top item
                    <ul>
                        <li>Nested item
                            <ul><li>Deep item</li></ul>
                        </li>
                    </ul>
                </li>
                <li>Second item</li>
            </ul>
            <pre id="code">if ready {
    run();
}</pre>
            <table>
                <caption>Example table</caption>
                <thead><tr><th>Name</th><th>Value</th></tr></thead>
                <tbody><tr><td>alpha</td><td>1</td></tr></tbody>
            </table>
        </body></html>"#,
    );
    let extracted = extract(&document);
    let plain = extracted.plain_text();

    assert!(plain.contains("- Top item"));
    assert!(plain.contains("  - Nested item"));
    assert!(plain.contains("    - Deep item"));
    assert!(plain.contains("- Second item"));
    assert!(plain.contains("if ready {\n    run();\n}"));
    assert!(plain.contains("| Name | Value |"));
    assert!(plain.contains("| alpha | 1 |"));
    assert!(plain.find("| Name | Value |").unwrap() < plain.find("| alpha | 1 |").unwrap());

    let depths: Vec<_> = extracted
        .blocks()
        .iter()
        .filter(|block| block.kind == BlockKind::ListItem)
        .map(|block| (block.text.as_str(), block.depth))
        .collect();
    assert_eq!(
        depths,
        vec![
            ("Top item", 1),
            ("Nested item", 2),
            ("Deep item", 3),
            ("Second item", 1),
        ]
    );

    let code = extracted
        .blocks()
        .iter()
        .find(|block| block.kind == BlockKind::Code)
        .expect("code block");
    assert_eq!(code.text, "if ready {\n    run();\n}");
    assert_eq!(code.search_text, "if ready { run(); }");
}

#[test]
fn image_text_and_captions_are_preserved() {
    let document = load(
        r#"<html><body>
            <p>See <img alt="Embedded PNG preview"> now.</p>
            <figure>
                <img alt="Standalone diagram">
                <figcaption>Figure caption text</figcaption>
            </figure>
        </body></html>"#,
    );
    let extracted = extract(&document);
    let plain = extracted.plain_text();

    assert!(plain.contains("See Embedded PNG preview now."));
    assert!(plain.contains("[image: Standalone diagram]"));
    assert!(plain.contains("Figure caption text"));
    assert!(
        extracted
            .blocks()
            .iter()
            .any(|block| block.kind == BlockKind::Image && block.text == "Standalone diagram")
    );
    assert!(
        extracted
            .blocks()
            .iter()
            .any(|block| block.kind == BlockKind::Caption && block.text == "Figure caption text")
    );
}

#[test]
fn document_controls_are_removed_from_every_output_path() {
    let html = format!(
        r#"<html><body>
            <p id="text">safe{esc}red{bell}text</p>
            <pre id="code">line{esc}one{del}next{c1}</pre>
            <img alt="alt{esc}text">
        </body></html>"#,
        esc = '\u{1b}',
        bell = '\u{7}',
        del = '\u{7f}',
        c1 = '\u{9b}',
    );
    let document = load(&html);
    let plain = extract_plain_text(&document);

    assert!(plain.contains("saferedtext"));
    assert!(plain.contains("lineonenext"));
    assert!(plain.contains("[image: alttext]"));
    assert!(
        plain.chars().all(|ch| ch == '\n' || !ch.is_control()),
        "control character survived: {plain:?}"
    );
    for forbidden in ['\u{1b}', '\u{7}', '\u{7f}', '\u{9b}'] {
        assert!(!plain.contains(forbidden), "{forbidden:?} survived");
    }
}

#[test]
fn extraction_is_reproducible_without_a_terminal() {
    let document = fixture();
    let first = extract(&document);
    let second = extract(&document);
    assert_eq!(first, second);
    assert_eq!(first.plain_text(), extract_plain_text(&document));
    assert!(!first.is_empty());
    assert!(first.plain_text().ends_with('\n'));
}
