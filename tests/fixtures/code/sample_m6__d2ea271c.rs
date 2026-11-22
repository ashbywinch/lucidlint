struct ConfigManager { a: u32, b: u32, c: u32 }
impl ConfigManager {
    fn m1(&self) -> u32 { self.a }
    fn m2(&self) -> u32 { self.b }
    fn m3(&self) -> u32 { self.c }
    fn m4(&self) -> u32 { self.a + self.b }
    fn m5(&self) -> u32 { self.b + self.c }
    fn m6(&self) -> u32 { self.a + self.c }
}
