use std::path::PathBuf;

use crate::recv::Timestamp;

/// A placeholder recognized in filename [Template]s
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    Interface,
    TimestampIso,
    TimestampUnix,
    Index,
    Ext,
}

#[derive(Debug)]
enum Segment {
    Literal(String),
    Placeholder(Placeholder),
}

/// A parsed file path template.
///
/// A template is a relative or absolute path whose final component names a log file, e.g.
/// `/var/log/can/{interface}/i{index}_{interface}_{timestamp-iso}.{ext}`. Literal text is kept
/// verbatim; placeholders are evaluated as follows:
///
/// | Placeholder        | Meaning                                                     |
/// | ------------------ | ----------------------------------------------------------- |
/// | `{interface}`      | CAN interface name, e.g. `can0`                             |
/// | `{index}`          | rotation index                                              |
/// | `{timestamp-iso}`  | UTC timestamp of first message, e.g. `2026-07-28T14-33-05Z` |
/// | `{timestamp-unix}` | UTC timestamp of first message as unix epoch                |
/// | `{ext}`            | file extension for the selected log format                  |
///
/// There are additional rules:
/// * stray braces or unknown placeholders result in a parse error
/// * `{index}` and `{timestamp-*}` placeholders (which have dynamic values) may appear only in the
///   final filename component of the path, not in a directory name.
/// * There can be at most one `{index}` placeholder in the template
/// * There must be at least one literal character between any two placeholders. This literal must
///   not come from the placeholder's character class on either side
///
/// Following these rules allow for parsing directories containing files matching these template
/// patterns.
///
/// It is recommended that each network interface being logged is logged to a unique directory. It
/// is also recommended to include the rotation `{index}` in the template to decrease the amount of
/// pain and confusion clock jumps result in.
#[derive(Debug)]
pub struct Template {
    segments: Vec<Segment>,
}

/// The values bound to placeholders when resolving a [Template] to a concrete path.
#[derive(Debug, Clone, Copy)]
pub struct Values<'a> {
    pub interface: &'a str,
    pub index: u64,
    pub timestamp: Timestamp,
    pub ext: &'a str,
}

impl Placeholder {
    fn from_name(name: &str) -> Option<Placeholder> {
        match name {
            "interface" => Some(Placeholder::Interface),
            "timestamp-iso" => Some(Placeholder::TimestampIso),
            "timestamp-unix" => Some(Placeholder::TimestampUnix),
            "index" => Some(Placeholder::Index),
            "ext" => Some(Placeholder::Ext),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Placeholder::Interface => "interface",
            Placeholder::TimestampIso => "timestamp-iso",
            Placeholder::TimestampUnix => "timestamp-unix",
            Placeholder::Index => "index",
            Placeholder::Ext => "ext",
        }
    }

    /// The characters this placeholder's rendered values can contain
    fn alphabet(self) -> Option<&'static str> {
        match self {
            Placeholder::Index | Placeholder::TimestampUnix => Some("0123456789"),
            Placeholder::TimestampIso => Some("0123456789TZ-"),
            // These placeholders have static values, and are resolved to a literal value at match time
            Placeholder::Interface | Placeholder::Ext => None,
        }
    }
}

impl Template {
    /// Parse and validate a template string.
    pub fn parse(s: &str) -> eyre::Result<Template> {
        if s.is_empty() {
            eyre::bail!("empty template");
        }
        let (dir, file) = match s.rfind('/') {
            Some(i) => s.split_at(i + 1),
            None => ("", s),
        };
        if file.is_empty() {
            eyre::bail!("template {s:?} has an empty final path component");
        }

        let mut segments = Vec::new();
        tokenize(dir, &mut segments)?;
        for seg in &segments {
            // Placeholders with dynamic values can't be in directory names
            if let Segment::Placeholder(p) = seg
                && p.alphabet().is_some()
            {
                eyre::bail!(
                    "{{{}}} may only appear in the final path component",
                    p.name()
                );
            }
        }
        tokenize(file, &mut segments)?;

        let mut indices = 0;
        for (k, seg) in segments.iter().enumerate() {
            let Segment::Placeholder(p) = seg else {
                continue;
            };
            if *p == Placeholder::Index {
                indices += 1;
            }
            if k > 0
                && let Segment::Placeholder(prev) = &segments[k - 1]
            {
                eyre::bail!(
                    "placeholders {{{}}} and {{{}}} are directly adjacent; parsing could be ambiguous",
                    prev.name(),
                    p.name()
                );
            }
            if let Some(alphabet) = p.alphabet() {
                let before = match k.checked_sub(1).map(|i| &segments[i]) {
                    Some(Segment::Literal(l)) => l.chars().next_back(),
                    _ => None,
                };
                let after = match segments.get(k + 1) {
                    Some(Segment::Literal(l)) => l.chars().next(),
                    _ => None,
                };
                for c in [before, after].into_iter().flatten() {
                    if alphabet.contains(c) {
                        eyre::bail!(
                            "{{{}}} is adjacent to {c} but could also contain {c} when expanded",
                            p.name()
                        );
                    }
                }
            }
        }
        if indices > 1 {
            eyre::bail!("at most one {{index}} placeholder is allowed");
        }

        Ok(Template { segments })
    }

