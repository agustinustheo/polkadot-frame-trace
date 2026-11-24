//! Compare block traces and enforce CI regression budgets.
use crate::{
    decode::{AnnotatedBlock, AnnotatedOp, RuntimeOverrideInfo},
    report::{self, Tally},
    source::SourceArgs,
    summary::{signed, table},
};
use anyhow::{Context, Result};
use clap::{ArgGroup, Args, ValueEnum};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    path::PathBuf,
};

#[derive(Args)]
#[command(
    group(ArgGroup::new("source").required(true).args(["before", "offline"])),
    override_usage = "polkadot-frame-trace diff [OPTIONS] <BEFORE.json> <AFTER.json>\n       polkadot-frame-trace diff [OPTIONS] --offline <DIR> --runtime-override <WASM>"
)]
pub struct DiffArgs {
    /// Saved block --json trace for the baseline.
    #[arg(value_name = "BEFORE.json", requires = "after")]
    before: Option<PathBuf>,
    /// Saved block --json trace for the candidate.
    #[arg(value_name = "AFTER.json", requires = "before")]
    after: Option<PathBuf>,
    /// Replay the same saved block with both runtimes.
    #[arg(long, value_name = "DIR", requires = "runtime_override")]
    offline: Option<PathBuf>,
    /// Candidate runtime for the second replay.
    #[arg(
        long,
        value_name = "WASM",
        requires = "offline",
        conflicts_with = "before"
    )]
    runtime_override: Option<PathBuf>,
    /// Maximum rows each for items and extrinsics (JSON is complete).
    #[arg(long, value_name = "N", default_value_t = 15)]
    top: usize,
    /// Rank deltas by absolute change in the selected metric.
    #[arg(long, value_enum, default_value_t = SortKey::Proof)]
    sort_by: SortKey,
    /// Write diff JSON to a file (`-` sends JSON to stdout and tables to stderr).
    #[arg(long, value_name = "FILE")]
    json: Option<PathBuf>,
    #[command(flatten)]
    budgets: Budgets,
}

#[derive(Args)]
struct Budgets {
    /// Largest allowed increase in proof bytes.
    #[arg(
        long = "max-proof-delta",
        value_name = "BYTES",
        allow_negative_numbers = true
    )]
    proof: Option<i64>,
    /// Largest allowed increase in storage operations.
    #[arg(
        long = "max-ops-delta",
        value_name = "N",
        allow_negative_numbers = true
    )]
    ops: Option<i64>,
    /// Largest allowed increase in duplicate reads.
    #[arg(
        long = "max-duplicate-reads-delta",
        value_name = "N",
        allow_negative_numbers = true
    )]
    duplicate_reads: Option<i64>,
}

#[derive(Clone, Copy, ValueEnum)]
enum SortKey {
    Proof,
    Ops,
    Reads,
    Duplicates,
}

impl SortKey {
    fn label(self) -> &'static str {
        match self {
            Self::Proof => "proof bytes",
            Self::Ops => "operations",
            Self::Reads => "reads",
            Self::Duplicates => "duplicate reads",
        }
    }

    fn magnitude(self, storage: &StorageDelta) -> u128 {
        match self {
            Self::Proof => storage.proof_bytes.delta.unwrap_or(0),
            Self::Ops => storage.ops.delta,
            Self::Reads => storage.reads.delta,
            Self::Duplicates => storage.duplicate_reads.delta,
        }
        .unsigned_abs()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("regression budget exceeded")]
pub struct RegressionExceeded;

