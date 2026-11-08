#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Replays every scenario in `conformance/*.json` against the Rust model.

use std::collections::HashSet;

use aun_player::conformance::{
    SCENARIO_FILES, Scenario, builtin_scenarios, builtin_scenarios_json, parse_scenarios, run_scenario,
};

#[test]
fn every_builtin_scenario_passes() {
    let scenarios = builtin_scenarios().expect("fixtures parse");
    assert!(scenarios.len() >= 30, "suite shrank to {}", scenarios.len());
    let failures: Vec<String> = scenarios
        .iter()
        .filter_map(|s| run_scenario(s).err().map(|e| e.to_string()))
        .collect();
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

#[test]
fn scenario_names_are_unique_and_described() {
    let scenarios = builtin_scenarios().unwrap();
    let mut seen = HashSet::new();
    for s in &scenarios {
        assert!(seen.insert(s.name.clone()), "duplicate scenario {}", s.name);
        assert!(!s.description.is_empty(), "{} has no description", s.name);
        assert!(!s.steps.is_empty(), "{} has no steps", s.name);
    }
}

#[test]
fn fixture_directory_matches_compiled_list() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect();
    on_disk.sort();
    let mut compiled: Vec<String> = SCENARIO_FILES.iter().map(|(n, _)| (*n).to_owned()).collect();
    compiled.sort();
    assert_eq!(on_disk, compiled, "add new fixture files to SCENARIO_FILES");
}

#[test]
fn json_export_round_trips() {
    let json = builtin_scenarios_json().unwrap();
    let back: Vec<Scenario> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, builtin_scenarios().unwrap());
    // Every exported scenario still passes after the round trip (what native suites consume).
    for s in &back {
        run_scenario(s).unwrap();
    }
}

#[test]
fn custom_fixture_with_config_override() {
    let json = r#"{"scenarios":[{"name":"short_resume","description":"resume threshold 5 s",
        "config":{"resume_min_ms":5000},
        "steps":[{"at_ms":0,"input":{"type":"load_queue","items":[{"id":"a","duration_ms":60000,"resume_position_ms":6000}]},
                  "expect":[{"type":"load","item_id":"a","start_position_ms":6000,"play_when_ready":true},
                            {"type":"set_quality_cap","cap":{"max_height":1080}}]}]}]}"#;
    let scenarios = parse_scenarios("inline.json", json).unwrap();
    assert_eq!(scenarios[0].config.resume_min_ms, 5000);
    assert_eq!(scenarios[0].config.skip_ms, 10_000, "other defaults kept");
    run_scenario(&scenarios[0]).unwrap();
}
