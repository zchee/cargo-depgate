#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::time::Duration;

use super::*;

#[test]
fn phases_are_reported_in_pipeline_order_with_stable_labels() {
    let labels: Vec<&str> = Phase::ALL.iter().map(|phase| phase.label()).collect();

    assert_eq!(
        labels,
        ["read", "parse", "graph", "traversals", "evaluate", "manifest", "report", "total"]
    );
}

#[test]
fn no_two_timings_lines_share_a_first_token() {
    let mut out = Vec::new();
    Timings::default().write_to(&Counters::default(), &mut out).expect("write");

    let text = String::from_utf8(out).expect("ASCII output");
    let mut seen = std::collections::BTreeSet::new();
    for line in text.lines() {
        let (token, _) = line.split_once('\t').expect("tab-separated line");
        assert!(seen.insert(token), "first token {token:?} appears twice: {text}");
    }
    let phase_labels: std::collections::BTreeSet<&str> =
        Phase::ALL.iter().map(|phase| phase.label()).collect();
    for (counter, _) in Counters::default().entries() {
        assert!(!phase_labels.contains(counter), "counter {counter:?} is also a phase label");
    }
}

#[test]
fn write_to_prints_one_tab_separated_line_per_phase_then_per_counter() {
    let mut timings = Timings::start();
    timings.add(Phase::Read, Duration::from_micros(1500));
    timings.add(Phase::Parse, Duration::from_micros(2345));
    timings.add(Phase::Parse, Duration::from_micros(5));
    timings.add(Phase::Graph, Duration::from_nanos(123_456));
    // Ten deliberately distinct, arbitrary values: this test checks only that `write_to` emits
    // one tab-separated line per phase and then per counter, in order, so what matters is that no
    // two fields share a value and a swap would be visible. They are not any workspace's counters.
    let counters = Counters {
        packages: 585,
        members: 14,
        normal_edges: 1586,
        names: 529,
        superset_extra_edges: 7,
        direct_optional_decls: 1,
        unrebased_path_deps: 2,
        rules: 19,
        violations: 3,
        matches: 9,
    };
    let mut out = Vec::new();

    timings.write_to(&counters, &mut out).expect("writing to a Vec cannot fail");

    assert_eq!(
        String::from_utf8(out).expect("ASCII output"),
        "read\t1.500\n\
         parse\t2.350\n\
         graph\t0.123\n\
         traversals\t0.000\n\
         evaluate\t0.000\n\
         manifest\t0.000\n\
         report\t0.000\n\
         total\t0.000\n\
         packages\t585\n\
         members\t14\n\
         normal_edges\t1586\n\
         names\t529\n\
         superset_extra_edges\t7\n\
         direct_optional_decls\t1\n\
         unrebased_path_deps\t2\n\
         rules\t19\n\
         violations\t3\n\
         matches\t9\n"
    );
}

#[test]
fn every_line_splits_into_exactly_two_fields_on_the_tab() {
    let mut out = Vec::new();
    Timings::default().write_to(&Counters::default(), &mut out).expect("write");

    let text = String::from_utf8(out).expect("ASCII output");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), Phase::ALL.len() + Counters::default().entries().count());
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 2, "malformed line {line:?}");
        assert!(fields[1].parse::<f64>().is_ok(), "non-numeric value in {line:?}");
    }
}

#[test]
fn measure_records_elapsed_time_and_finish_sets_total() {
    let mut timings = Timings::start();

    let value = timings.measure(Phase::Graph, || {
        std::thread::sleep(Duration::from_millis(2));
        42
    });
    timings.finish();

    assert_eq!(value, 42);
    assert!(timings.millis(Phase::Graph) >= 2.0, "{}", timings.millis(Phase::Graph));
    assert!(timings.millis(Phase::Total) >= timings.millis(Phase::Graph));
    assert!(timings.millis(Phase::Read).abs() < f64::EPSILON);
}

#[test]
fn finish_is_idempotent_and_refreshes_total_from_wall_time() {
    let mut timings = Timings::start();
    timings.finish();
    let first = timings.millis(Phase::Total);

    std::thread::sleep(Duration::from_millis(2));
    timings.finish();
    let second = timings.millis(Phase::Total);

    assert!(second >= first, "first={first}, second={second}");
}

#[test]
fn counter_entries_follow_the_json_counters_order() {
    let labels: Vec<&str> = Counters::default().entries().map(|(label, _)| label).collect();

    assert_eq!(
        labels,
        [
            "packages",
            "members",
            "normal_edges",
            "names",
            "superset_extra_edges",
            "direct_optional_decls",
            "unrebased_path_deps",
            "rules",
            "violations",
            "matches",
        ]
    );
}
