struct Line {
    low: f64,
    high: f64,
}

impl Line {
    fn joins(&self, other: &Line) -> bool {
        self.low <= other.high && other.low <= self.high
    }

    fn is_ready(&self) -> bool {
        self.low > 0.0 && self.high > self.low && self.low < 100.0
    }
}
