//! Prints a few pixels of a PNG, to check what a screenshot mode actually saved.
//!
//! `cargo run -p prowave-app --example png_pixel -- <file.png>`

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "shot.png".to_string());
    let image = image::open(&path).expect("open png").to_rgba8();
    println!("{}: {}x{}", path, image.width(), image.height());
    for (x, y) in [(500, 300), (500, 75), (400, 150), (60, 400), (1500, 800)] {
        let p = image.get_pixel(x, y);
        println!("{x},{y}: {:?}", p.0);
    }
}
