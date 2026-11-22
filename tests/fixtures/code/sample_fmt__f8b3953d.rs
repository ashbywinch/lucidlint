struct S { x: i32 }
impl std::fmt::Display for S {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.x)
    }
}
