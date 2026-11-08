//! The conformance suite: scripted scenarios (inputs at given wall-clock times → the exact commands
//! expected) stored as JSON in `crates/aun-player/conformance/*.json` and compiled into the crate.
//!
//! The Rust runner ([`run_scenario`]) proves the model matches the spec. Native wrappers replay the
//! same JSON (via the FFI `player_conformance_scenarios_json()`) against their command executor to
//! prove they honour every command; see `docs/PLAYER_BEHAVIOUR.md` §10.

use serde::{Deserialize, Serialize};

use crate::machine::Player;
use crate::model::{PlaybackState, PlayerCommand, PlayerConfig, PlayerInput, RepeatMode, SleepTimer};

/// The fixture files, in a stable order (file name, JSON).
pub const SCENARIO_FILES: &[(&str, &str)] = &[
    ("queue.json", include_str!("../conformance/queue.json")),
    ("resume.json", include_str!("../conformance/resume.json")),
    ("transport.json", include_str!("../conformance/transport.json")),
    ("quality.json", include_str!("../conformance/quality.json")),
    ("study.json", include_str!("../conformance/study.json")),
    ("sleep.json", include_str!("../conformance/sleep.json")),
    ("chapters.json", include_str!("../conformance/chapters.json")),
];

/// One fixture file: a list of scenarios.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioFile {
    pub scenarios: Vec<Scenario>,
}

/// A named script.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Overrides of [`PlayerConfig`]; omitted fields keep their defaults.
    #[serde(default)]
    pub config: PlayerConfig,
    pub steps: Vec<Step>,
}

/// One input, the time it happens at and what must come out. `expect` is matched exactly and in
/// order; an omitted `expect` means "no commands".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub at_ms: i64,
    pub input: PlayerInput,
    #[serde(default)]
    pub expect: Vec<PlayerCommand>,
    #[serde(default)]
    pub expect_state: Option<ExpectState>,
}

/// Optional partial checks of the snapshot after a step.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExpectState {
    pub state: Option<PlaybackState>,
    pub current_item_id: Option<String>,
    pub position_ms: Option<u64>,
    pub rate: Option<f64>,
    pub repeat_mode: Option<RepeatMode>,
    pub sleep_timer: Option<SleepTimer>,
    pub has_ab_loop: Option<bool>,
    pub current_chapter: Option<u32>,
}

/// Why a scenario failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConformanceError {
    #[error("fixture {file}: {detail}")]
    Parse { file: String, detail: String },
    #[error("{scenario} step {step}: expected commands {expected} but got {actual}")]
    Commands {
        scenario: String,
        step: usize,
        expected: String,
        actual: String,
    },
    #[error("{scenario} step {step}: state mismatch: {detail}")]
    State {
        scenario: String,
        step: usize,
        detail: String,
    },
}

/// Parses every compiled-in fixture.
pub fn builtin_scenarios() -> Result<Vec<Scenario>, ConformanceError> {
    let mut all = Vec::new();
    for (file, json) in SCENARIO_FILES {
        all.extend(parse_scenarios(file, json)?);
    }
    Ok(all)
}

/// Every compiled-in scenario as one JSON array (what the FFI hands to native test suites).
pub fn builtin_scenarios_json() -> Result<String, ConformanceError> {
    let all = builtin_scenarios()?;
    serde_json::to_string(&all).map_err(|e| ConformanceError::Parse {
        file: "<builtin>".into(),
        detail: e.to_string(),
    })
}

/// Parses one fixture file.
pub fn parse_scenarios(file: &str, json: &str) -> Result<Vec<Scenario>, ConformanceError> {
    serde_json::from_str::<ScenarioFile>(json)
        .map(|f| f.scenarios)
        .map_err(|e| ConformanceError::Parse {
            file: file.into(),
            detail: e.to_string(),
        })
}

/// Replays a scenario against a fresh [`Player`].
pub fn run_scenario(scenario: &Scenario) -> Result<(), ConformanceError> {
    let mut player = Player::new(scenario.config.clone());
    for (i, step) in scenario.steps.iter().enumerate() {
        let actual = player.handle(step.input.clone(), step.at_ms);
        if actual != step.expect {
            return Err(ConformanceError::Commands {
                scenario: scenario.name.clone(),
                step: i,
                expected: to_json(&step.expect),
                actual: to_json(&actual),
            });
        }
        if let Some(expect) = &step.expect_state {
            check_state(&player, expect).map_err(|detail| ConformanceError::State {
                scenario: scenario.name.clone(),
                step: i,
                detail,
            })?;
        }
    }
    Ok(())
}

fn to_json(cmds: &[PlayerCommand]) -> String {
    serde_json::to_string(cmds).unwrap_or_else(|e| format!("<unserialisable: {e}>"))
}

fn check_state(player: &Player, e: &ExpectState) -> Result<(), String> {
    let s = player.snapshot();
    let mut problems = Vec::new();
    if let Some(v) = e.state
        && s.state != v
    {
        problems.push(format!("state {:?} != {:?}", s.state, v));
    }
    if let Some(v) = &e.current_item_id
        && s.current_item_id.as_ref() != Some(v)
    {
        problems.push(format!("current_item_id {:?} != {v:?}", s.current_item_id));
    }
    if let Some(v) = e.position_ms
        && s.position_ms != v
    {
        problems.push(format!("position_ms {} != {v}", s.position_ms));
    }
    if let Some(v) = e.rate
        && s.rate != v
    {
        problems.push(format!("rate {} != {v}", s.rate));
    }
    if let Some(v) = e.repeat_mode
        && s.repeat_mode != v
    {
        problems.push(format!("repeat_mode {:?} != {v:?}", s.repeat_mode));
    }
    if let Some(v) = e.sleep_timer
        && s.sleep_timer != v
    {
        problems.push(format!("sleep_timer {:?} != {v:?}", s.sleep_timer));
    }
    if let Some(v) = e.has_ab_loop
        && s.ab_loop.is_some() != v
    {
        problems.push(format!("ab_loop {:?}, expected present={v}", s.ab_loop));
    }
    if let Some(v) = e.current_chapter
        && s.current_chapter != Some(v)
    {
        problems.push(format!("current_chapter {:?} != {v}", s.current_chapter));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatch_is_reported() {
        let s = Scenario {
            name: "bad".into(),
            description: String::new(),
            config: PlayerConfig::default(),
            steps: vec![Step {
                at_ms: 0,
                input: PlayerInput::Play,
                expect: vec![PlayerCommand::Play],
                expect_state: None,
            }],
        };
        let err = run_scenario(&s).err();
        assert!(matches!(err, Some(ConformanceError::Commands { step: 0, .. })));
    }

    #[test]
    fn state_mismatch_is_reported() {
        let s = Scenario {
            name: "bad-state".into(),
            description: String::new(),
            config: PlayerConfig::default(),
            steps: vec![Step {
                at_ms: 0,
                input: PlayerInput::Play,
                expect: vec![],
                expect_state: Some(ExpectState {
                    state: Some(PlaybackState::Playing),
                    ..ExpectState::default()
                }),
            }],
        };
        assert!(matches!(run_scenario(&s), Err(ConformanceError::State { .. })));
    }

    #[test]
    fn parse_error_names_file() {
        let err = parse_scenarios("x.json", "{").err();
        assert!(matches!(err, Some(ConformanceError::Parse { file, .. }) if file == "x.json"));
    }
}