pub fn run(args: DiffArgs) -> Result<()> {
    let (before, after) = if let Some(dir) = args.offline {
        // Validate the candidate Wasm before paying for the baseline replay.
        if let Some(wasm_path) = args.runtime_override.as_deref() {
            let wasm = std::fs::read(wasm_path)
                .with_context(|| format!("reading --runtime-override {}", wasm_path.display()))?;
            anyhow::ensure!(
                !wasm.is_empty(),
                "--runtime-override {} is empty",
                wasm_path.display()
            );
            crate::replay::check_runtime_wasm(&wasm)
                .with_context(|| format!("--runtime-override {}", wasm_path.display()))?;
        }
        let mut source = SourceArgs {
            offline: Some(dir),
            runtime_override: None,
            from_json: None,
            rpc: None,
            block: None,
            save: None,
            rpc_timeout: std::time::Duration::from_secs(30),
        };
        let before = crate::load(&source).context("replaying baseline")?;
        source.runtime_override = args.runtime_override;
        let after = crate::load(&source).context("replaying candidate")?;
        (before, after)
    } else if let (Some(before), Some(after)) = (args.before.as_deref(), args.after.as_deref()) {
        (report::read_trace(before)?, report::read_trace(after)?)
    } else {
        anyhow::bail!("expected two saved block traces or an offline replay");
    };

    if args.budgets.proof.is_some() && BlockDiff::proof_total(&before, &after).delta.is_none() {
        anyhow::bail!(
            "cannot check --max-proof-delta: proof bytes are missing or incomplete in one or both inputs"
        );
    }

    let diff = BlockDiff::new(&before, &after, args.sort_by, &args.budgets);
    for warning in &diff.warnings {
        eprintln!("warning: {warning}");
    }
    if let Some(path) = &args.json {
        crate::write_json(&diff, path)?;
    }
    let tables = diff.render(args.top, args.sort_by);
    if args.json.is_some_and(|path| path.as_os_str() == "-") {
        io::stderr().write_all(tables.as_bytes())?;
    } else {
        io::stdout().write_all(tables.as_bytes())?;
    }
    for regression in &diff.regressions {
        eprintln!(
            "regression: {} delta {} exceeds {} ({})",
            regression.metric,
            signed(regression.delta),
            signed(regression.max.into()),
            regression.flag,
        );
    }
    if !diff.regressions.is_empty() {
        return Err(RegressionExceeded.into());
    }
    Ok(())
}

#[derive(Clone, Copy, Serialize)]
struct Delta {
    before: u64,
    after: u64,
    delta: i128,
}

impl Delta {
    fn new(before: u64, after: u64) -> Self {
        Self {
            before,
            after,
            delta: i128::from(after) - i128::from(before),
        }
    }
}

#[derive(Clone, Copy, Serialize)]
struct OptionalDelta {
    before: Option<u64>,
    after: Option<u64>,
    delta: Option<i128>,
}