    /// Render a concrete path. Infallible: out-of-range timestamps clamp to jiff's bounds.
    pub fn resolve(&self, v: &Values) -> PathBuf {
        use std::fmt::Write;

        let mut out = String::new();
        for seg in &self.segments {
            match seg {
                Segment::Literal(l) => out.push_str(l),
                Segment::Placeholder(Placeholder::Interface) => out.push_str(v.interface),
                Segment::Placeholder(Placeholder::Ext) => out.push_str(v.ext),
                Segment::Placeholder(Placeholder::Index) => {
                    write!(out, "{:04}", v.index).unwrap();
                }
                Segment::Placeholder(Placeholder::TimestampUnix) => {
                    write!(out, "{}", v.timestamp.sec).unwrap();
                }
                Segment::Placeholder(Placeholder::TimestampIso) => {
                    out.push_str(&iso_utc(v.timestamp.sec));
                }
            }
        }
        PathBuf::from(out)
    }

    /// Does the template contain this placeholder?
    pub fn contains(&self, p: Placeholder) -> bool {
        self.segments
            .iter()
            .any(|s| matches!(s, Segment::Placeholder(q) if *q == p))
    }
}

/// Render seconds since the epoch as UTC at second precision, with dashes for colons.
/// Out-of-range seconds clamp to jiff's representable bounds: a garbage clock is an expected
/// input on target systems and must never prevent resolving a filename.
fn iso_utc(sec: i64) -> String {
    let ts = jiff::Timestamp::from_second(sec).unwrap_or_else(|_| {
        let clamped = if sec < 0 {
            jiff::Timestamp::MIN
        } else {
            jiff::Timestamp::MAX
        };
        tracing::warn!("timestamp {sec}s since the epoch is out of range; clamping to {clamped}");
        clamped
    });
    ts.strftime("%Y-%m-%dT%H-%M-%SZ").to_string()
}

