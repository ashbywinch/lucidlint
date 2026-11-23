struct S {}
impl S {
    fn m(&self, x: i32) -> i32 {
        self.x + x
    }
}