impl OptionalDelta {
    fn new(before: Option<u64>, after: Option<u64>) -> Self {
        Self {
            before,
            after,
            delta: before.zip(after).map(|(b, a)| Delta::new(b, a).delta),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProofCoverage {
    Complete(u64),
    Partial(u64),
    Missing,
}

impl ProofCoverage {
    fn of(tally: &Tally) -> Self {
        if tally.ops == 0 || tally.proof_ops_observed == tally.ops {
            Self::Complete(tally.proof_bytes as u64)
        } else if tally.proof_ops_observed == 0 {
            Self::Missing
        } else {
            Self::Partial(tally.proof_bytes as u64)
        }
    }

    fn complete(self) -> Option<u64> {
        match self {
            Self::Complete(n) => Some(n),
            Self::Partial(_) | Self::Missing => None,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Complete(n) => n.to_string(),
            Self::Partial(n) => format!("{n} (partial)"),
            Self::Missing => "n/a".into(),
        }
    }
}

#[derive(Serialize)]
struct StorageDelta {
    proof_bytes: OptionalDelta,
    #[serde(skip)]
    before_proof: ProofCoverage,
    #[serde(skip)]
    after_proof: ProofCoverage,
    ops: Delta,
    reads: Delta,
    writes: Delta,
    duplicate_reads: Delta,
}

impl StorageDelta {
    fn new(before: &Tally, after: &Tally) -> Self {
        let before_proof = ProofCoverage::of(before);
        let after_proof = ProofCoverage::of(after);
        Self {
            proof_bytes: OptionalDelta::new(before_proof.complete(), after_proof.complete()),
            before_proof,
            after_proof,
            ops: Delta::new(before.ops as u64, after.ops as u64),
            reads: Delta::new(before.reads as u64, after.reads as u64),
            writes: Delta::new(before.writes as u64, after.writes as u64),
            duplicate_reads: Delta::new(before.duplicates as u64, after.duplicates as u64),
        }
    }

    fn changed(&self) -> bool {
        self.proof_bytes.delta.is_some_and(|d| d != 0)
            || [self.ops, self.reads, self.writes, self.duplicate_reads]
                .iter()
                .any(|value| value.delta != 0)
    }

    fn row(&self, label: String, show_reads: bool) -> Vec<String> {
        let mut row = vec![
            label,
            self.before_proof.label(),
            self.after_proof.label(),
            self.proof_bytes.delta.map_or_else(unavailable, signed),
            signed(self.ops.delta),
        ];
        if show_reads {
            row.push(signed(self.reads.delta));
        }
        row.push(signed(self.duplicate_reads.delta));
        row
    }
}

#[derive(Serialize)]
struct Totals {
    /// Authoritative block proof total: uses `execution.proof_bytes` when both traces have it,
    /// otherwise complete operation-attributed sums. `null` when either side lacks proof data.
    proof_bytes: OptionalDelta,
    /// Raw `execution.proof_bytes` from both traces, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_proof_bytes: Option<OptionalDelta>,
    /// Operation-attributed proof bytes sum (complete sides only).
    attributed_proof_bytes: OptionalDelta,
    ops: Delta,
    reads: Delta,
    writes: Delta,
    duplicate_reads: Delta,
    reads_cache: Delta,
    reads_trie_already_in_proof: Delta,
    reads_trie_new: Delta,
    storage_time_ns: OptionalDelta,
    wall_time_ns: OptionalDelta,
    #[serde(skip)]
    before_proof_label: String,
    #[serde(skip)]
    after_proof_label: String,
}

impl Totals {
    fn tally(block: &AnnotatedBlock) -> (Tally, Option<u64>) {
        let mut timed = false;
        let tally = Tally::of(
            block
                .all_phases()
                .flat_map(|phase| &phase.ops)
                .inspect(|op| {
                    timed |= op.timing.is_some();
                }),
        );
        let time = (timed || tally.ops == 0).then_some(tally.storage_ns);
        (tally, time)
    }

    fn proof_total_and_warnings(
        before: &AnnotatedBlock,
        after: &AnnotatedBlock,
        b: &Tally,
        a: &Tally,
    ) -> (OptionalDelta, String, String, Vec<String>) {
        let b_cov = ProofCoverage::of(b);
        let a_cov = ProofCoverage::of(a);
        let b_exec = before.execution.as_ref().map(|e| e.proof_bytes as u64);
        let a_exec = after.execution.as_ref().map(|e| e.proof_bytes as u64);
        let mut warnings = Vec::new();
        for (side, exec, cov) in [("before", b_exec, b_cov), ("after", a_exec, a_cov)] {
            if let (Some(exec), Some(attr)) = (exec, cov.complete())
                && exec != attr
            {
                warnings.push(format!(
                    "{side}: execution proof ({exec} B) differs from attributed operation sum ({attr} B); using execution proof for block totals"
                ));
            }
        }
        if let (Some(be), Some(ae)) = (b_exec, a_exec) {
            return (
                OptionalDelta::new(Some(be), Some(ae)),
                be.to_string(),
                ae.to_string(),
                warnings,
            );
        }
        let b_val = b_exec.or_else(|| b_cov.complete());
        let a_val = a_exec.or_else(|| a_cov.complete());
        let b_label = b_exec.map_or_else(|| b_cov.label(), |n| n.to_string());
        let a_label = a_exec.map_or_else(|| a_cov.label(), |n| n.to_string());
        (OptionalDelta::new(b_val, a_val), b_label, a_label, warnings)
    }

    fn new(before: &AnnotatedBlock, after: &AnnotatedBlock) -> (Self, Vec<String>) {
        let (b, bt) = Self::tally(before);
        let (a, at) = Self::tally(after);
        let storage = StorageDelta::new(&b, &a);
        let (proof_bytes, before_proof_label, after_proof_label, warnings) =
            Self::proof_total_and_warnings(before, after, &b, &a);
        let b_exec = before.execution.as_ref().map(|e| e.proof_bytes as u64);
        let a_exec = after.execution.as_ref().map(|e| e.proof_bytes as u64);
        let execution_proof_bytes =
            (b_exec.is_some() || a_exec.is_some()).then(|| OptionalDelta::new(b_exec, a_exec));
        let totals = Self {
            proof_bytes,
            execution_proof_bytes,
            attributed_proof_bytes: storage.proof_bytes,
            ops: storage.ops,
            reads: storage.reads,
            writes: storage.writes,
            duplicate_reads: storage.duplicate_reads,
            reads_cache: Delta::new(b.overlay as u64, a.overlay as u64),
            reads_trie_already_in_proof: Delta::new(b.backend_warm as u64, a.backend_warm as u64),
            reads_trie_new: Delta::new(b.backend_cold as u64, a.backend_cold as u64),
            storage_time_ns: OptionalDelta::new(bt, at),
            wall_time_ns: OptionalDelta::new(
                before.timing.map(|t| t.duration_ns),
                after.timing.map(|t| t.duration_ns),
            ),
            before_proof_label,
            after_proof_label,
        };
        (totals, warnings)
    }

    fn table(&self) -> comfy_table::Table {
        let mut table = table(1, &["metric", "before", "after", "delta", "change %"]);
        table.add_row([
            "proof bytes".into(),
            self.before_proof_label.clone(),
            self.after_proof_label.clone(),
            self.proof_bytes.delta.map_or_else(unavailable, signed),
            percent(self.proof_bytes.before, self.proof_bytes.delta),
        ]);
        for (label, value) in [
            ("operations", self.ops),
            ("reads (cache)", self.reads_cache),
            (
                "reads (trie: already in proof)",
                self.reads_trie_already_in_proof,
            ),
            ("reads (trie: new)", self.reads_trie_new),
            ("writes", self.writes),
            ("duplicate reads", self.duplicate_reads),
        ] {
            table.add_row([
                label.into(),
                value.before.to_string(),
                value.after.to_string(),
                signed(value.delta),
                percent(Some(value.before), Some(value.delta)),
            ]);
        }
        for (label, time) in [
            ("storage time µs", &self.storage_time_ns),
            ("wall time µs", &self.wall_time_ns),
        ] {
            table.add_row([
                label.into(),
                time.before.map_or_else(unavailable, |ns| micros(ns.into())),
                time.after.map_or_else(unavailable, |ns| micros(ns.into())),
                time.delta.map_or_else(unavailable, |ns| {
                    let value = micros(ns);
                    if ns > 0 { format!("+{value}") } else { value }
                }),
                percent(time.before, time.delta),
            ]);
        }
        table
    }
}

#[derive(Serialize)]
struct BlockInfo<'a> {
    block_hash: &'a str,
    block_number: Option<u64>,
    runtime_override: Option<&'a RuntimeOverrideInfo>,
}

impl<'a> From<&'a AnnotatedBlock> for BlockInfo<'a> {
    fn from(block: &'a AnnotatedBlock) -> Self {
        Self {
            block_hash: &block.block_hash,
            block_number: block.block_number,
            runtime_override: block.runtime_override.as_ref(),
        }
    }
}

impl BlockInfo<'_> {
    fn label(&self) -> String {
        let mut label = self.block_number.map_or_else(
            || self.block_hash.to_owned(),
            |n| format!("#{n} ({})", self.block_hash),
        );
        if let Some(runtime) = self.runtime_override {
            label += &format!(" [RUNTIME OVERRIDE {}]", runtime.code_hash);
        }
        label
    }
}

