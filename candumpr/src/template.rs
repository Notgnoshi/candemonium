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

    /// Does the template contain this placeholder?
    pub fn contains(&self, p: Placeholder) -> bool {
        self.segments
            .iter()
            .any(|s| matches!(s, Segment::Placeholder(q) if *q == p))
    }
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
    use super::*;

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
}
