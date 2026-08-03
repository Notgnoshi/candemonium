/// Render seconds since the epoch as UTC at second precision, with dashes for colons
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

/// Render a filename given various parameters.
pub fn render(index: u64, interface: &str, sec: i64, ext: &str) -> String {
    format!("i{index:04}_{interface}_{}.{ext}", iso_utc(sec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_utc_renders_and_clamps() {
        let vectors = [
            (0, "1970-01-01T00-00-00Z"),
            (1732117385, "2024-11-20T15-43-05Z"),
            (-86400, "1969-12-31T00-00-00Z"),
            (i64::MIN, "-9999-01-02T01-59-59Z"),
            (i64::MAX, "9999-12-30T22-00-00Z"),
        ];
        for (sec, expected) in vectors {
            assert_eq!(iso_utc(sec), expected, "sec={sec}");
        }
    }

    #[test]
    fn template_filename_renders_the_fixed_scheme() {
        assert_eq!(
            render(0, "can0", 1732117385, "log"),
            "i0000_can0_2024-11-20T15-43-05Z.log"
        );
        // The index pads to width 4 and keeps counting past 9999 as plain decimal.
        assert_eq!(
            render(10000, "vcan1", 0, "pcap"),
            "i10000_vcan1_1970-01-01T00-00-00Z.pcap"
        );
    }
}
