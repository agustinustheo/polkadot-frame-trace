//! Exercise both diff input modes and CI exit codes with real offline blocks.
#![allow(clippy::unwrap_used)]

use polkadot_frame_trace::replay::{Artifact, rpc};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/replay")
        .join(name)
}

fn command(subcommand: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_polkadot-frame-trace"));
    cmd.arg(subcommand);
    cmd
}

fn run(command: &mut Command, expected: i32) -> Output {
    let out = command.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(expected),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("frame-trace-diff-{}-{tag}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn block_json(name: &str, path: &Path) {
    run(
        command("block")
            .arg("--offline")
            .arg(fixture(name))
            .arg("--json")
            .arg(path),
        0,
    );
}

fn tables(text: &str) {
    for header in [
        "Overview",
        "Phase deltas",
        "Top item deltas",
        "reads (trie: new)",
        "duplicate reads",
    ] {
        assert!(text.contains(header), "{header} missing from {text}");
    }
}

#[test]
fn saved_blocks_show_tables_and_enforce_proof_budgets() {
    let scratch = Scratch::new("blocks");
    let before = scratch.file("before.json");
    let after = scratch.file("after.json");
    block_json("people-transfer", &before);
    block_json("people-traced", &after);
    let diff = || {
        let mut cmd = command("diff");
        cmd.arg(&before).arg(&after);
        cmd
    };

    let out = run(diff().arg("--json").arg("-"), 0);
    tables(&String::from_utf8_lossy(&out.stderr));
    let complete: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(complete["schema_version"], 1);
    assert_eq!(complete["before"]["block_number"], 2);
    assert_eq!(complete["after"]["block_number"], 4);
    assert!(complete["items"].as_array().unwrap().len() > 1);
    assert!(complete["phases"].as_array().unwrap().len() > 1);
    assert_eq!(complete["regressions"], serde_json::json!([]));
    let proof = &complete["totals"]["proof_bytes"];
    let delta = proof["delta"].as_i64().unwrap();
    assert_eq!(
        delta,
        proof["after"].as_i64().unwrap() - proof["before"].as_i64().unwrap()
    );
    assert_ne!(delta, 0);

    let out = run(diff().arg("--max-proof-delta").arg(delta.to_string()), 0);
    tables(&String::from_utf8_lossy(&out.stdout));

    let out = run(
        diff()
            .args(["--top", "0", "--json", "-", "--max-proof-delta"])
            .arg((delta - 1).to_string()),
        4,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    tables(&stderr);
    assert!(
        stderr.contains("more items") && stderr.contains("more extrinsics"),
        "{stderr}"
    );
    assert!(!stderr.contains("extrinsic 0:"), "{stderr}");
    assert!(stderr.contains("regression: proof bytes delta"), "{stderr}");
    let limited: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(limited["items"], complete["items"]);
    assert_eq!(limited["phases"], complete["phases"]);
    assert_eq!(limited["regressions"][0]["delta"], delta);
    assert_eq!(limited["regressions"][0]["max"], delta - 1);

    let file = scratch.file("diff.json");
    let out = run(
        diff()
            .arg("--max-proof-delta")
            .arg((delta - 1).to_string())
            .arg("--json")
            .arg(&file),
        4,
    );
    tables(&String::from_utf8_lossy(&out.stdout));
    let saved: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(saved["regressions"], limited["regressions"]);

    let out = run(
        command("diff").arg(&before).arg(&before).args([
            "--max-ops-delta",
            "-1",
            "--max-duplicate-reads-delta",
            "-1",
        ]),
        4,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("regression: operations delta 0 exceeds -1"),
        "{stderr}"
    );
    assert!(
        stderr.contains("regression: duplicate reads delta 0 exceeds -1"),
        "{stderr}"
    );
}

#[test]
fn offline_ab_replays_both_runtimes() {
    let scratch = Scratch::new("offline");
    let artifact = Artifact::load(&fixture("people-traced")).unwrap();
    // The :code value is the largest proof node in this fixture.
    let wasm = artifact
        .proof_nodes
        .iter()
        .max_by_key(|node| node.len())
        .unwrap();
    let wasm_path = scratch.file("candidate.wasm");
    fs::write(&wasm_path, rpc::unhex(wasm).unwrap()).unwrap();
    let out = run(
        command("diff")
            .arg("--offline")
            .arg(fixture("people-transfer"))
            .arg("--runtime-override")
            .arg(&wasm_path)
            .args(["--sort-by", "duplicates", "--json", "-"]),
        0,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    tables(&stderr);
    assert_eq!(stderr.matches("root check: PASS").count(), 2, "{stderr}");
    assert!(
        stderr.contains("root check: PASS with RUNTIME OVERRIDE"),
        "{stderr}"
    );
    let diff: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(diff["before"]["block_number"], 2);
    assert_eq!(diff["before"]["block_hash"], diff["after"]["block_hash"]);
    assert_eq!(diff["totals"]["proof_bytes"]["delta"], 0);
    assert!(diff["before"]["runtime_override"].is_null());
    assert_eq!(
        diff["after"]["runtime_override"]["path"],
        wasm_path.to_str().unwrap()
    );
    assert_ne!(
        diff["after"]["runtime_override"]["chain_code_hash"],
        diff["after"]["runtime_override"]["code_hash"],
    );
    assert_eq!(diff["regressions"], serde_json::json!([]));
}

#[test]
fn invalid_input_modes_are_clap_errors() {
    for args in [
        &[][..],
        &["before.json"],
        &["before.json", "after.json", "extra.json"],
        &["--offline", "fixture"],
        &["--runtime-override", "candidate.wasm"],
        &[
            "before.json",
            "after.json",
            "--runtime-override",
            "candidate.wasm",
        ],
        &[
            "before.json",
            "after.json",
            "--offline",
            "fixture",
            "--runtime-override",
            "candidate.wasm",
        ],
        &[
            "before.json",
            "--offline",
            "fixture",
            "--runtime-override",
            "candidate.wasm",
        ],
        &["before.json", "after.json", "--sort-by", "time"],
    ] {
        let out = run(command("diff").args(args), 2);
        assert!(String::from_utf8_lossy(&out.stderr).starts_with("error:"));
    }
}

#[test]
fn missing_proof_data_refuses_proof_budget_and_missing_override_fails_fast() {
    let scratch = Scratch::new("missing-proof");
    let before = scratch.file("before.json");
    block_json("people-transfer", &before);
    let mut no_proof: Value = serde_json::from_slice(&fs::read(&before).unwrap()).unwrap();
    no_proof["execution"] = Value::Null;
    for phase in no_proof["phases"].as_array_mut().unwrap() {
        for op in phase["ops"].as_array_mut().unwrap() {
            op["proof_bytes_added"] = Value::Null;
        }
        if let Some(nested) = phase["post_inherents"].as_object_mut() {
            for op in nested["ops"].as_array_mut().unwrap() {
                op["proof_bytes_added"] = Value::Null;
            }
        }
    }
    let after = scratch.file("after-noproof.json");
    fs::write(&after, serde_json::to_vec(&no_proof).unwrap()).unwrap();

    // Without a proof budget, diff succeeds and reports n/a / null proof delta.
    let out = run(
        command("diff")
            .arg(&before)
            .arg(&after)
            .args(["--json", "-"]),
        0,
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(json["totals"]["proof_bytes"]["delta"].is_null());
    assert!(String::from_utf8_lossy(&out.stderr).contains("n/a"));

    // With --max-proof-delta, missing proof data fails with exit code 1 (never passes silently).
    let out = run(
        command("diff")
            .arg(&before)
            .arg(&after)
            .args(["--max-proof-delta", "0"]),
        1,
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("proof bytes are missing or incomplete"));

    // Missing --runtime-override fails before replaying the baseline block.
    let out = run(
        command("diff")
            .arg("--offline")
            .arg(fixture("people-transfer"))
            .arg("--runtime-override")
            .arg(scratch.file("does-not-exist.wasm")),
        1,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("reading --runtime-override"), "{stderr}");
    assert!(!stderr.contains("root check: PASS"), "{stderr}");
}