#[derive(Serialize)]
struct PhaseDelta<'a> {
    kind: &'a str,
    extrinsic_index: Option<u32>,
    before_label: Option<&'a str>,
    after_label: Option<&'a str>,
    #[serde(flatten)]
    storage: StorageDelta,
}

impl PhaseDelta<'_> {
    fn label(&self) -> String {
        match (self.before_label, self.after_label) {
            (Some(b), Some(a)) if b != a => format!("{b} → {a}"),
            (Some(_), Some(a)) => a.to_owned(),
            (None, Some(a)) => format!("{a} (added)"),
            (Some(b), None) => format!("{b} (removed)"),
            (None, None) => self.kind.to_owned(),
        }
    }
}

// Group by phase kind and extrinsic index, independent of labels and nesting.
type PhaseKey<'a> = (u8, Option<u32>, &'a str);
type PhaseGroups<'a> = BTreeMap<PhaseKey<'a>, (&'a str, Vec<&'a AnnotatedOp>)>;

fn phase_groups(block: &AnnotatedBlock) -> PhaseGroups<'_> {
    let mut groups = BTreeMap::new();
    for phase in block.all_phases() {
        let order = match phase.kind.as_str() {
            "initialization" => 0,
            "apply_extrinsic" => 1,
            "post_inherents" => 2,
            "finalization" => 3,
            _ => 4,
        };
        let key = (order, phase.extrinsic_index, phase.kind.as_str());
        groups
            .entry(key)
            .or_insert_with(|| (phase.label.as_str(), Vec::new()))
            .1
            .extend(&phase.ops);
    }
    groups
}

