/// Warm-run statistics: population mean and population standard
/// deviation over every sample except the first (the cold run), matching
/// `warm_stats` in the original Python harness.
#[derive(Clone, Copy)]
pub struct WarmStats {
    pub mean: f64,
    pub sigma: f64,
}

impl WarmStats {
    pub fn from_samples(samples: &[f64]) -> WarmStats {
        let warm: &[f64] = if samples.len() > 1 {
            &samples[1..]
        } else {
            samples
        };
        if warm.is_empty() {
            return WarmStats {
                mean: 0.0,
                sigma: 0.0,
            };
        }
        let n = warm.len() as f64;
        let mean = warm.iter().sum::<f64>() / n;
        let variance = warm.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n;
        WarmStats {
            mean,
            sigma: variance.sqrt(),
        }
    }
}

pub fn fmt_seconds(seconds: f64) -> String {
    format!("{seconds:.3} s")
}

/// rom-converto / reference ratio, with a parenthesised speed-up when
/// rom-converto wins, exactly as the Python `delta_ratio`.
pub fn delta_ratio(rom_mean: f64, ref_mean: f64) -> String {
    if ref_mean == 0.0 {
        return "n/a".to_string();
    }
    let ratio = rom_mean / ref_mean;
    if ratio < 1.0 {
        let speedup = ref_mean / rom_mean;
        format!("{ratio:.2}x ({speedup:.2}x faster)")
    } else {
        format!("{ratio:.2}x")
    }
}

/// Signed byte delta with thousands separators plus a percentage, e.g.
/// `-63,268 B (-0.0029 %)`, matching the `.md` size-delta column.
pub fn fmt_size_delta(rom_size: u64, ref_size: u64) -> String {
    let diff = rom_size as i64 - ref_size as i64;
    let pct = if ref_size == 0 {
        0.0
    } else {
        100.0 * diff as f64 / ref_size as f64
    };
    format!("{} B ({pct:+.4} %)", fmt_thousands_signed(diff))
}

/// Humanized byte size for the Output column, e.g. `915 MB`, `1.4 GB`.
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn fmt_thousands_signed(n: i64) -> String {
    let sign = if n < 0 { "-" } else { "+" };
    let digits = n.unsigned_abs().to_string();
    let len = digits.len();
    let mut grouped = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}{grouped}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_stats_excludes_first_sample() {
        let s = WarmStats::from_samples(&[100.0, 2.0, 4.0]);
        assert_eq!(s.mean, 3.0);
        assert_eq!(s.sigma, 1.0);
    }

    #[test]
    fn delta_ratio_formats_speedup_when_faster() {
        assert_eq!(delta_ratio(2.0, 4.0), "0.50x (2.00x faster)");
        assert_eq!(delta_ratio(4.0, 2.0), "2.00x");
    }

    #[test]
    fn size_delta_groups_thousands_with_sign() {
        assert_eq!(
            fmt_size_delta(1_000_000, 1_063_268),
            "-63,268 B (-5.9503 %)"
        );
        assert_eq!(fmt_size_delta(100, 100), "+0 B (+0.0000 %)");
    }
}
