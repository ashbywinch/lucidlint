fn sample() {
    let code = "fn f() {\n    let x = 1;\n}\n";
    let _ = code;
}

fn feed() {
    scan("fn f() {}");
}

struct Runner;

impl Runner {
    fn scan_src(&self, _snippet: &str) {}

    fn feed(&self) {
        self.scan_src("fn f() {}");
    }
}
