//! Terminal storage tables.

use std::collections::BTreeMap;

use crate::decode::{AnnotatedBlock, AnnotatedOp, AnnotatedPhase};
use crate::model::Stats;
use crate::report::{Tally, proof_label};
use comfy_table::{CellAlignment, Table};

/// Renders the phase table, the top storage items, and the duplicate reads of each phase.
pub fn render(block: &AnnotatedBlock, top: usize) -> String {
    let report = crate::report::aggregate(std::slice::from_ref(block));
    let mut out = crate::report::override_lines(&report, top);
    out += &if let Some(call) = &block.call {
        format!(
            "call {} at #{} ({})\n\n",
            call.method,
            block.block_number.unwrap_or(0),
            block.block_hash
        )
    } else {
        format!(
            "block {} (parent {})\n\n",
            block.block_hash, block.parent_hash
        )
    };
    out += &format!("Storage per phase\n{}\n", phases(block));
    out += &block_weight_lines(block);
    out += "\n";
    out += &crate::report::render_body(&report, top, crate::report::SortKey::Proof);
    out += &format!(
        "Duplicate reads per phase (top {top} each)\n{}\n",
        duplicates(block, top)
    );
    out
}

fn phases(block: &AnnotatedBlock) -> Table {
    let charged = block.all_phases().any(|p| p.charged_weight.is_some());
    let mut header = vec![
        "phase",
        "operations",
        "reads",
        "writes",
        "proof bytes",
        "storage time µs",
        "wall time µs",
    ];
    if charged {
        header.extend(["charged proof", "proof gap"]);
    }
    let mut table = table(1, &header);
    for phase in &block.phases {
        table.add_row(phase_row(phase, phase.label.clone(), charged));
        // Runs inside the extrinsic (between its checks and its own work); not in its row.
        if let Some(nested) = &phase.post_inherents {
            table.add_row(phase_row(nested, format!("  └ {}", nested.label), charged));
        }
    }
    let mut total = stats_row("total".into(), &block.totals);
    let tally = Tally::of(block.all_phases().flat_map(|p| &p.ops));
    total.extend([
        proof_label(&tally),
        format!("{:.1}", tally.storage_ns as f64 / 1000.0),
        block.timing.map_or_else(
            || "?".into(),
            |t| format!("{:.1}", t.duration_ns as f64 / 1000.0),
        ),
    ]);
    if charged {
        // Charged weights are per extrinsic; block totals are in the lines below the table.
        total.extend(["—".into(), "—".into()]);
    }
    table.add_row(total);
    table
}

fn phase_row(phase: &AnnotatedPhase, label: String, charged: bool) -> Vec<String> {
    let mut row = stats_row(label, &phase.stats);
    let t = Tally::of(&phase.ops);
    row.extend([
        proof_label(&t),
        format!("{:.1}", t.storage_ns as f64 / 1000.0),
        phase.wall_time.as_ref().map_or_else(
            || "?".into(),
            |t| format!("{:.1}", t.duration_ns as f64 / 1000.0),
        ),
    ]);
    if charged {
        match phase.charged_weight {
            Some(weight) => row.extend([
                weight.proof_size.to_string(),
                gap_label(&t, weight.proof_size),
            ]),
            None => row.extend(["—".into(), "—".into()]),
        }
    }
    row
}

/// Measured minus charged proof bytes, with an explicit sign.
pub(crate) fn gap_label(measured: &Tally, charged: u64) -> String {
    if measured.proof_ops_observed == 0 {
        return "n/a".into();
    }
    let gap = signed(measured.proof_bytes as i128 - i128::from(charged));
    if measured.proof_ops_observed < measured.ops {
        format!("{gap} (partial)")
    } else {
        gap
    }
}

pub(crate) fn signed(n: i128) -> String {
    if n > 0 {
        format!("+{n}")
    } else {
        n.to_string()
    }
}

/// Compares the final `System::BlockWeight` with the measured proof bytes: the whole block,
/// then the mandatory class (hooks, post inherents and mandatory extrinsics).
fn block_weight_lines(block: &AnnotatedBlock) -> String {
    let Some(weight) = block.block_weight else {
        return String::new();
    };
    let line = |what: &str, charged: u64, measured: &Tally| {
        format!(
            "{what}: charged proof {charged}, measured {}, gap {}\n",
            proof_label(measured),
            gap_label(measured, charged)
        )
    };
    let all = Tally::of(block.all_phases().flat_map(|p| &p.ops));
    let mut out = line(
        "Block weight (System::BlockWeight)",
        weight.total().proof_size,
        &all,
    );
    // Only when every extrinsic has a class: otherwise the measured side is incomplete.
    let classified = block.all_phases().all(|p| match p.kind.as_str() {
        "apply_extrinsic" => p.dispatch_class.is_some(),
        "unsegmented" => false,
        _ => true,
    });
    if classified {
        let mandatory = Tally::of(
            block
                .all_phases()
                .filter(|p| {
                    p.kind != "apply_extrinsic" || p.dispatch_class.as_deref() == Some("mandatory")
                })
                .flat_map(|p| &p.ops),
        );
        out += &line(
            "Mandatory class (hooks and mandatory extrinsics)",
            weight.mandatory.proof_size,
            &mandatory,
        );
    }
    out
}
fn stats_row(label: String, s: &Stats) -> Vec<String> {
    let mut row = vec![label];
    row.extend([s.ops, s.reads, s.writes].map(|n| n.to_string()));
    row
}

