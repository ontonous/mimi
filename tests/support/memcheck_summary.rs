#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemcheckSummary {
    pub rendered: String,
    pub still_reachable_bytes: u64,
    pub still_reachable_blocks: u64,
    pub still_reachable_line: String,
}

fn summary_counts(line: &str, marker: &str) -> (u64, u64) {
    let body = report_line_body(line)
        .strip_prefix(marker)
        .unwrap_or_else(|| panic!("Memcheck line lacks {marker:?}: {line}"));
    let words = body.split_whitespace().collect::<Vec<_>>();
    assert!(
        words.len() >= 5 && words[1] == "bytes" && words[2] == "in" && words[4] == "blocks",
        "unexpected Memcheck summary grammar for {marker:?}: {line}"
    );
    let bytes = words[0]
        .replace(',', "")
        .parse::<u64>()
        .unwrap_or_else(|error| panic!("invalid Memcheck byte count in {line}: {error}"));
    let blocks = words[3]
        .replace(',', "")
        .parse::<u64>()
        .unwrap_or_else(|error| panic!("invalid Memcheck block count in {line}: {error}"));
    (bytes, blocks)
}

fn report_line_body(line: &str) -> &str {
    let line = line.trim();
    if let Some(after_prefix) = line.strip_prefix("==") {
        if let Some((pid, body)) = after_prefix.split_once("==") {
            if !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()) {
                return body.trim_start();
            }
        }
    }
    line
}

fn summary_lines<'a>(report: &'a str, marker: &str) -> Vec<&'a str> {
    report
        .lines()
        .filter(|line| report_line_body(line).starts_with(marker))
        .map(str::trim)
        .collect()
}

fn assert_zero_error_summary(line: &str) {
    let body = report_line_body(line)
        .strip_prefix("ERROR SUMMARY:")
        .unwrap_or_else(|| panic!("Memcheck line lacks \"ERROR SUMMARY:\": {line}"));
    let words = body.split_whitespace().collect::<Vec<_>>();
    assert_eq!(
        words.len(),
        9,
        "unexpected Memcheck ERROR SUMMARY grammar: {line}"
    );
    assert_eq!(words[0], "0", "Memcheck reported errors: {line}");
    assert_eq!(
        words[1], "errors",
        "unexpected Memcheck error count: {line}"
    );
    assert_eq!(
        words[2], "from",
        "unexpected Memcheck error summary: {line}"
    );
    assert_eq!(
        words[3], "0",
        "unexpected Memcheck error context count: {line}"
    );
    assert_eq!(
        words[4], "contexts",
        "unexpected Memcheck error summary: {line}"
    );
    assert_eq!(
        words[5], "(suppressed:",
        "unexpected Memcheck suppression summary: {line}"
    );
    assert_eq!(
        words[7], "from",
        "unexpected Memcheck suppression summary: {line}"
    );
    let suppressed_errors = words[6].parse::<u64>().unwrap_or_else(|error| {
        panic!("invalid Memcheck suppressed error count in {line}: {error}")
    });
    let suppressed_contexts = words[8]
        .strip_suffix(')')
        .unwrap_or_else(|| panic!("unexpected Memcheck suppression summary: {line}"))
        .parse::<u64>()
        .unwrap_or_else(|error| {
            panic!("invalid Memcheck suppressed context count in {line}: {error}")
        });
    assert_eq!(
        suppressed_errors, 0,
        "strict Memcheck must not suppress any errors: {line}"
    );
    assert_eq!(
        suppressed_contexts, 0,
        "strict Memcheck must not suppress any error contexts: {line}"
    );
}

