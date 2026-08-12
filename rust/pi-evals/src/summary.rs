//! Eval comparison summaries, port of
//! `packages/evals/src/vitest-evals/summary.ts`.
//!
//! `styleText` (node:util) is replaced by ANSI SGR codes (gray/bold/yellow/
//! green/red), producing the same rendered output in terminals.

#[derive(Clone, Debug, PartialEq)]
pub struct HarnessObservation {
    pub eval_set: String,
    pub group_key: String,
    pub test_name: String,
    pub file: String,
    pub harness: String,
    pub baseline: String,
    pub candidates: Vec<String>,
    pub repetition: f64,
    pub total_tokens: Option<f64>,
    pub total_ms: Option<f64>,
    pub estimated_cost_usd: Option<f64>,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Scored { score: f64 },
    Unscored,
    Skipped,
    Pending,
    Errored,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PairedMetricSummary {
    pub total_pairs: usize,
    pub eligible_pairs: usize,
    pub baseline_mean: Option<f64>,
    pub candidate_mean: Option<f64>,
    pub mean_delta: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CorrectnessLiftSummary {
    pub total_pairs: usize,
    pub eligible_pairs: usize,
    pub baseline_pass_rate: Option<f64>,
    pub candidate_pass_rate: Option<f64>,
    pub lift: Option<f64>,
    pub baseline_wins: usize,
    pub candidate_wins: usize,
    pub ties: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HarnessPairComparison {
    pub baseline: String,
    pub candidate: String,
    pub correctness: CorrectnessLiftSummary,
    pub total_tokens: PairedMetricSummary,
    pub total_ms: PairedMetricSummary,
    pub estimated_cost_usd: PairedMetricSummary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HarnessComparisonDiagnostic {
    pub eval_set: String,
    pub group_key: String,
    pub test_name: String,
    pub file: String,
    pub repetition: f64,
    pub harness: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HarnessEvalSetReport {
    pub eval_set: String,
    pub comparisons: Vec<HarnessPairComparison>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HarnessComparisonReport {
    pub schema_version: f64,
    pub eval_sets: Vec<HarnessEvalSetReport>,
    pub diagnostics: Vec<HarnessComparisonDiagnostic>,
}

#[derive(Clone, Debug, PartialEq)]
struct HarnessDescriptor {
    name: String,
    index: usize,
}

#[derive(Clone, Debug)]
struct ObservationGroup {
    eval_set: String,
    group_key: String,
    test_name: String,
    file: String,
    repetition: f64,
    observations_by_harness: std::collections::HashMap<String, Vec<HarnessObservation>>,
}

#[derive(Clone, Debug)]
struct EvalSetData {
    baseline: HarnessDescriptor,
    candidates_by_name: std::collections::HashMap<String, HarnessDescriptor>,
    groups_by_key: std::collections::HashMap<String, ObservationGroup>,
}

#[derive(Clone, Debug)]
struct ObservationPair {
    baseline: HarnessObservation,
    candidate: HarnessObservation,
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

fn precise_difference(left: f64, right: f64) -> f64 {
    // JS Number((left - right).toPrecision(15)): round to 15 significant digits.
    let value = left - right;
    if value == 0.0 {
        return 0.0;
    }
    let magnitude = value.abs().log10().floor() as i32;
    let scale = 10f64.powi(14 - magnitude);
    (value * scale).round() / scale
}

fn group_key_of(observation: &HarnessObservation) -> String {
    format!(
        "[{:?},{:?},{:?}]",
        observation.file, observation.test_name, observation.group_key
    )
}

fn group_observations(observations: &[HarnessObservation]) -> std::collections::HashMap<String, EvalSetData> {
    let mut eval_sets: std::collections::HashMap<String, EvalSetData> = std::collections::HashMap::new();
    for observation in observations {
        let eval_set = eval_sets.entry(observation.eval_set.clone()).or_insert_with(|| EvalSetData {
            baseline: HarnessDescriptor {
                name: observation.baseline.clone(),
                index: 0,
            },
            candidates_by_name: std::collections::HashMap::new(),
            groups_by_key: std::collections::HashMap::new(),
        });

        for (index, name) in observation.candidates.iter().enumerate() {
            match eval_set.candidates_by_name.get(name) {
                Some(existing) if existing.index <= index => {}
                _ => {
                    eval_set.candidates_by_name.insert(name.clone(), HarnessDescriptor {
                        name: name.clone(),
                        index,
                    });
                }
            }
        }

        let group = eval_set
            .groups_by_key
            .entry(group_key_of(observation))
            .or_insert_with(|| ObservationGroup {
                eval_set: observation.eval_set.clone(),
                group_key: observation.group_key.clone(),
                test_name: observation.test_name.clone(),
                file: observation.file.clone(),
                repetition: observation.repetition,
                observations_by_harness: std::collections::HashMap::new(),
            });
        group
            .observations_by_harness
            .entry(observation.harness.clone())
            .or_default()
            .push(observation.clone());
    }
    eval_sets
}

fn ordered_harnesses(eval_set: &EvalSetData) -> Vec<HarnessDescriptor> {
    let mut candidates: Vec<HarnessDescriptor> = eval_set.candidates_by_name.values().cloned().collect();
    candidates.sort_by(|left, right| left.index.cmp(&right.index).then(left.name.cmp(&right.name)));
    let mut result = vec![eval_set.baseline.clone()];
    result.extend(candidates);
    result
}

fn ordered_candidates(eval_set: &EvalSetData) -> Vec<HarnessDescriptor> {
    let mut candidates: Vec<HarnessDescriptor> = eval_set.candidates_by_name.values().cloned().collect();
    candidates.sort_by(|left, right| left.index.cmp(&right.index).then(left.name.cmp(&right.name)));
    candidates
}

fn ordered_groups(eval_set: &EvalSetData) -> Vec<ObservationGroup> {
    let mut groups: Vec<ObservationGroup> = eval_set.groups_by_key.values().cloned().collect();
    groups.sort_by(|left, right| {
        left.group_key
            .cmp(&right.group_key)
            .then(left.repetition.partial_cmp(&right.repetition).unwrap_or(std::cmp::Ordering::Equal))
    });
    groups
}

fn collect_diagnostics(
    harnesses: &[HarnessDescriptor],
    groups: &[ObservationGroup],
) -> Vec<HarnessComparisonDiagnostic> {
    let mut diagnostics: Vec<HarnessComparisonDiagnostic> = Vec::new();
    for group in groups {
        for harness in harnesses {
            let observations = group.observations_by_harness.get(&harness.name);
            let reason = match observations {
                None => "missing-observation".to_string(),
                Some(observations) if observations.len() > 1 => "duplicate-observation".to_string(),
                Some(observations) => match observations[0].outcome {
                    Outcome::Errored => "harness-error".to_string(),
                    Outcome::Unscored => "missing-score".to_string(),
                    Outcome::Scored { .. } => continue,
                    _ => "unscorable-outcome".to_string(),
                },
            };
            diagnostics.push(HarnessComparisonDiagnostic {
                eval_set: group.eval_set.clone(),
                group_key: group.group_key.clone(),
                test_name: group.test_name.clone(),
                file: group.file.clone(),
                repetition: group.repetition,
                harness: harness.name.clone(),
                reason,
            });
        }
    }
    diagnostics
}

fn pair_observations(
    groups: &[ObservationGroup],
    baseline_harness: &str,
    candidate_harness: &str,
) -> Vec<ObservationPair> {
    let mut pairs: Vec<ObservationPair> = Vec::new();
    for group in groups {
        let baseline = group.observations_by_harness.get(baseline_harness);
        let candidate = group.observations_by_harness.get(candidate_harness);
        if let (Some(baseline), Some(candidate)) = (baseline, candidate) {
            if baseline.len() == 1 && candidate.len() == 1 {
                pairs.push(ObservationPair {
                    baseline: baseline[0].clone(),
                    candidate: candidate[0].clone(),
                });
            }
        }
    }
    pairs
}

fn summarize_metric(
    pairs: &[ObservationPair],
    select: impl Fn(&HarnessObservation) -> Option<f64>,
    total_pairs: usize,
) -> PairedMetricSummary {
    let mut baseline_values: Vec<f64> = Vec::new();
    let mut candidate_values: Vec<f64> = Vec::new();
    for pair in pairs {
        if !matches!(pair.baseline.outcome, Outcome::Scored { .. })
            || !matches!(pair.candidate.outcome, Outcome::Scored { .. })
        {
            continue;
        }
        let Some(baseline_value) = select(&pair.baseline) else { continue };
        let Some(candidate_value) = select(&pair.candidate) else { continue };
        if !baseline_value.is_finite() || !candidate_value.is_finite() {
            continue;
        }
        baseline_values.push(baseline_value);
        candidate_values.push(candidate_value);
    }

    let baseline_mean = mean(&baseline_values);
    let candidate_mean = mean(&candidate_values);
    PairedMetricSummary {
        total_pairs,
        eligible_pairs: baseline_values.len(),
        baseline_mean,
        candidate_mean,
        mean_delta: match (baseline_mean, candidate_mean) {
            (Some(baseline), Some(candidate)) => Some(precise_difference(candidate, baseline)),
            _ => None,
        },
    }
}

fn summarize_correctness(pairs: &[ObservationPair], total_pairs: usize) -> CorrectnessLiftSummary {
    let mut eligible_pairs = 0usize;
    let mut baseline_passes = 0usize;
    let mut candidate_passes = 0usize;
    let mut baseline_wins = 0usize;
    let mut candidate_wins = 0usize;
    let mut ties = 0usize;

    for pair in pairs {
        let (Some(baseline_score), Some(candidate_score)) = (
            match pair.baseline.outcome {
                Outcome::Scored { score } => Some(score),
                _ => None,
            },
            match pair.candidate.outcome {
                Outcome::Scored { score } => Some(score),
                _ => None,
            },
        ) else {
            continue;
        };
        eligible_pairs += 1;
        let baseline_passed = baseline_score >= 1.0;
        let candidate_passed = candidate_score >= 1.0;
        if baseline_passed {
            baseline_passes += 1;
        }
        if candidate_passed {
            candidate_passes += 1;
        }
        if baseline_passed == candidate_passed {
            ties += 1;
        } else if baseline_passed {
            baseline_wins += 1;
        } else {
            candidate_wins += 1;
        }
    }

    let baseline_pass_rate = if eligible_pairs == 0 {
        None
    } else {
        Some(baseline_passes as f64 / eligible_pairs as f64)
    };
    let candidate_pass_rate = if eligible_pairs == 0 {
        None
    } else {
        Some(candidate_passes as f64 / eligible_pairs as f64)
    };
    CorrectnessLiftSummary {
        total_pairs,
        eligible_pairs,
        baseline_pass_rate,
        candidate_pass_rate,
        lift: match (baseline_pass_rate, candidate_pass_rate) {
            (Some(baseline), Some(candidate)) => Some(precise_difference(candidate, baseline)),
            _ => None,
        },
        baseline_wins,
        candidate_wins,
        ties,
    }
}

fn compare_harnesses(
    baseline: &HarnessDescriptor,
    candidate: &HarnessDescriptor,
    groups: &[ObservationGroup],
) -> HarnessPairComparison {
    let pairs = pair_observations(groups, &baseline.name, &candidate.name);
    HarnessPairComparison {
        baseline: baseline.name.clone(),
        candidate: candidate.name.clone(),
        correctness: summarize_correctness(&pairs, groups.len()),
        total_tokens: summarize_metric(&pairs, |observation| observation.total_tokens, groups.len()),
        total_ms: summarize_metric(&pairs, |observation| observation.total_ms, groups.len()),
        estimated_cost_usd: summarize_metric(
            &pairs,
            |observation| observation.estimated_cost_usd,
            groups.len(),
        ),
    }
}

pub fn summarize_harness_comparisons(observations: &[HarnessObservation]) -> HarnessComparisonReport {
    let mut eval_sets: Vec<(String, EvalSetData)> = group_observations(observations).into_iter().collect();
    eval_sets.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut eval_set_reports: Vec<HarnessEvalSetReport> = Vec::new();
    let mut diagnostics: Vec<HarnessComparisonDiagnostic> = Vec::new();
    for (eval_set, data) in eval_sets {
        let harnesses = ordered_harnesses(&data);
        let candidates = ordered_candidates(&data);
        let groups = ordered_groups(&data);
        eval_set_reports.push(HarnessEvalSetReport {
            eval_set: eval_set.clone(),
            comparisons: candidates
                .iter()
                .map(|candidate| compare_harnesses(&data.baseline, candidate, &groups))
                .collect(),
        });
        diagnostics.extend(collect_diagnostics(&harnesses, &groups));
    }
    diagnostics.sort_by(|left, right| {
        left.eval_set
            .cmp(&right.eval_set)
            .then(left.file.cmp(&right.file))
            .then(left.group_key.cmp(&right.group_key))
            .then(left.repetition.partial_cmp(&right.repetition).unwrap_or(std::cmp::Ordering::Equal))
            .then(left.harness.cmp(&right.harness))
    });
    HarnessComparisonReport {
        schema_version: 1.0,
        eval_sets: eval_set_reports,
        diagnostics,
    }
}

// ---------------------------------------------------------------------------
// ANSI-styled report formatting (styleText analog)
// ---------------------------------------------------------------------------

fn style_text(style: &str, text: &str) -> String {
    let code = match style {
        "gray" => "90",
        "bold" => "1",
        "yellow" => "33",
        "green" => "32",
        "red" => "31",
        _ => return text.to_string(),
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

fn format_percentage(value: Option<f64>) -> String {
    match value {
        None => "unavailable".to_string(),
        Some(value) => format!("{:.1}%", value * 100.0),
    }
}

fn format_signed(value: f64, fraction_digits: usize) -> String {
    let sign = if value >= 0.0 { "+" } else { "" };
    format!("{sign}{value:.width$}", width = fraction_digits)
}

fn format_coverage(eligible_pairs: usize, total_pairs: usize) -> String {
    style_text("gray", &format!("({eligible_pairs}/{total_pairs} pairs)"))
}

fn format_report_line(label: &str, value: &str) -> String {
    format!("    {}  {value}", style_text("gray", &format!("{:>9}", label)))
}

fn color_delta(value: f64, formatted: &str, positive_is_better: bool) -> String {
    if value == 0.0 {
        return style_text("gray", formatted);
    }
    let improved = if positive_is_better { value > 0.0 } else { value < 0.0 };
    style_text(if improved { "green" } else { "red" }, formatted)
}

fn format_metric(
    label: &str,
    metric: &PairedMetricSummary,
    format_value: impl Fn(f64) -> String,
    format_delta: impl Fn(f64) -> String,
    comparison_pairs: usize,
) -> String {
    let coverage = if metric.eligible_pairs == 0 || metric.eligible_pairs == comparison_pairs {
        String::new()
    } else {
        format!(" {}", format_coverage(metric.eligible_pairs, metric.total_pairs))
    };
    let (Some(baseline_mean), Some(candidate_mean), Some(mean_delta)) =
        (metric.baseline_mean, metric.candidate_mean, metric.mean_delta)
    else {
        return format_report_line(label, &format!("{}{coverage}", style_text("yellow", "unavailable")));
    };
    let delta = color_delta(mean_delta, &format_delta(mean_delta), false);
    let values = style_text(
        "gray",
        &format!(
            "(candidate {}, baseline {})",
            format_value(candidate_mean),
            format_value(baseline_mean)
        ),
    );
    format_report_line(label, &format!("{delta} {values}{coverage}"))
}

pub fn format_harness_comparison_report(report: &HarnessComparisonReport) -> String {
    if report.eval_sets.iter().all(|eval_set| eval_set.comparisons.is_empty()) {
        return String::new();
    }
    let mut lines: Vec<String> = vec![style_text("bold", "Eval Comparisons")];
    for eval_set in &report.eval_sets {
        lines.push(format!("  {}", eval_set.eval_set));
        for (index, comparison) in eval_set.comparisons.iter().enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            let correctness = &comparison.correctness;
            lines.push(format_report_line("Baseline", &comparison.baseline));
            lines.push(format_report_line(
                "Candidate",
                &format!(
                    "{} {}",
                    comparison.candidate,
                    format_coverage(correctness.eligible_pairs, correctness.total_pairs)
                ),
            ));
            match correctness.lift {
                None => {
                    lines.push(format_report_line("Pass rate", &style_text("yellow", "unavailable")));
                }
                Some(lift) => {
                    let lift_pp = lift * 100.0;
                    let delta = color_delta(lift_pp, &format!("{} pp", format_signed(lift_pp, 1)), true);
                    let values = style_text(
                        "gray",
                        &format!(
                            "(candidate {}, baseline {})",
                            format_percentage(correctness.candidate_pass_rate),
                            format_percentage(correctness.baseline_pass_rate)
                        ),
                    );
                    lines.push(format_report_line("Pass rate", &format!("{delta} {values}")));
                }
            }
            lines.push(format_metric(
                "Tokens",
                &comparison.total_tokens,
                |value| format!("{value:.1}"),
                |value| format_signed(value, 1),
                correctness.eligible_pairs,
            ));
            lines.push(format_metric(
                "Latency",
                &comparison.total_ms,
                |value| format!("{value:.1}ms"),
                |value| format!("{}ms", format_signed(value, 1)),
                correctness.eligible_pairs,
            ));
            lines.push(format_metric(
                "Est. cost",
                &comparison.estimated_cost_usd,
                |value| format!("${value:.4}"),
                |value| {
                    let sign = if value >= 0.0 { "+" } else { "-" };
                    format!("{sign}${:.4}", value.abs())
                },
                correctness.eligible_pairs,
            ));
        }
    }
    if !report.diagnostics.is_empty() {
        lines.push(format!("  {}", style_text("yellow", "Incomplete observations")));
        for diagnostic in &report.diagnostics {
            lines.push(format!(
                "    {}: {}/{} repetition {}, harness {}",
                diagnostic.reason,
                diagnostic.file,
                diagnostic.test_name,
                diagnostic.repetition,
                diagnostic.harness
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(
        eval_set: &str,
        group_key: &str,
        harness: &str,
        baseline: &str,
        candidates: &[&str],
        repetition: f64,
        score: Option<f64>,
    ) -> HarnessObservation {
        HarnessObservation {
            eval_set: eval_set.to_string(),
            group_key: group_key.to_string(),
            test_name: "t".to_string(),
            file: "f.ts".to_string(),
            harness: harness.to_string(),
            baseline: baseline.to_string(),
            candidates: candidates.iter().map(|name| name.to_string()).collect(),
            repetition,
            total_tokens: Some(100.0),
            total_ms: Some(50.0),
            estimated_cost_usd: Some(0.01),
            outcome: match score {
                Some(score) => Outcome::Scored { score },
                None => Outcome::Errored,
            },
        }
    }

    #[test]
    fn summarizes_single_pair() {
        let observations = vec![
            observation("set1", "g1", "base", "base", &["cand"], 0.0, Some(1.0)),
            observation("set1", "g1", "cand", "base", &["cand"], 0.0, Some(0.0)),
        ];
        let report = summarize_harness_comparisons(&observations);
        assert_eq!(report.schema_version, 1.0);
        assert_eq!(report.eval_sets.len(), 1);
        let comparison = &report.eval_sets[0].comparisons[0];
        assert_eq!(comparison.baseline, "base");
        assert_eq!(comparison.candidate, "cand");
        assert_eq!(comparison.correctness.eligible_pairs, 1);
        assert_eq!(comparison.correctness.baseline_wins, 1);
        assert_eq!(comparison.correctness.candidate_wins, 0);
        assert_eq!(comparison.correctness.ties, 0);
        assert_eq!(comparison.correctness.baseline_pass_rate, Some(1.0));
        assert_eq!(comparison.correctness.candidate_pass_rate, Some(0.0));
        assert_eq!(comparison.correctness.lift, Some(-1.0));
        assert_eq!(comparison.total_tokens.eligible_pairs, 1);
        assert_eq!(comparison.total_tokens.mean_delta, Some(0.0));
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn reports_missing_observations() {
        let observations = vec![observation("set1", "g1", "base", "base", &["cand"], 0.0, Some(1.0))];
        let report = summarize_harness_comparisons(&observations);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].harness, "cand");
        assert_eq!(report.diagnostics[0].reason, "missing-observation");
    }

    #[test]
    fn sorts_eval_sets_and_groups() {
        let observations = vec![
            observation("b", "z", "base", "base", &["cand"], 0.0, Some(1.0)),
            observation("b", "z", "cand", "base", &["cand"], 0.0, Some(1.0)),
            observation("a", "a", "base", "base", &["cand"], 0.0, Some(1.0)),
            observation("a", "a", "cand", "base", &["cand"], 0.0, Some(1.0)),
        ];
        let report = summarize_harness_comparisons(&observations);
        assert_eq!(report.eval_sets[0].eval_set, "a");
        assert_eq!(report.eval_sets[1].eval_set, "b");
    }

    #[test]
    fn summarizes_correctness_lift() {
        let baseline = "baseline";
        let candidates = vec!["candidate"];
        let observations = vec![
            observation("set1", "g1", baseline, baseline, &candidates, 0.0, Some(1.0)),
            observation("set1", "g1", "candidate", baseline, &candidates, 0.0, Some(1.0)),
            observation("set1", "g2", baseline, baseline, &candidates, 0.0, Some(0.0)),
            observation("set1", "g2", "candidate", baseline, &candidates, 0.0, Some(1.0)),
        ];
        let report = summarize_harness_comparisons(&observations);
        assert_eq!(report.schema_version, 1.0);
        assert_eq!(report.eval_sets.len(), 1);
        let comparison = &report.eval_sets[0].comparisons[0];
        assert_eq!(comparison.correctness.total_pairs, 2);
        assert_eq!(comparison.correctness.eligible_pairs, 2);
        assert_eq!(comparison.correctness.baseline_wins, 0);
        assert_eq!(comparison.correctness.candidate_wins, 1);
        assert_eq!(comparison.correctness.ties, 1);
        assert_eq!(comparison.correctness.baseline_pass_rate.unwrap(), 0.5);
        assert_eq!(comparison.correctness.candidate_pass_rate.unwrap(), 1.0);
        assert_eq!(comparison.correctness.lift.unwrap(), 0.5);
    }

    #[test]
    fn collects_diagnostics_for_missing_and_duplicate() {
        let observations = vec![
            observation("set1", "g1", "base", "base", &["cand"], 0.0, Some(1.0)),
            observation("set1", "g1", "base", "base", &["cand"], 0.0, Some(1.0)),
            observation("set1", "g1", "cand", "base", &["cand"], 0.0, None),
        ];
        let report = summarize_harness_comparisons(&observations);
        let reasons: Vec<&str> = report.diagnostics.iter().map(|diagnostic| diagnostic.reason.as_str()).collect();
        assert!(reasons.contains(&"duplicate-observation"));
        assert!(reasons.contains(&"harness-error"));
    }

    #[test]
    fn precise_difference_rounds() {
        // JS Number((0.1+0.2-0.3).toPrecision(15)) keeps 15 significant
        // digits; floating-point noise is NOT zeroed.
        let value = precise_difference(0.1 + 0.2, 0.3);
        assert!((value - 5.55111512312578e-17).abs() < 1e-20);
        assert_eq!(precise_difference(1.5, 1.0), 0.5);
        assert_eq!(precise_difference(1.0, 1.0), 0.0);
    }

    #[test]
    fn formats_report() {
        let observations = vec![
            observation("set1", "g1", "base", "base", &["cand"], 0.0, Some(1.0)),
            observation("set1", "g1", "cand", "base", &["cand"], 0.0, Some(0.0)),
        ];
        let report = summarize_harness_comparisons(&observations);
        let text = format_harness_comparison_report(&report);
        assert!(text.contains("Eval Comparisons"));
        assert!(text.contains("set1"));
        assert!(text.contains("-100.0 pp"));
        assert!(text.contains("Pass rate"));
    }

    #[test]
    fn empty_report_formats_empty() {
        let report = summarize_harness_comparisons(&[]);
        assert_eq!(format_harness_comparison_report(&report), "");
    }
}
