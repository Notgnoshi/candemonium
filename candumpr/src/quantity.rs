use std::time::Duration;

/// A parsed configuration quantity: `"off"`, a duration, a size, or a file count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quantity {
    Off,
    Duration(Duration),
    Bytes(u64),
    Count(u64),
}

fn strip_suffix<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let split = s.len().checked_sub(suffix.len())?;
    let (head, tail) = (s.get(..split)?, s.get(split..)?);
    // case insensitive
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

impl std::str::FromStr for Quantity {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("off") {
            return Ok(Quantity::Off);
        }
        if let Some(count) = strip_suffix(raw, "files").or_else(|| strip_suffix(raw, "file")) {
            let count = count
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("invalid file count {raw:?}"))?;
            return Ok(Quantity::Count(count));
        }
        // A bare number is rejected rather than silently meaning bytes: bytesize would take it,
        // and "100" is far more likely to be a forgotten unit than a 100 byte limit.
        if raw.ends_with(['b', 'B']) {
            let size = raw
                .parse::<bytesize::ByteSize>()
                .map_err(|e| format!("invalid size {raw:?}: {e}"))?;
            return Ok(Quantity::Bytes(size.as_u64()));
        }
        let span: jiff::Span = raw.parse().map_err(|_| {
            format!(
                "expected a size like \"100MB\", a duration like \"30min\", a count like \
                 \"10 files\", or \"off\", got {raw:?}"
            )
        })?;
        // Days and weeks convert at a fixed 24h/7d.
        //
        // Months and years don't make sense for rotation or retention intervals, so don't support them.
        if span.get_years() != 0 || span.get_months() != 0 {
            return Err(format!("months and years are not supported, got {raw:?}"));
        }
        let signed = span
            .to_duration(jiff::SpanRelativeTo::days_are_24_hours())
            .map_err(|e| format!("invalid duration {raw:?}: {e}"))?;
        let duration = Duration::try_from(signed)
            .map_err(|_| format!("duration must not be negative, got {raw:?}"))?;
        Ok(Quantity::Duration(duration))
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn quantity_tells_the_variants_apart() {
        fn quantity(value: &str) -> Result<Quantity, String> {
            value.parse()
        }
        fn secs(secs: u64) -> Result<Quantity, String> {
            Ok(Quantity::Duration(Duration::from_secs(secs)))
        }

        assert_eq!(quantity("off"), Ok(Quantity::Off));
        assert_eq!(quantity(" OFF "), Ok(Quantity::Off));

        assert_eq!(quantity("1m"), secs(60));
        assert_eq!(quantity("1MB"), Ok(Quantity::Bytes(1_000_000)));

        assert_eq!(quantity("30min"), secs(1800));
        assert_eq!(quantity("1min 30s"), secs(90));
        assert_eq!(quantity("3 days"), secs(3 * 24 * 3600));
        assert_eq!(quantity("1 week"), secs(7 * 24 * 3600));

        assert_eq!(quantity("100MiB"), Ok(Quantity::Bytes(104_857_600)));
        assert_eq!(quantity("512b"), Ok(Quantity::Bytes(512)));

        assert_eq!(quantity("10 files"), Ok(Quantity::Count(10)));
        assert_eq!(quantity("1 file"), Ok(Quantity::Count(1)));

        let err = quantity("100").unwrap_err();
        assert!(err.contains("expected a size like"), "got: {err}");
        let err = quantity("soon").unwrap_err();
        assert!(err.contains("expected a size like"), "got: {err}");
        let err = quantity("2 months").unwrap_err();
        assert!(err.contains("months and years"), "got: {err}");
        let err = quantity("-3s").unwrap_err();
        assert!(err.contains("must not be negative"), "got: {err}");
        let err = quantity("ten files").unwrap_err();
        assert!(err.contains("invalid file count"), "got: {err}");
    }
}