pub fn parse(report: &str) -> MemcheckSummary {
    const ALL_FREE_SUMMARY: &str = "All heap blocks were freed -- no leaks are possible";

    let all_free_lines = report
        .lines()
        .filter(|line| report_line_body(line) == ALL_FREE_SUMMARY)
        .map(str::trim)
        .collect::<Vec<_>>();
    assert!(
        all_free_lines.len() <= 1,
        "strict Memcheck report must contain at most one all-free summary, found {}:\n{report}",
        all_free_lines.len()
    );
    let all_heap_blocks_freed = all_free_lines.len() == 1;

    let error_summaries = summary_lines(report, "ERROR SUMMARY:");
    let [error_summary] = error_summaries.as_slice() else {
        panic!(
            "strict Memcheck report must contain exactly one ERROR SUMMARY line, found {}:\n{report}",
            error_summaries.len()
        );
    };
    assert_zero_error_summary(error_summary);

    let mut selected = Vec::new();
    for marker in ["definitely lost:", "indirectly lost:", "possibly lost:"] {
        let matches = summary_lines(report, marker);
        match matches.as_slice() {
            [line] => {
                let (bytes, blocks) = summary_counts(line, marker);
                assert_eq!(
                    (bytes, blocks),
                    (0, 0),
                    "strict Memcheck report contains nonzero {marker} allocations:\n{report}"
                );
                selected.push((*line).to_string());
            }
            [] if all_heap_blocks_freed => selected.push(format!(
                "{marker} 0 bytes in 0 blocks (inferred from all-free summary)"
            )),
            [] => panic!(
                "strict Memcheck report must contain exactly one {marker} line unless it proves all-free zero use:\n{report}"
            ),
            _ => panic!(
                "strict Memcheck report must contain exactly one {marker} line, found {}:\n{report}",
                matches.len()
            ),
        }
    }

    let reachable_lines = summary_lines(report, "still reachable:");
    let (still_reachable_line, still_reachable_bytes, still_reachable_blocks) =
        match reachable_lines.as_slice() {
            [line] => {
                let (bytes, blocks) = summary_counts(line, "still reachable:");
                assert!(
                    !all_heap_blocks_freed || (bytes == 0 && blocks == 0),
                    "all-free Memcheck summary conflicts with reachable allocations:\n{report}"
                );
                (
                    format!(
                        "still reachable:{}",
                        report_line_body(line)
                            .strip_prefix("still reachable:")
                            .expect("matched still-reachable summary row")
                    ),
                    bytes,
                    blocks,
                )
            }
            [] if all_heap_blocks_freed => (
                "still reachable: 0 bytes in 0 blocks (inferred from all-free summary)".into(),
                0,
                0,
            ),
            [] => panic!(
                "strict Memcheck report must contain exactly one still-reachable row unless it proves all-free zero use:\n{report}"
            ),
            _ => panic!(
                "strict Memcheck report must contain exactly one still-reachable row, found {}:\n{report}",
                reachable_lines.len()
            ),
        };

    if all_heap_blocks_freed {
        let in_use_lines = summary_lines(report, "in use at exit:");
        let [in_use_line] = in_use_lines.as_slice() else {
            panic!(
                "all-free Memcheck report must contain exactly one in-use-at-exit row, found {}:\n{report}",
                in_use_lines.len()
            );
        };
        let (bytes, blocks) = summary_counts(in_use_line, "in use at exit:");
        assert_eq!(
            (bytes, blocks),
            (0, 0),
            "all-free Memcheck summary conflicts with in-use-at-exit allocations:\n{report}"
        );
    }

    selected.push((*error_summary).to_string());
    selected.push(still_reachable_line.clone());
    if all_heap_blocks_freed {
        selected.push(ALL_FREE_SUMMARY.to_string());
    }
    MemcheckSummary {
        rendered: format!("{}\n", selected.join("\n")),
        still_reachable_bytes,
        still_reachable_blocks,
        still_reachable_line,
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    const ALL_FREE_REPORT: &str = "\
==41== HEAP SUMMARY:
==41==     in use at exit: 0 bytes in 0 blocks
==41==   total heap usage: 40 allocs, 40 frees, 2,995 bytes allocated
==41==
==41== All heap blocks were freed -- no leaks are possible
==41==
==41== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)
";

    const EXPLICIT_ZERO_REPORT: &str = "\
==42== HEAP SUMMARY:
==42==     in use at exit: 0 bytes in 0 blocks
==42==   total heap usage: 40 allocs, 40 frees, 2,995 bytes allocated
==42==
==42==     definitely lost: 0 bytes in 0 blocks
==42==     indirectly lost: 0 bytes in 0 blocks
==42==       possibly lost: 0 bytes in 0 blocks
==42==    still reachable: 0 bytes in 0 blocks
==42==
==42== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)
";

    #[test]
    fn accepts_explicit_all_free_report_with_omitted_leak_rows() {
        let summary = parse(ALL_FREE_REPORT);

        for kind in ["definitely lost", "indirectly lost", "possibly lost"] {
            assert!(summary.rendered.contains(&format!(
                "{kind}: 0 bytes in 0 blocks (inferred from all-free summary)"
            )));
        }
        assert!(summary.rendered.contains("ERROR SUMMARY: 0 errors"));
        assert!(summary
            .rendered
            .contains("All heap blocks were freed -- no leaks are possible"));
        assert_eq!(summary.still_reachable_bytes, 0);
        assert_eq!(summary.still_reachable_blocks, 0);
    }

    #[test]
    fn accepts_standard_report_with_explicit_zero_rows() {
        let summary = parse(EXPLICIT_ZERO_REPORT);

        for kind in ["definitely lost", "indirectly lost", "possibly lost"] {
            assert!(summary
                .rendered
                .contains(&format!("{kind}: 0 bytes in 0 blocks")));
        }
        assert!(summary
            .rendered
            .contains("still reachable: 0 bytes in 0 blocks"));
        assert!(summary.rendered.contains("ERROR SUMMARY: 0 errors"));
        assert_eq!(summary.still_reachable_bytes, 0);
        assert_eq!(summary.still_reachable_blocks, 0);
    }

    #[test]
    fn rejects_malformed_ambiguous_or_nonzero_reports() {
        let invalid_reports = [
            // Missing or incomplete report summaries cannot support inference.
            ALL_FREE_REPORT.replace("ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n", ""),
            ALL_FREE_REPORT.replace(
                "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
                "ERROR SUMMARY: 0 errors",
            ),
            ALL_FREE_REPORT.replace(
                "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
                "ERROR SUMMARY: 1 errors from 1 contexts (suppressed: 0 from 0)",
            ),
            ALL_FREE_REPORT.replace(
                "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
                "ERROR SUMMARY: 0 errors from 1 contexts (suppressed: 0 from 0)",
            ),
            ALL_FREE_REPORT.replace(
                "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
                "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 1 from 0)",
            ),
            ALL_FREE_REPORT.replace(
                "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
                "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 1)",
            ),
            // A nonzero explicit loss row or reachable allocation contradicts
            // the all-free claim and must never be hidden by inferred zeros.
            ALL_FREE_REPORT.replace(
                "All heap blocks were freed -- no leaks are possible",
                "definitely lost: 8 bytes in 1 blocks\n==41== All heap blocks were freed -- no leaks are possible",
            ),
            ALL_FREE_REPORT.replace(
                "All heap blocks were freed -- no leaks are possible",
                "still reachable: 8 bytes in 1 blocks\n==41== All heap blocks were freed -- no leaks are possible",
            ),
            ALL_FREE_REPORT.replace(
                "in use at exit: 0 bytes in 0 blocks",
                "in use at exit: 8 bytes in 1 blocks",
            ),
            format!("{ALL_FREE_REPORT}{ALL_FREE_REPORT}"),
            // Missing leak rows without an exact all-free summary remain incomplete.
            EXPLICIT_ZERO_REPORT.replace("definitely lost: 0 bytes in 0 blocks\n", ""),
            // A complete explicit report still rejects nonzero loss.
            EXPLICIT_ZERO_REPORT.replace(
                "definitely lost: 0 bytes in 0 blocks",
                "definitely lost: 8 bytes in 1 blocks",
            ),
            // Duplicate summary rows make the report ambiguous.
            EXPLICIT_ZERO_REPORT.replace(
                "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
                "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n==42== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
            ),
        ];

        for report in invalid_reports {
            assert!(
                catch_unwind(AssertUnwindSafe(|| parse(&report))).is_err(),
                "unexpectedly accepted malformed Memcheck report:\n{report}"
            );
        }
    }
}
