//! The metric names a PromQL expression reads. It reads the names, not the expression's
//! meaning: a name is an identifier outside label matchers and label lists, that isn't a
//! keyword, an aggregation or a function. Prometheus itself checks the syntax (`alert-rules`).

/// The words of PromQL that aren't metric names: operators, modifiers and literals.
const KEYWORDS: [&str; 10] = [
    "and", "or", "unless", "bool", "offset", "atan2", "inf", "nan", "start", "end",
];

/// The words after which a parenthesized list of label names follows.
const LABEL_LISTS: [&str; 6] = [
    "by",
    "without",
    "on",
    "ignoring",
    "group_left",
    "group_right",
];

/// The aggregations, which take their labels before their argument, as in `sum by (job) (x)`.
const AGGREGATIONS: [&str; 14] = [
    "sum",
    "avg",
    "min",
    "max",
    "count",
    "group",
    "stddev",
    "stdvar",
    "topk",
    "bottomk",
    "quantile",
    "count_values",
    "limitk",
    "limit_ratio",
];

/// Every metric name `expression` reads, in order, each once.
pub fn metric_names(expression: &str) -> Vec<String> {
    let chars: Vec<char> = expression.chars().collect();
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    let mut braces = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' | '\'' | '`' => i = past_string(&chars, i),
            '{' => {
                braces += 1;
                i += 1;
            }
            '}' => {
                braces = braces.saturating_sub(1);
                i += 1;
            }
            // A range or a subquery's duration, such as [5m] or [$__rate_interval].
            '[' => i = past(&chars, i, ']'),
            // A dashboard's variable, such as $__rate_interval.
            '$' => i = past_word(&chars, i + 1),
            c if c.is_ascii_digit() || (c == '.' && next_is_digit(&chars, i)) => {
                i = past_word(&chars, i);
            }
            c if c.is_ascii_alphabetic() || c == '_' || c == ':' => {
                let end = past_word(&chars, i);
                let word: String = chars[i..end].iter().collect();
                i = end;
                if braces > 0 {
                    continue;
                }
                let next = skip_spaces(&chars, i);
                if LABEL_LISTS.contains(&word.as_str()) {
                    if chars.get(next) == Some(&'(') {
                        i = past(&chars, next, ')');
                    }
                } else if KEYWORDS.contains(&word.as_str())
                    || AGGREGATIONS.contains(&word.as_str())
                    || chars.get(next) == Some(&'(')
                {
                    // A keyword, an aggregation or a function: not a metric.
                } else if !names.contains(&word) {
                    names.push(word);
                }
            }
            _ => i += 1,
        }
    }
    names
}

fn next_is_digit(chars: &[char], i: usize) -> bool {
    chars.get(i + 1).is_some_and(char::is_ascii_digit)
}

/// The index after the word that starts at `i`: letters, digits, `_`, `:` and `.`.
fn past_word(chars: &[char], mut i: usize) -> usize {
    while chars
        .get(i)
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.'))
    {
        i += 1;
    }
    i
}

/// The index after the `close` that ends the bracket opened at `i`.
fn past(chars: &[char], i: usize, close: char) -> usize {
    let mut j = i + 1;
    while j < chars.len() && chars[j] != close {
        j += 1;
    }
    j + 1
}

/// The index after the string whose quote is at `i`.
fn past_string(chars: &[char], i: usize) -> usize {
    let quote = chars[i];
    let mut j = i + 1;
    while j < chars.len() && chars[j] != quote {
        if chars[j] == '\\' && quote != '`' {
            j += 1;
        }
        j += 1;
    }
    j + 1
}

fn skip_spaces(chars: &[char], mut i: usize) -> usize {
    while chars.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obs_12_names_are_read_outside_matchers_label_lists_and_functions() {
        let expression = r#"histogram_quantile(0.99, sum by (le, http_route) (rate(http_server_request_duration_seconds_bucket{http_route=~"/v1/.+", le!="x"}[$__rate_interval])))"#;
        assert_eq!(
            metric_names(expression),
            ["http_server_request_duration_seconds_bucket"]
        );
    }

    #[test]
    fn obs_12_every_operand_of_a_binary_expression_is_read() {
        let expression = "(job:slo_request_errors:ratio_rate1h > 14.4 * 0.001 and job:slo_request_errors:ratio_rate5m > 14.4 * 0.001)
            or sum by (job, instance) (increase(otel_sdk_exporter_span_exported_total{error_type!=\"\"}[10m])) > 0
            or (sum(increase(otelcol_receiver_refused_spans[1h])) or vector(0)) + up offset 5m
            or count by (alertname, severity) (ALERTS{alertstate='firing'}) / on (job) group_left (x) platform_panics_total";
        assert_eq!(
            metric_names(expression),
            [
                "job:slo_request_errors:ratio_rate1h",
                "job:slo_request_errors:ratio_rate5m",
                "otel_sdk_exporter_span_exported_total",
                "otelcol_receiver_refused_spans",
                "up",
                "ALERTS",
                "platform_panics_total",
            ]
        );
    }

    #[test]
    fn obs_12_a_name_in_a_string_is_not_read() {
        assert_eq!(
            metric_names(r#"absent_over_time(platform_database_reachable{job="up or down"}[2m])"#),
            ["platform_database_reachable"]
        );
    }
}
