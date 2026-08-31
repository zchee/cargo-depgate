//! The README is part of the contract: its section markers, its one configuration example, and
//! the `src/graph.rs` design rationale it points at are all asserted here (AC 19, AC 21).
#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{fs, path::PathBuf};

use cargo_depgate::config;

/// The repository root, which is where `README.md` and `src/` live.
fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn readme() -> String {
    fs::read_to_string(repository_root().join("README.md")).expect("README.md should be readable")
}

/// Every fenced TOML block in `text`, in document order.
fn toml_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("```toml\n") {
        let body = &rest[open + "```toml\n".len()..];
        let close = body.find("\n```").expect("a README TOML fence must be closed");
        blocks.push(&body[..close]);
        rest = &body[close + "\n```".len()..];
    }
    blocks
}

/// The README's configuration example is the documentation of `depgate.toml`, so it has to be a
/// file the tool would actually accept: it is written to disk and taken through the same
/// `load` + Phase-A `validate` path a real `--config` file takes.
#[test]
fn readme_configuration_example_parses_and_validates() {
    let text = readme();
    let blocks = toml_blocks(&text);
    assert_eq!(
        blocks.len(),
        1,
        "README should carry exactly one TOML example, the complete depgate.toml"
    );

    let temp = tempfile::tempdir().expect("a temporary directory should be creatable");
    let path = temp.path().join("depgate.toml");
    fs::write(&path, blocks[0]).expect("the README example should be writable");

    let raw = config::load(&path).expect("the README example should parse as depgate.toml");
    let validated =
        config::validate(&raw, None).expect("the README example should pass Phase-A validation");

    assert!(
        validated.config.manifest_versions_in_root,
        "the example should enable manifest checks"
    );

    // Rule ids are `rules.<package>.<kind>`, so the trailing segment names the kind. The example
    // is meant to show all six, which is what makes it a reference rather than a sample.
    for kind in ["deny", "require", "internal", "leaf", "direct", "sealed"] {
        let suffix = format!(".{kind}");
        assert!(
            validated.config.rules.iter().any(|rule| rule.id.ends_with(&suffix)),
            "the README example should demonstrate a {kind} rule"
        );
    }
}

#[test]
fn readme_keeps_the_contract_markers_and_graph_rationale() {
    let text = readme();
    for marker in [
        "<!-- depgate:semantics -->",
        "<!-- depgate:gap-table -->",
        "<!-- depgate:exit-codes -->",
        "<!-- depgate:ci -->",
        "<!-- depgate:version-blind -->",
        "<!-- depgate:codeowners -->",
    ] {
        assert!(text.contains(marker), "README is missing the contract marker {marker}");
    }

    let graph = fs::read_to_string(repository_root().join("src/graph.rs"))
        .expect("src/graph.rs should be readable");
    assert!(
        graph.contains("Why a hand-rolled CSR and bitsets instead of petgraph or guppy"),
        "src/graph.rs should keep the CSR/bitset design rationale in its module documentation"
    );
}
