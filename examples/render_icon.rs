//! Renders `packaging/linux/files.svg` to PNG: `cargo run --example render_icon -- <size> <out.png>`.
//! Used by `make icon` for the README, through the same patina-apps code the window's icon uses, so
//! it needs no system SVG tools.

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: render_icon <size> <out.png>";
    let size: u32 = args.next().and_then(|s| s.parse().ok()).expect(usage);
    let out = args.next().expect(usage);
    let svg = std::fs::read("packaging/linux/files.svg").expect("read packaging/linux/files.svg");
    let rgba = patina_apps::icon::svg(&svg, size).expect("the icon is a valid SVG");
    // tiny-skia renders premultiplied; a PNG wants it straight.
    let rgba: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] as u16;
            let un = |c: u8| (c as u16 * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
            [un(p[0]), un(p[1]), un(p[2]), p[3]]
        })
        .collect();
    image::RgbaImage::from_raw(size, size, rgba).expect("the size matches").save(&out).expect("write the PNG");
    println!("{out}");
}
