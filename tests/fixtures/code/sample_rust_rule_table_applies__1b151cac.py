pub struct M { bits: i32 }
impl M { fn get(&self, i: i32) -> bool { (self.bits >> i) & 1 == 1 } }
fn main() {
    for bits in [0b111, 0b101, 0b000] {
        let m = M { bits };
        println!("{}", check(&m, "w").join(","));
    }
}