fn phase_deltas<'a>(before: &'a AnnotatedBlock, after: &'a AnnotatedBlock) -> Vec<PhaseDelta<'a>> {
    let before = phase_groups(before);
    let after = phase_groups(after);
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    keys.into_iter()
        .map(|key @ (_, extrinsic_index, kind)| {
            let b = before.get(&key);
            let a = after.get(&key);
            let storage = StorageDelta::new(
                &Tally::of(b.into_iter().flat_map(|(_, ops)| ops.iter().copied())),
                &Tally::of(a.into_iter().flat_map(|(_, ops)| ops.iter().copied())),
            );
            PhaseDelta {
                kind,
                extrinsic_index,
                before_label: b.map(|(label, _)| *label),
                after_label: a.map(|(label, _)| *label),
                storage,
            }
        })
        .collect()
}

#[derive(Serialize)]
struct ItemDelta<'a> {
    item: &'a str,
    #[serde(flatten)]
    storage: StorageDelta,
}

fn normalized_item(op: &AnnotatedOp) -> &str {
    if op.op == "storage_root" {
        if op.child_trie.is_some() {
            "<child storage root>"
        } else {
            "<storage root>"
        }
    } else {
        &op.item
    }
}

fn item_deltas<'a>(
    before: &'a AnnotatedBlock,
    after: &'a AnnotatedBlock,
    sort: SortKey,
) -> Vec<ItemDelta<'a>> {
    let mut groups: BTreeMap<&str, [Vec<&AnnotatedOp>; 2]> = BTreeMap::new();
    for (side, block) in [before, after].into_iter().enumerate() {
        for op in block.all_phases().flat_map(|phase| &phase.ops) {
            groups.entry(normalized_item(op)).or_default()[side].push(op);
        }
    }
    let mut items: Vec<_> = groups
        .into_iter()
        .filter_map(|(item, [b, a])| {
            let storage = StorageDelta::new(&Tally::of(b), &Tally::of(a));
            storage.changed().then_some(ItemDelta { item, storage })
        })
        .collect();
    items.sort_by(|a, b| {
        sort.magnitude(&b.storage)
            .cmp(&sort.magnitude(&a.storage))
            .then(a.item.cmp(b.item))
    });
    items
}

#[derive(Serialize)]
struct Regression {
    metric: &'static str,
    flag: &'static str,
    max: i64,
    delta: i128,
}

impl Budgets {
    fn regressions(&self, totals: &Totals) -> Vec<Regression> {
        let mut out = Vec::new();
        if let (Some(max), Some(delta)) = (self.proof, totals.proof_bytes.delta)
            && delta > i128::from(max)
        {
            out.push(Regression {
                metric: "proof bytes",
                flag: "--max-proof-delta",
                max,
                delta,
            });
        }
        for (metric, flag, max, value) in [
            ("operations", "--max-ops-delta", self.ops, totals.ops),
            (
                "duplicate reads",
                "--max-duplicate-reads-delta",
                self.duplicate_reads,
                totals.duplicate_reads,
            ),
        ] {
            if let Some(max) = max
                && value.delta > i128::from(max)
            {
                out.push(Regression {
                    metric,
                    flag,
                    max,
                    delta: value.delta,
                });
            }
        }
        out
    }
}

#[derive(Serialize)]
struct BlockDiff<'a> {
    schema_version: u32,
    before: BlockInfo<'a>,
    after: BlockInfo<'a>,
    totals: Totals,
    phases: Vec<PhaseDelta<'a>>,
    items: Vec<ItemDelta<'a>>,
    regressions: Vec<Regression>,
    #[serde(skip)]
    warnings: Vec<String>,
}

