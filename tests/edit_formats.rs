//! Edit/write round-trip tests for the CSV and HTML handlers.

use filetools_rs::patch::{Op, Patch};
use filetools_rs::{edit, extract};

fn hash_of(idmap: &filetools_rs::idmap::IdMap, id: &str) -> String {
    idmap.get(id).expect("id present").hash.clone()
}

#[test]
fn csv_replaces_a_cell_value_surgically() {
    let csv = b"name,age,city\nAlice,30,NYC\nBob,25,LA\n";
    let out = extract("data.csv", csv).unwrap();
    let idmap = out.idmap.clone().unwrap();

    // Replace Bob's age (row 2, col 1) with 26.
    let patch = Patch {
        patch: vec![
            Op::Test {
                path: "/structure/cell[2,1]".into(),
                hash: hash_of(&idmap, "cell[2,1]"),
            },
            Op::Replace {
                path: "/structure/cell[2,1]/text".into(),
                value: "26".into(),
            },
        ],
    };

    let result = edit(&out.envelope, &idmap, csv, &patch).unwrap();
    assert_eq!(
        std::str::from_utf8(&result).unwrap(),
        "name,age,city\nAlice,30,NYC\nBob,26,LA\n"
    );
}

#[test]
fn csv_quotes_value_when_it_contains_a_comma() {
    let csv = b"name,city\nAlice,NYC\n";
    let out = extract("data.csv", csv).unwrap();
    let idmap = out.idmap.clone().unwrap();

    let patch = Patch {
        patch: vec![Op::Replace {
            path: "/structure/cell[1,1]/text".into(),
            value: "New York, NY".into(),
        }],
    };

    let result = edit(&out.envelope, &idmap, csv, &patch).unwrap();
    assert_eq!(
        std::str::from_utf8(&result).unwrap(),
        "name,city\nAlice,\"New York, NY\"\n"
    );
}

#[test]
fn csv_edits_an_already_quoted_field() {
    let csv = b"name,note\nAlice,\"hi, there\"\n";
    let out = extract("data.csv", csv).unwrap();
    let idmap = out.idmap.clone().unwrap();

    // Cell text should have been unescaped on extract.
    let cell = out
        .envelope
        .structure
        .iter()
        .find(|n| n.id == "cell[1,1]")
        .unwrap();
    assert_eq!(cell.text.as_deref(), Some("hi, there"));

    let patch = Patch {
        patch: vec![Op::Replace {
            path: "/structure/cell[1,1]/text".into(),
            value: "bye".into(),
        }],
    };
    let result = edit(&out.envelope, &idmap, csv, &patch).unwrap();
    assert_eq!(
        std::str::from_utf8(&result).unwrap(),
        "name,note\nAlice,bye\n"
    );
}

#[test]
fn csv_stale_guard_aborts() {
    let csv = b"a,b\n1,2\n";
    let out = extract("data.csv", csv).unwrap();
    let idmap = out.idmap.clone().unwrap();

    let patch = Patch {
        patch: vec![
            Op::Test {
                path: "/structure/cell[1,0]".into(),
                hash: "sha256:deadbeef".into(),
            },
            Op::Replace {
                path: "/structure/cell[1,0]/text".into(),
                value: "9".into(),
            },
        ],
    };
    assert!(edit(&out.envelope, &idmap, csv, &patch).is_err());
}

#[test]
fn html_replaces_heading_text_surgically() {
    let html =
        b"<html><head><title>Old</title></head><body><h1>Intro</h1><p>hello</p></body></html>";
    let out = extract("page.html", html).unwrap();
    let idmap = out.idmap.clone().unwrap();

    let patch = Patch {
        patch: vec![Op::Replace {
            path: "/structure/section[0]/text".into(),
            value: "Introduction".into(),
        }],
    };
    let result = edit(&out.envelope, &idmap, html, &patch).unwrap();
    assert_eq!(
        std::str::from_utf8(&result).unwrap(),
        "<html><head><title>Old</title></head><body><h1>Introduction</h1><p>hello</p></body></html>"
    );
}

#[test]
fn html_replaces_title_and_paragraph() {
    let html =
        b"<html><head><title>Old</title></head><body><h1>H</h1><p>one</p><p>two</p></body></html>";
    let out = extract("page.html", html).unwrap();
    let idmap = out.idmap.clone().unwrap();

    let patch = Patch {
        patch: vec![
            Op::Replace {
                path: "/structure/title/text".into(),
                value: "New".into(),
            },
            Op::Replace {
                path: "/structure/paragraph[1]/text".into(),
                value: "second".into(),
            },
        ],
    };
    let result = edit(&out.envelope, &idmap, html, &patch).unwrap();
    assert_eq!(
        std::str::from_utf8(&result).unwrap(),
        "<html><head><title>New</title></head><body><h1>H</h1><p>one</p><p>second</p></body></html>"
    );
}

#[test]
fn html_escapes_special_characters_in_replacement() {
    let html = b"<body><p>plain</p></body>";
    let out = extract("page.html", html).unwrap();
    let idmap = out.idmap.clone().unwrap();

    let patch = Patch {
        patch: vec![Op::Replace {
            path: "/structure/paragraph[0]/text".into(),
            value: "a < b & c".into(),
        }],
    };
    let result = edit(&out.envelope, &idmap, html, &patch).unwrap();
    assert_eq!(
        std::str::from_utf8(&result).unwrap(),
        "<body><p>a &lt; b &amp; c</p></body>"
    );
}
