pub struct Marker {
    signal: String,
}

impl Marker {
    pub fn signal(&self) -> &str {
        &self.signal
    }
}

pub fn render_expr(e: &Expr) -> u32 {
    1
}

pub fn width_expr(e: &Expr) -> u32 {
    2
}

pub fn scan(text: &str) -> usize {
    text.len()
}
