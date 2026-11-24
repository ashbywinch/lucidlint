pub enum Marker {
    File { signal: String },
}

impl Marker {
    pub fn parse(text: &str) -> Vec<Marker> {
        vec![Marker::File { signal: text.to_string() }]
    }

    fn signal_and_why(rest: &str) -> (String, String) {
        (rest.to_string(), rest.to_string())
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
