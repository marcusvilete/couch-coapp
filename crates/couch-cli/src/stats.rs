//! Percentiles for latency reports.

#[derive(Debug, Default, Clone)]
pub struct Samples {
    values: Vec<f64>,
}

impl Samples {
    pub fn push(&mut self, v: f64) {
        self.values.push(v);
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// The `p`th percentile (0 to 100), nearest-rank.
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.values.is_empty() {
            return None;
        }
        let mut sorted = self.values.clone();
        sorted.sort_by(f64::total_cmp);
        let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        Some(sorted[rank.clamp(1, sorted.len()) - 1])
    }

    /// `p50 / p95 / max` in milliseconds, or "no samples".
    pub fn summary_ms(&self) -> String {
        match (
            self.percentile(50.0),
            self.percentile(95.0),
            self.percentile(100.0),
        ) {
            (Some(p50), Some(p95), Some(max)) => format!(
                "p50 {p50:6.1} ms   p95 {p95:6.1} ms   max {max:6.1} ms   (n={})",
                self.len()
            ),
            _ => "no samples".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let mut s = Samples::default();
        for v in 1..=100 {
            s.push(f64::from(v));
        }
        assert_eq!(s.percentile(50.0), Some(50.0));
        assert_eq!(s.percentile(95.0), Some(95.0));
        assert_eq!(s.percentile(100.0), Some(100.0));
        assert_eq!(Samples::default().percentile(50.0), None);
    }
}