fn duplicates(block: &AnnotatedBlock, top: usize) -> Table {
    let mut table = table(
        2,
        &[
            "phase",
            "key",
            "duplicate reads",
            "cache",
            "trie (already in proof)",
            "trie (new)",
            "unknown",
        ],
    );
    for phase in block.all_phases() {
        // Group by key identity (child trie, raw key), whatever read method was used: names
        // can be shared by different keys (`<unknown>`, undecoded map keys).
        type DuplicateRows<'a> =
            BTreeMap<(Option<&'a str>, &'a str), (&'a AnnotatedOp, [usize; 5])>;
        let mut counts: DuplicateRows<'_> = BTreeMap::new();
        for op in phase.ops.iter().filter(|op| op.duplicate_read) {
            let row = counts
                .entry((op.child_trie.as_deref(), op.key.as_str()))
                .or_insert((op, [0; 5]));
            row.1[0] += 1;
            row.1[crate::report::origin_index(op.origin.as_deref()) + 1] += 1;
        }
        let mut rows: Vec<_> = counts.into_values().collect();
        rows.sort_by(|a, b| b.1[0].cmp(&a.1[0]).then(a.0.seq.cmp(&b.0.seq)));
        let omitted = rows.len().saturating_sub(top);
        for (op, n) in rows.into_iter().take(top) {
            let mut row = vec![phase.label.clone(), key_name(op)];
            row.extend(n.map(|n| n.to_string()));
            table.add_row(row);
        }
        if omitted > 0 {
            table.add_row(vec![
                phase.label.clone(),
                format!("… {omitted} more"),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ]);
        }
    }
    table
}

/// The op's key name, with the start of the raw key when the name alone does not identify it.
fn key_name(op: &AnnotatedOp) -> String {
    if op.key_complete {
        return op.name.clone();
    }
    match op.key.get(..34) {
        Some(head) if op.key.len() > 34 => format!("{} [{head}…]", op.name),
        _ => format!("{} [{}]", op.name, op.key),
    }
}

/// A table with the given header; columns after the first `text_columns` hold numbers and are
/// right-aligned.
pub(crate) fn table(text_columns: usize, header: &[&str]) -> Table {
    let mut table = Table::new();
    table
        .load_style(comfy_table::presets::UTF8_FULL_CONDENSED)
        .set_header(header.to_vec());
    for column in table.column_iter_mut().skip(text_columns) {
        column.set_cell_alignment(CellAlignment::Right);
    }
    table
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::decode::{BlockWeight, Weight};

    #[test]
    fn proof_gap_columns_and_block_weight_lines() {
        let raw = serde_json::from_value::<crate::model::rpc::BlockTrace>(
            serde_json::from_str::<serde_json::Value>(include_str!(
                "../fixtures/model/two-extrinsics.json"
            ))
            .unwrap()["result"]["blockTrace"]
                .take(),
        )
        .unwrap();
        let mut block =
            crate::decode::annotate(&crate::model::TracedBlock::from_trace(&raw).unwrap(), None);
        let plain = render(&block, 3);
        assert!(!plain.contains("proof gap") && !plain.contains("Block weight"));
        for phase in &mut block.phases {
            let nested = phase.post_inherents.as_deref_mut().into_iter();
            for op in nested.flat_map(|p| &mut p.ops).chain(&mut phase.ops) {
                op.proof_bytes_added = Some(10);
            }
        }
        let weight = |proof_size| Weight {
            ref_time: 1,
            proof_size,
        };
        let mut charged = 0;
        for phase in block
            .phases
            .iter_mut()
            .filter(|p| p.kind == "apply_extrinsic")
        {
            let measured = phase.ops.len() as u64 * 10;
            // One extrinsic undercharged by 7 bytes, one overcharged by 5.
            let (proof, class) = if charged == 0 {
                (measured - 7, "mandatory")
            } else {
                (measured + 5, "normal")
            };
            phase.charged_weight = Some(weight(proof));
            phase.dispatch_class = Some(class.into());
            charged += 1;
        }
        assert_eq!(charged, 2);
        block.block_weight = Some(BlockWeight {
            normal: weight(100),
            operational: weight(0),
            mandatory: weight(20),
        });
        let text = render(&block, 3);
        let row = |label: &str| {
            text.lines()
                .find(|l| l.starts_with(&format!("│ {label}")))
                .unwrap()
                .to_owned()
        };
        assert!(text.contains("charged proof ┆ proof gap"), "{text}");
        assert!(
            row("extrinsic 0")
                .trim_end_matches(['│', ' '])
                .ends_with("+7"),
            "{text}"
        );
        assert!(
            row("extrinsic 1")
                .trim_end_matches(['│', ' '])
                .ends_with("-5"),
            "{text}"
        );
        assert!(
            row("initialization")
                .trim_end_matches(['│', ' '])
                .ends_with('—'),
            "{text}"
        );
        let total = block.all_phases().map(|p| p.ops.len() * 10).sum::<usize>();
        assert!(
            text.contains(&format!(
                "Block weight (System::BlockWeight): charged proof 120, measured {total}, gap +{}",
                total - 120
            )),
            "{text}"
        );
        let normal = block
            .phases
            .iter()
            .find(|p| p.dispatch_class.as_deref() == Some("normal"));
        let mandatory = total - normal.unwrap().ops.len() * 10;
        assert!(
            text.contains(&format!(
                "Mandatory class (hooks and mandatory extrinsics): charged proof 20, measured {mandatory}, gap +{}",
                mandatory - 20
            )),
            "{text}"
        );
        // An extrinsic without a class leaves the mandatory side unknown.
        block
            .phases
            .iter_mut()
            .find(|p| p.kind == "apply_extrinsic")
            .unwrap()
            .dispatch_class = None;
        assert!(!render(&block, 3).contains("Mandatory class"));
        assert_eq!(
            (signed(0), signed(-1493), signed(2803)),
            ("0".into(), "-1493".into(), "+2803".into())
        );
    }
}
