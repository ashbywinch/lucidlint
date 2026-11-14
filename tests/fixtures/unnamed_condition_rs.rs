struct Line {
    low: f64,
    high: f64,
    thickness: f64,
}

impl Line {
    fn joins(&self, other: &Line, tolerance: f64) -> bool {
        self.low - tolerance <= other.high && other.low <= self.high + tolerance && self.thickness > 0.0
    }
}

fn decide(a: &Line, b: &Line, ready: bool) -> bool {
    if a.low > b.high && ready && a.thickness > 2.0 {
        return true;
    }
    false
}