impl<'a> BlockDiff<'a> {
    fn proof_total(before: &AnnotatedBlock, after: &AnnotatedBlock) -> OptionalDelta {
        let (b, _) = Totals::tally(before);
        let (a, _) = Totals::tally(after);
        Totals::proof_total_and_warnings(before, after, &b, &a).0
    }

    fn new(
        before: &'a AnnotatedBlock,
        after: &'a AnnotatedBlock,
        sort: SortKey,
        budgets: &Budgets,
    ) -> Self {
        let (totals, warnings) = Totals::new(before, after);
        Self {
            schema_version: 1,
            before: before.into(),
            after: after.into(),
            regressions: budgets.regressions(&totals),
            totals,
            phases: phase_deltas(before, after),
            items: item_deltas(before, after, sort),
            warnings,
        }
    }

    fn render(&self, top: usize, sort: SortKey) -> String {
        let show_reads = matches!(sort, SortKey::Reads);
        let mut extrinsics: Vec<_> = self
            .phases
            .iter()
            .enumerate()
            .filter(|(_, phase)| phase.kind == "apply_extrinsic")
            .collect();
        extrinsics.sort_by_key(|(index, phase)| {
            (std::cmp::Reverse(sort.magnitude(&phase.storage)), *index)
        });
        let shown: BTreeSet<_> = extrinsics
            .iter()
            .take(top)
            .map(|(index, _)| *index)
            .collect();
        let mut phases = delta_table("phase", "dup reads delta", show_reads);
        for (index, phase) in self.phases.iter().enumerate() {
            if phase.kind != "apply_extrinsic" || shown.contains(&index) {
                phases.add_row(phase.storage.row(phase.label(), show_reads));
            }
        }

        let mut items = delta_table("item", "dup delta", show_reads);
        for item in self.items.iter().take(top) {
            items.add_row(item.storage.row(item.item.to_owned(), show_reads));
        }
        format!(
            "Before: {}\nAfter: {}\n\nOverview\n{}\n\nPhase deltas\n{phases}\n{}\nTop item deltas (ranked by {})\n{items}\n{}",
            self.before.label(),
            self.after.label(),
            self.totals.table(),
            omitted(extrinsics.len(), top, "extrinsics"),
            sort.label(),
            omitted(self.items.len(), top, "items"),
        )
    }
}

fn delta_table(label: &str, duplicates: &str, show_reads: bool) -> comfy_table::Table {
    let mut header = vec![
        label,
        "before proof",
        "after proof",
        "proof delta",
        "ops delta",
    ];
    if show_reads {
        header.push("reads delta");
    }
    header.push(duplicates);
    table(1, &header)
}

fn omitted(total: usize, top: usize, label: &str) -> String {
    let n = total.saturating_sub(top);
    if n == 0 {
        String::new()
    } else {
        format!("… {n} more {label}\n")
    }
}

fn micros(ns: i128) -> String {
    format!("{:.3}", ns as f64 / 1_000.0)
}

fn percent(before: Option<u64>, delta: Option<i128>) -> String {
    match (before, delta) {
        (Some(b), Some(d)) if b > 0 && d != 0 => format!("{:+.1}%", d as f64 * 100.0 / b as f64),
        (Some(b), Some(_)) if b > 0 => "0.0%".into(),
        _ => unavailable(),
    }
}

