fn main() {
    for sel in ["a", "b", "c", "zz"] {
        println!("{} -> {}", sel, route(sel, 5));
    }
}
