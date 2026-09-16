//! Percentile budget gate for CI (estate standard §5).
//!
//! A thin wrapper around [percentile-kit]'s report feature: parses the
//! criterion output under `--criterion-dir`, compares it with the
//! committed `--budgets` file (`percentile-budgets.toml`), prints the
//! canonical PASS/FAIL markdown table, and exits non-zero when any
//! gated P50/P99 exceeds its budget beyond `max_regression_pct`. It
//! only reads criterion output that `cargo bench` already wrote — no
//! re-benching — so ci.yml runs it as a cheap second gate right after
//! the bench run.
//!
//! [percentile-kit]: https://github.com/WyattAu/percentile-kit
#![allow(clippy::print_stderr, clippy::print_stdout)]

use std::{path::PathBuf, process::ExitCode};

use percentile_kit::ReportError;

const USAGE: &str =
    "usage: kestrel-budget-check --criterion-dir <dir> --budgets <percentile-budgets.toml>";

/// Command-line or gate failure.
#[derive(Debug, thiserror::Error)]
enum GateError {
    /// Bad arguments; rendered together with [`USAGE`].
    #[error("{0}\n{USAGE}")]
    Args(String),
    /// percentile-kit found missing/malformed input, or a budget was
    /// breached (`BudgetExceeded`).
    #[error(transparent)]
    Budget(#[from] ReportError),
}

/// Parsed command line.
struct Args {
    /// Criterion output root (usually `target/criterion`).
    criterion_dir: PathBuf,
    /// Committed budgets file (`percentile-budgets.toml`).
    budgets: PathBuf,
}

/// Consumes the value following `flag` from the argument iterator.
fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<PathBuf, GateError> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| GateError::Args(format!("`{flag}` requires a value")))
}

/// Parses `--criterion-dir <dir>` and `--budgets <file>` (both required).
fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, GateError> {
    let mut args = args;
    let mut criterion_dir: Option<PathBuf> = None;
    let mut budgets: Option<PathBuf> = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--criterion-dir" => criterion_dir = Some(next_value(&mut args, &flag)?),
            "--budgets" => budgets = Some(next_value(&mut args, &flag)?),
            other => return Err(GateError::Args(format!("unexpected argument `{other}`"))),
        }
    }
    let missing = |name: &str| GateError::Args(format!("missing required flag {name}"));
    Ok(Args {
        criterion_dir: criterion_dir.ok_or_else(|| missing("--criterion-dir"))?,
        budgets: budgets.ok_or_else(|| missing("--budgets"))?,
    })
}

/// Runs the gate: parse criterion output against the budgets, print the
/// markdown table, then hard-fail on the first breached budget.
fn run(args: &Args) -> Result<(), GateError> {
    let report = percentile_kit::check_budgets(&args.budgets, &args.criterion_dir)?;
    println!("{}", report.to_markdown());
    report.ensure_pass()?;
    println!("All percentile budgets within SLA.");
    Ok(())
}

fn main() -> ExitCode {
    match parse_args(std::env::args().skip(1)).and_then(|args| run(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("kestrel-budget-check: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Args, run};

    /// The committed `percentile-budgets.toml` must parse against
    /// percentile-kit's real serde schema (`deny_unknown_fields`), so a
    /// typo like `p999` or a stray key fails CI here — before any bench
    /// time is spent.
    #[test]
    fn committed_budgets_parse_into_percentile_kit_schema() {
        let raw = include_str!("../../../percentile-budgets.toml");
        let file: toml::Value = toml::from_str(raw).expect("percentile-budgets.toml parses");
        let budgets = file
            .get("budget")
            .and_then(toml::Value::as_array)
            .expect("[[budget]] array present");
        assert!(!budgets.is_empty(), "at least one [[budget]] entry");
        for entry in budgets {
            let budget: percentile_kit::Budget =
                entry.clone().try_into().expect("entry matches schema");
            assert!(!budget.metric.is_empty(), "metric is named");
            assert!(
                budget.p50.is_some() || budget.p99.is_some(),
                "`{}` gates at least one percentile",
                budget.metric
            );
            assert!(
                budget.max_regression_pct.is_finite() && budget.max_regression_pct >= 0.0,
                "`{}` has a sane max_regression_pct",
                budget.metric
            );
        }
    }

    /// End-to-end against a fabricated criterion dir: a budget met
    /// exactly (0% excess) passes, and a P99 breach beyond the
    /// allowance fails — the percentile-kit behavior the CI step
    /// relies on.
    #[test]
    fn gate_passes_at_budget_and_fails_on_p99_breach() {
        let temp = tempfile::tempdir().expect("tempdir");
        let criterion = temp.path().join("criterion");
        let new_dir = criterion.join("ci_gate_probe").join("new");
        std::fs::create_dir_all(&new_dir).expect("mkdir");

        // P50 (median point estimate) = 90.0 ns.
        let estimate = r#"{"confidence_interval":{"confidence_level":0.95,"lower_bound":80.0,"upper_bound":100.0},"point_estimate":90.0,"standard_error":1.0}"#;
        std::fs::write(
            new_dir.join("estimates.json"),
            format!(
                r#"{{"mean":{estimate},"median":{estimate},"slope":{estimate},"std_dev":{{"confidence_interval":{{"confidence_level":0.95,"lower_bound":0.0,"upper_bound":2.0}},"point_estimate":1.0,"standard_error":0.1}}}}"#
            ),
        )
        .expect("write estimates.json");
        // Per-iteration times: 99 x 10.0 ns and one 100.0 ns tail.
        let iters = vec!["1000"; 100].join(",");
        let mut times = vec!["10000"; 99];
        times.push("100000");
        std::fs::write(
            new_dir.join("sample.json"),
            format!(r#"{{"iters":[{iters}],"times":[{}]}}"#, times.join(",")),
        )
        .expect("write sample.json");

        let budgets_path = temp.path().join("percentile-budgets.toml");
        std::fs::write(
            &budgets_path,
            "[[budget]]\nmetric = \"ci_gate_probe\"\np50 = 90.0\np99 = 100.0\nmax_regression_pct = 10.0\n",
        )
        .expect("write budgets");
        let args = Args {
            criterion_dir: criterion.clone(),
            budgets: budgets_path.clone(),
        };
        run(&args).expect("met budgets pass (P50 exactly at budget)");

        // Tighten P99 below the observed tail: +100% excess must blow
        // past the 10% allowance and fail the gate.
        std::fs::write(
            &budgets_path,
            "[[budget]]\nmetric = \"ci_gate_probe\"\np50 = 90.0\np99 = 5.0\nmax_regression_pct = 10.0\n",
        )
        .expect("rewrite budgets");
        assert!(run(&args).is_err(), "P99 breach must fail the gate");
    }
}