fn tokenize(s: &str, segments: &mut Vec<Segment>) -> eyre::Result<()> {
    let mut literal = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(['{', '}']) {
        literal.push_str(&rest[..i]);
        if rest.as_bytes()[i] == b'}' {
            eyre::bail!("stray '}}' in template");
        }
        let after = &rest[i + 1..];
        let Some(j) = after
            .find(['{', '}'])
            .filter(|&j| after.as_bytes()[j] == b'}')
        else {
            eyre::bail!("stray '{{' in template");
        };
        let name = &after[..j];
        let Some(p) = Placeholder::from_name(name) else {
            eyre::bail!("unknown placeholder {{{name}}}");
        };
        if !literal.is_empty() {
            segments.push(Segment::Literal(std::mem::take(&mut literal)));
        }
        segments.push(Segment::Placeholder(p));
        rest = &after[j + 1..];
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn values(sec: i64) -> Values<'static> {
        Values {
            interface: "can0",
            index: 7,
            timestamp: Timestamp {
                sec,
                nsec: 123456789,
            },
            ext: "log",
        }
    }

    #[test]
    fn accepts_valid_templates() {
        let accept = [
            "{interface}_{index}_{timestamp-iso}.{ext}",
            "{index}_{interface}_{timestamp-iso}.{ext}",
            "/var/log/can/{interface}_{index}.{ext}",
            "{interface}/{index}_{timestamp-unix}.{ext}",
            "logs/{interface}/{interface}_{index}.log",
            "{timestamp-iso}_{timestamp-iso}_{index}.{ext}.{ext}",
            "plain.log",
            "{index}",
        ];
        for t in accept {
            let result = Template::parse(t);
            assert!(
                result.is_ok(),
                "{t:?} should parse: {}",
                result.unwrap_err()
            );
        }
    }

    #[test]
    fn rejects_invalid_templates() {
        let reject = [
            ("", "empty template"),
            ("logs/", "empty final path component"),
            ("foo{bar.log", "stray '{'"),
            ("foo{index.log", "stray '{'"),
            ("foo}bar.log", "stray '}'"),
            ("{foo}.log", "unknown placeholder {foo}"),
            ("{timestmap-iso}.log", "unknown placeholder {timestmap-iso}"),
            ("{index}/file.log", "final path component"),
            ("{timestamp-iso}/{interface}.log", "final path component"),
            ("{timestamp-unix}/{interface}.log", "final path component"),
            ("{index}_{index}.log", "at most one {index}"),
            ("{interface}{index}.log", "adjacent"),
            ("{timestamp-iso}{ext}", "adjacent"),
            ("{interface}{ext}", "adjacent"),
            ("{interface}{interface}/x.log", "adjacent"),
            ("0{index}.log", "{index} is adjacent to 0"),
            ("{index}1.log", "{index} is adjacent to 1"),
            ("{timestamp-unix}7.log", "{timestamp-unix} is adjacent to 7"),
            ("T{timestamp-iso}.log", "{timestamp-iso} is adjacent to T"),
            (
                "{timestamp-iso}-{index}.log",
                "{timestamp-iso} is adjacent to -",
            ),
        ];
        for (t, want) in reject {
            match Template::parse(t) {
                Ok(_) => panic!("{t:?} should be rejected"),
                Err(e) => assert!(
                    e.to_string().contains(want),
                    "{t:?}: error {:?} should contain {want:?}",
                    e.to_string()
                ),
            }
        }
    }

    #[test]
    fn contains_reports_placeholders() {
        let t = Template::parse("{interface}_{index}.log").unwrap();
        assert!(t.contains(Placeholder::Interface));
        assert!(t.contains(Placeholder::Index));
        assert!(!t.contains(Placeholder::Ext));
        assert!(!t.contains(Placeholder::TimestampIso));
        assert!(!t.contains(Placeholder::TimestampUnix));
    }

    #[test]
    fn resolves_default_templates() {
        // nsec is nonzero in the fixture to prove second-precision rendering ignores it.
        let v = values(1732117385);
        let t = Template::parse("{interface}_{index}_{timestamp-iso}.{ext}").unwrap();
        assert_eq!(
            t.resolve(&v),
            PathBuf::from("can0_0007_2024-11-20T15-43-05Z.log")
        );
        let t = Template::parse("{index}_{interface}_{timestamp-iso}.{ext}").unwrap();
        assert_eq!(
            t.resolve(&v),
            PathBuf::from("0007_can0_2024-11-20T15-43-05Z.log")
        );
    }

    #[test]
    fn resolves_directory_components() {
        let v = values(0);
        let t = Template::parse("/var/log/can/{interface}_{index}.{ext}").unwrap();
        assert_eq!(t.resolve(&v), PathBuf::from("/var/log/can/can0_0007.log"));
        let t = Template::parse("logs/{interface}/{interface}_{index}.log").unwrap();
        assert_eq!(t.resolve(&v), PathBuf::from("logs/can0/can0_0007.log"));
    }

    #[test]
    fn index_zero_pads_to_width_4_then_grows() {
        let t = Template::parse("{index}").unwrap();
        for (index, expected) in [(0, "0000"), (42, "0042"), (9999, "9999"), (10000, "10000")] {
            let v = Values { index, ..values(0) };
            assert_eq!(t.resolve(&v), PathBuf::from(expected));
        }
    }

    #[test]
    fn renders_unix_timestamps_as_plain_seconds() {
        let t = Template::parse("{timestamp-unix}.log").unwrap();
        assert_eq!(
            t.resolve(&values(1732117385)),
            PathBuf::from("1732117385.log")
        );
        // Pre-epoch clock: renders with a minus sign, will simply never reverse-match.
        assert_eq!(t.resolve(&values(-86400)), PathBuf::from("-86400.log"));
    }

    #[test]
    fn renders_iso_timestamps_in_utc() {
        // Expected values computed independently: date -u -d @<sec> +%Y-%m-%dT%H-%M-%SZ
        let vectors = [
            (0, "1970-01-01T00-00-00Z"),
            (1732117385, "2024-11-20T15-43-05Z"),
            (-1, "1969-12-31T23-59-59Z"),
            (-86400, "1969-12-31T00-00-00Z"),
            (4102444800, "2100-01-01T00-00-00Z"),
        ];
        let t = Template::parse("{timestamp-iso}").unwrap();
        for (sec, expected) in vectors {
            assert_eq!(t.resolve(&values(sec)), PathBuf::from(expected));
        }
    }

    #[test]
    fn out_of_range_timestamps_clamp_to_jiff_bounds() {
        let t = Template::parse("{timestamp-iso}").unwrap();
        assert_eq!(
            t.resolve(&values(i64::MIN)),
            PathBuf::from("-9999-01-02T01-59-59Z")
        );
        assert_eq!(
            t.resolve(&values(i64::MAX)),
            PathBuf::from("9999-12-30T22-00-00Z")
        );
    }
}
