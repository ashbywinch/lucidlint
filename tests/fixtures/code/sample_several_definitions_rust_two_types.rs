pub struct Marker {
    signal: String,
}

impl Marker {
    pub fn signal(&self) -> &str {
        &self.signal
    }
}

pub struct Suppressions {
    markers: Vec<Marker>,
}

impl Suppressions {
    pub fn add(&mut self, marker: Marker) {
        self.markers.push(marker);
    }
}

pub fn scan(text: &str) -> usize {
    text.len()
}
