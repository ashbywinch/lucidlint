//! The marker grammar: `lucidlint: ignore` / `ignore-file` comments into the
//! directives the suppression engine folds into its maps.

/// One suppression directive parsed from a comment. `from_comments` turns a
/// comment stream into markers, then folds each marker into a `Suppressions`;
/// the parse and the accumulation are separate steps, so a per-comment case is
/// testable without building the maps.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Marker {
    /// `lucidlint: ignore-file <signal>[,<signal>...] <why>` — exempts each
    /// signal anywhere in the file.
    File { signal: String, why: String },
    /// `lucidlint: ignore <signal>[,<signal>...] <why>` — exempts each signal
    /// at the comment's line (and the line above, by the marker window).
    Line { line: usize, signal: String, why: String },
}

impl Marker {
    /// The markers one comment declares. A comment holds at most one directive:
    /// `ignore` may name several comma-separated signals, which share the line
    /// and the why. A marker with no why still parses — the binding check later
    /// decides it is why-less and exempts nothing (G5).
    pub(super) fn parse(line: usize, text: &str) -> Vec<Marker> {
        let trimmed = text.trim_start_matches(['#', '/']).trim_start();
        if let Some(rest) = trimmed.strip_prefix("lucidlint: ignore-file ") {
            let (signals, why) = Self::signal_and_why(rest);
            // the file form shares the line form's comma list: one marker
            // exempts every named signal for the whole file
            return signals
                .split(',')
                .map(str::trim)
                .filter(|signal| !signal.is_empty())
                .map(|signal| Marker::File {
                    signal: signal.to_string(),
                    why: why.clone(),
                })
                .collect();
        }
        let Some(rest) = trimmed.strip_prefix("lucidlint: ignore ") else {
            return Vec::new();
        };
        let (signals, why) = Self::signal_and_why(rest);
        // comma-separated signals let one comment exempt several families
        // (`ignore long-param-list,detached-method <why>`) — stacked
        // comments only fit the line/line-1 window for the last one
        signals
            .split(',')
            .map(str::trim)
            .filter(|signal| !signal.is_empty())
            .map(|signal| Marker::Line {
                line,
                signal: signal.to_string(),
                why: why.clone(),
            })
            .collect()
    }

    /// The first whitespace-delimited token and the remaining text as the why.
    fn signal_and_why(rest: &str) -> (String, String) {
        let mut it = rest.splitn(2, char::is_whitespace);
        let signal = it.next().unwrap_or("").to_string();
        let why = it.next().unwrap_or("").trim().to_string();
        (signal, why)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_file_marker_and_a_comma_line_marker() {
        let file = Marker::parse(3, "// lucidlint: ignore-file closures the walker is one pass");
        assert_eq!(
            file,
            vec![Marker::File {
                signal: "closures".to_string(),
                why: "the walker is one pass".to_string(),
            }]
        );
        let line = Marker::parse(7, "// lucidlint: ignore closed-classes,closure-strewing one site");
        assert_eq!(
            line,
            vec![
                Marker::Line {
                    line: 7,
                    signal: "closed-classes".to_string(),
                    why: "one site".to_string(),
                },
                Marker::Line {
                    line: 7,
                    signal: "closure-strewing".to_string(),
                    why: "one site".to_string(),
                },
            ]
        );
    }

    #[test]
    fn a_file_marker_parses_the_same_signal_list_as_a_line_marker() {
        // the file form used to keep the comma list as ONE unknown signal, so
        // `ignore-file fakefs,monkeypatch` suppressed neither family
        let file = Marker::parse(
            1,
            "# lucidlint: ignore-file fakefs,monkeypatch real fs and an SDK patch are the subject",
        );
        assert_eq!(
            file,
            vec![
                Marker::File {
                    signal: "fakefs".to_string(),
                    why: "real fs and an SDK patch are the subject".to_string(),
                },
                Marker::File {
                    signal: "monkeypatch".to_string(),
                    why: "real fs and an SDK patch are the subject".to_string(),
                },
            ]
        );
    }

    #[test]
    fn a_why_less_marker_parses_with_an_empty_why() {
        // the binding check rejects a why-less marker; the parse still keeps it
        // so stale bookkeeping sees the marker the author wrote (G5)
        assert_eq!(
            Marker::parse(2, "// lucidlint: ignore closures"),
            vec![Marker::Line {
                line: 2,
                signal: "closures".to_string(),
                why: String::new(),
            }]
        );
    }

    #[test]
    fn an_ordinary_comment_yields_no_marker() {
        assert!(Marker::parse(1, "// a note about the walk").is_empty());
        assert!(Marker::parse(1, "# lucidlint: ignore-file ").is_empty());
    }
}
