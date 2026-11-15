//! The `digup status` text helpers, measured from Swift on this box (not
//! assumed): `main.swift`'s `tilde`/`pad`/`lpad`/`number`, `statesLine`, and
//! Foundation's `ByteCountFormatter` at `countStyle = .file`.
//!
//! The measured `.file` rules (LANG=C.UTF-8, the container's locale):
//!   * 0 → "Zero KB"; 1 → "1 byte"; 2..999 → "N bytes";
//!   * the band is the largest 1000^u (u = 1..=6, KB..EB) whose value ≤ bytes;
//!   * fraction digits by band: KB 0, MB 1, GB/TB/PB/EB 2;
//!   * rounding is half-to-even (1.5 KB → "2 KB", 10.5 KB → "10 KB"), trailing
//!     zeros are stripped ("1.50 MB" → "1.5 MB", "1.00 GB" → "1 GB");
//!   * a value rounding to 1000 stays in its band, grouped: 999999 → "1,000 KB",
//!     999999999 → "1,000 MB" (measured — the unit never carries over).
//!
//! `number(Int)` is `.formatted(.number)`: 3-digit grouping with commas.

use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

pub fn number(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 {
        out.insert(0, '-');
    }
    out
}

/// Swift's `String.count` is UAX #29 extended grapheme clusters — a Bengali conjunct
/// (য + ্ + ন) is ONE character there. Pads measured in code points would shift every
/// multilingual eval line (the evals quote Bengali/Arabic text through pad/lpad).
pub fn pad(text: &str, width: usize) -> String {
    let count = text.graphemes(true).count();
    if count >= width {
        text.to_string()
    } else {
        format!("{}{}", text, " ".repeat(width - count))
    }
}

pub fn lpad(text: &str, width: usize) -> String {
    let count = text.graphemes(true).count();
    if count >= width {
        text.to_string()
    } else {
        format!("{}{}", " ".repeat(width - count), text)
    }
}

/// `main.swift:159` — home spelled `~`.
pub fn tilde(path: &str, home: &str) -> String {
    if !home.is_empty() && path.starts_with(home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

/// `statesLine` (`main.swift:346`): states sorted by name, "N state" joined by " · ".
pub fn states_line(states: &HashMap<String, i64>) -> String {
    let mut entries: Vec<(&String, &i64)> = states.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .iter()
        .map(|(state, count)| format!("{} {}", count, state))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `duration(_:)` (main.swift:164): "N s" under 90, "N min" under an hour and
/// a half, else "X.Y h".
/// `clock(_:)` (main.swift:170): "H:MM:SS" from an hour on, else "M:SS".
pub fn clock(seconds: f64) -> String {
    let total = seconds as i64; // Swift's Int(Double): truncate
    if total >= 3600 {
        format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
    } else {
        format!("{}:{:02}", total / 60, total % 60)
    }
}

pub fn duration(seconds: f64) -> String {
    if seconds < 90.0 {
        format!("{} s", seconds.round() as i64)
    } else if seconds < 5400.0 {
        format!("{} min", (seconds / 60.0).round() as i64)
    } else {
        format!("{:.1} h", seconds / 3600.0)
    }
}

const UNITS: [&str; 7] = ["", "KB", "MB", "GB", "TB", "PB", "EB"];

/// Foundation's `ByteCountFormatter.string(fromByteCount:countStyle:.file)`,
/// per the measured table above. Int64 tops out inside the EB band, so the
/// band loop ends there (measured: 9223372036854775807 → "9.22 EB").
pub fn file_size(bytes: i64) -> String {
    if bytes == 0 {
        return "Zero KB".to_string();
    }
    if bytes < 1000 {
        return format!(
            "{} byte{}",
            number(bytes),
            if bytes == 1 { "" } else { "s" }
        );
    }
    let mut unit = 1usize;
    let mut scale = 1000.0f64;
    while unit < 6 && (bytes as f64) >= scale * 1000.0 {
        unit += 1;
        scale *= 1000.0;
    }
    let value = bytes as f64 / scale;
    let digits = match unit {
        1 => 0,
        2 => 1,
        _ => 2,
    };
    // Rust's float Display rounds half-to-even, the same as CFNumberFormatter's
    // default (`{:.0}` of 10.5 is "10", of 1.5 is "2").
    let rounded = format!("{value:.digits$}");
    let (head, tail) = match rounded.split_once('.') {
        Some((head, tail)) => (head, Some(tail.trim_end_matches('0'))),
        None => (rounded.as_str(), None),
    };
    let grouped = number(head.parse::<i64>().expect("rounded value"));
    let shown = match tail {
        Some(tail) if !tail.is_empty() => format!("{grouped}.{tail}"),
        _ => grouped,
    };
    format!("{shown} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_groups_threes() {
        // Measured against Swift `number()` on this box.
        assert_eq!(number(0), "0");
        assert_eq!(number(42), "42");
        assert_eq!(number(999), "999");
        assert_eq!(number(1000), "1,000");
        assert_eq!(number(1234), "1,234");
        assert_eq!(number(12345), "12,345");
        assert_eq!(number(1234567), "1,234,567");
        assert_eq!(number(123456789), "123,456,789");
        assert_eq!(number(999999999999), "999,999,999,999");
        assert_eq!(number(12345678901234), "12,345,678,901,234");
    }

    #[test]
    fn file_size_matches_the_measured_table() {
        // Every line measured from ByteCountFormatter(.file) on this box.
        let cases = [
            (0, "Zero KB"),
            (1, "1 byte"),
            (12, "12 bytes"),
            (500, "500 bytes"),
            (999, "999 bytes"),
            (1000, "1 KB"),
            (1001, "1 KB"),
            (1234, "1 KB"),
            (1500, "2 KB"),
            (9999, "10 KB"),
            (10000, "10 KB"),
            (10500, "10 KB"),
            (12345, "12 KB"),
            (99999, "100 KB"),
            (123456, "123 KB"),
            (831456, "831 KB"),
            (999999, "1,000 KB"),
            (1000000, "1 MB"),
            (1048576, "1 MB"),
            (1200000, "1.2 MB"),
            (1234567, "1.2 MB"),
            (1500000, "1.5 MB"),
            (9999999, "10 MB"),
            (12345678, "12.3 MB"),
            (123456789, "123.5 MB"),
            (999999999, "1,000 MB"),
            (1000000000, "1 GB"),
            (1234567890, "1.23 GB"),
            (12345678901, "12.35 GB"),
            (123456789012, "123.46 GB"),
            (999999999999, "1,000 GB"),
            (1000000000000, "1 TB"),
            (12345678901234, "12.35 TB"),
            (999999999999999, "1,000 TB"),
            (1000000000000000000, "1 EB"),
            (1230000000000000000, "1.23 EB"),
            (9223372036854775807, "9.22 EB"),
        ];
        for (bytes, want) in cases {
            assert_eq!(file_size(bytes), want, "bytes={bytes}");
        }
    }

    #[test]
    fn tilde_pads_and_states() {
        assert_eq!(tilde("/home/user/x", "/home/user"), "~/x");
        assert_eq!(tilde("/var/x", "/home/user"), "/var/x");
        assert_eq!(pad("screenshot", 11), "screenshot ");
        assert_eq!(pad("code", 11), "code       ");
        assert_eq!(lpad("4", 6), "     4");
        let mut states = HashMap::new();
        states.insert("done".to_string(), 2);
        states.insert("failed".to_string(), 1);
        states.insert("skipped".to_string(), 5);
        assert_eq!(states_line(&states), "2 done · 1 failed · 5 skipped");
    }
}