fn unavailable() -> String {
    "—".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> AnnotatedBlock {
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/model/two-extrinsics.json"))
                .expect("valid JSON");
        let trace =
            serde_json::from_value(raw["result"]["blockTrace"].clone()).expect("valid block trace");
        let block = crate::model::TracedBlock::from_trace(&trace).expect("parsed trace");
        crate::decode::annotate(&block, None)
    }

    #[test]
    fn delta_math_and_signed_formatting() {
        assert_eq!(Delta::new(20, 10).delta, -10);
        assert_eq!(Delta::new(0, u64::MAX).delta, i128::from(u64::MAX));
        assert_eq!(Delta::new(u64::MAX, 0).delta, -i128::from(u64::MAX));
        assert_eq!([120, -48, 0].map(signed), ["+120", "-48", "0"]);
        assert_eq!(percent(Some(20), Some(-10)), "-50.0%");
        assert_eq!(percent(Some(20), Some(0)), "0.0%");
        assert_eq!(percent(Some(0), Some(10)), "—");
        assert_eq!(OptionalDelta::new(None, Some(10)).delta, None);
    }

    #[test]
    fn phase_alignment_uses_kind_and_index_including_nested_phases() {
        let mut before = fixture();
        let extrinsic = before
            .phases
            .iter()
            .find(|p| p.extrinsic_index.is_some())
            .expect("extrinsic")
            .clone();
        let mut nested = extrinsic.clone();
        nested.kind = "post_inherents".into();
        nested.extrinsic_index = None;
        nested.label = "post inherents".into();
        nested.ops.truncate(1);
        before.phases = vec![extrinsic.clone()];
        before.phases[0].post_inherents = Some(Box::new(nested.clone()));

        let mut renamed = extrinsic.clone();
        renamed.label = "Renamed.call".into();
        renamed.ops.clear();
        let mut removed = extrinsic.clone();
        removed.extrinsic_index = Some(40);
        before.phases.push(removed);
        let mut added = extrinsic;
        added.extrinsic_index = Some(41);
        let mut after = before.clone();
        after.phases = vec![renamed, added, nested];

        let phases = phase_deltas(&before, &after);
        assert_eq!(phases.len(), 4);
        assert!(phases[0].label().ends_with(" → Renamed.call"));
        assert!(phases[0].storage.ops.delta < 0);
        assert_eq!(phases[1].extrinsic_index, Some(40));
        assert!(phases[1].label().ends_with("(removed)"));
        assert_eq!(phases[2].extrinsic_index, Some(41));
        assert!(phases[2].label().ends_with("(added)"));
        assert_eq!(phases[3].label(), "post inherents");
        assert_eq!(phases[3].storage.ops.before, 1);
        assert_eq!(phases[3].storage.ops.delta, 0);
    }

    #[test]
    fn item_ranking_uses_absolute_deltas_and_name_ties() {
        let mut before = fixture();
        let template = before
            .all_phases()
            .flat_map(|p| &p.ops)
            .next()
            .expect("storage op")
            .clone();
        let op = |name: &str, proof, kind: &str, duplicate| {
            let mut op = template.clone();
            op.item = name.into();
            op.proof_bytes_added = Some(proof);
            op.op = kind.into();
            op.duplicate_read = duplicate;
            op
        };
        let unchanged = op("unchanged", 200, "get", true);
        before.phases.truncate(1);
        before.phases[0].post_inherents = None;
        before.phases[0].ops = vec![op("A", 30, "get", false), unchanged.clone()];
        let mut after = before.clone();
        after.phases[0].ops = vec![
            op("B", 30, "get", false),
            op("C", 10, "get", true),
            op("C", 10, "get", true),
            op("D", 0, "put", false),
            unchanged,
        ];
        for (sort, expected) in [
            (SortKey::Proof, ["A", "B", "C", "D"]),
            (SortKey::Ops, ["C", "A", "B", "D"]),
            (SortKey::Reads, ["C", "A", "B", "D"]),
            (SortKey::Duplicates, ["C", "A", "B", "D"]),
        ] {
            let items = item_deltas(&before, &after, sort);
            assert_eq!(
                items.iter().map(|item| item.item).collect::<Vec<_>>(),
                expected
            );
        }
        let mut after = before.clone();
        after.phases[0].ops[0].op = "put".into();
        let items = item_deltas(&before, &after, SortKey::Proof);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].storage.writes.delta, 1);
        assert_eq!(items[0].storage.ops.delta, 0);
    }

    #[test]
    fn regression_budgets_use_strict_signed_comparisons() {
        let before = Tally {
            duplicates: 3,
            ..Tally::default()
        };
        let after = Tally {
            proof_bytes: 100,
            proof_ops_observed: 1,
            ops: 1,
            duplicates: 2,
            ..Tally::default()
        };
        let storage = StorageDelta::new(&before, &after);
        let totals = Totals {
            proof_bytes: storage.proof_bytes,
            execution_proof_bytes: None,
            attributed_proof_bytes: storage.proof_bytes,
            ops: storage.ops,
            reads: storage.reads,
            writes: storage.writes,
            duplicate_reads: storage.duplicate_reads,
            reads_cache: Delta::new(0, 0),
            reads_trie_already_in_proof: Delta::new(0, 0),
            reads_trie_new: Delta::new(0, 0),
            storage_time_ns: OptionalDelta::new(None, None),
            wall_time_ns: OptionalDelta::new(None, None),
            before_proof_label: "0".into(),
            after_proof_label: "100".into(),
        };
        let mut budgets = Budgets {
            proof: Some(100),
            ops: None,
            duplicate_reads: Some(-1),
        };
        assert!(budgets.regressions(&totals).is_empty());

        budgets.proof = Some(99);
        budgets.ops = Some(0);
        budgets.duplicate_reads = Some(-2);
        let failed: Vec<_> = budgets
            .regressions(&totals)
            .into_iter()
            .map(|r| (r.flag, r.delta, r.max))
            .collect();
        assert_eq!(
            failed,
            [
                ("--max-proof-delta", 100, 99),
                ("--max-ops-delta", 1, 0),
                ("--max-duplicate-reads-delta", -1, -2),
            ]
        );
    }

    #[test]
    fn execution_proof_overrides_over_attributed_ops_and_normalizes_storage_roots() {
        let mut before = fixture();
        before.phases.truncate(1);
        before.phases[0].post_inherents = None;
        before.phases[0].ops.truncate(2);
        for op in &mut before.phases[0].ops {
            op.proof_bytes_added = Some(100);
        }
        before.phases[0].ops[0].op = "storage_root".into();
        before.phases[0].ops[0].item = "<unknown>".into();
        before.execution = Some(crate::decode::ExecutionInfo {
            root_check: true,
            proof_bytes: 100,
            rpc_calls: 0,
            measured_rpc_calls: 0,
            proof_rounds: 0,
        });

        let mut after = before.clone();
        after.phases[0].ops[0].item = "<storage root>".into();
        after.phases[0].ops[0].proof_bytes_added = Some(75);
        after.phases[0].ops[1].proof_bytes_added = Some(75);
        after.execution.as_mut().expect("exec").proof_bytes = 150;

        let budgets = Budgets {
            proof: Some(0),
            ops: None,
            duplicate_reads: None,
        };
        let diff = BlockDiff::new(&before, &after, SortKey::Proof, &budgets);
        // Execution proof grew 100 -> 150 (+50) even though attributed ops dropped 200 -> 150 (-50).
        assert_eq!(diff.totals.proof_bytes.delta, Some(50));
        assert_eq!(diff.totals.attributed_proof_bytes.delta, Some(-50));
        assert_eq!(diff.regressions.len(), 1);
        assert_eq!(diff.regressions[0].delta, 50);
        assert!(
            diff.warnings
                .iter()
                .any(|w| w.contains("differs from attributed operation sum"))
        );

        // Identical storage_root ops named "<unknown>" vs "<storage root>" produce zero item delta.
        let mut same_after = before.clone();
        same_after.phases[0].ops[0].item = "<storage root>".into();
        let items = item_deltas(&before, &same_after, SortKey::Proof);
        assert!(
            items.is_empty(),
            "phantom storage_root diff: {}",
            items.len()
        );
    }

    #[test]
    fn missing_or_partial_proof_data_never_reports_zero_delta() {
        let mut before = fixture();
        before.phases.truncate(1);
        before.phases[0].post_inherents = None;
        before.phases[0].ops.truncate(2);
        before.execution = None;
        for op in &mut before.phases[0].ops {
            op.proof_bytes_added = None;
        }
        let after = before.clone();
        assert_eq!(BlockDiff::proof_total(&before, &after).delta, None);

        // Partial coverage on one side also produces None delta.
        let mut partial = before.clone();
        partial.phases[0].ops[0].proof_bytes_added = Some(42);
        let diff = BlockDiff::new(
            &before,
            &partial,
            SortKey::Proof,
            &Budgets {
                proof: None,
                ops: None,
                duplicate_reads: None,
            },
        );
        assert_eq!(diff.totals.proof_bytes.delta, None);
        let rendered = diff.render(5, SortKey::Proof);
        assert!(rendered.contains("n/a") && rendered.contains("42 (partial)"));
    }
}
