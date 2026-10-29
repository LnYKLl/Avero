//! Writes the Windows icon `assets/logo/avero.ico` from the logo
//! `assets/logo/avero-icon.svg`. Run it again after changing the logo:
//!
//! ```text
//! cargo run -p avero-app --example make_icon
//! ```
//!
//! The `.ico` holds one PNG image per size (Windows Vista and later read
//! PNG inside an icon). `build.rs` then puts it inside `avero.exe`.

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

/// The sizes Windows asks for: title bar and lists (16 to 32), the
/// desktop and Explorer (48 to 256), at 100 % to 200 % display scaling.
const SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

const LOGO: &[u8] = include_bytes!("../assets/logo/avero-icon.svg");

fn main() {
    let tree = Tree::from_data(LOGO, &Options::default()).expect("the logo is a valid SVG");
    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&size| (size, render_png(&tree, size)))
        .collect();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/logo/avero.ico");
    std::fs::write(path, ico(&images)).expect("cannot write the icon file");
    println!("{path} : {} tailles", images.len());
}

/// The logo drawn as a `size` x `size` PNG.
fn render_png(tree: &Tree, size: u32) -> Vec<u8> {
    let mut pixmap = Pixmap::new(size, size).expect("a non-zero size");
    let scale = size as f32 / tree.size().width();
    resvg::render(
        tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().expect("PNG encoding")
}

/// An `.ico` file: a 6-byte header, one 16-byte entry per image, then
/// the images themselves. All numbers are little-endian.
fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let count = images.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon
    out.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6 + 16 * u32::from(count);
    for (size, png) in images {
        // A width or height of 256 is written as 0.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        out.push(side); // width
        out.push(side); // height
        out.push(0); // no palette
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}
