use resvg::{
    tiny_skia::{Pixmap, Transform},
    usvg::{Options, Tree, TreeParsing},
};
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/waveinput.svg");

    let svg = fs::read("assets/waveinput.svg").expect("read WaveInput SVG");
    let tree = Tree::from_data(&svg, &Options::default()).expect("parse WaveInput SVG");
    let render_tree = resvg::Tree::from_usvg(&tree);
    let output_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"));

    let mut ico_images = Vec::new();
    for size in [16, 24, 32, 48, 64, 128, 256] {
        let pixmap = render_icon(&render_tree, size);
        if size == 64 {
            fs::write(
                output_dir.join("waveinput-64.rgba"),
                unpremultiply(pixmap.data()),
            )
            .expect("write embedded WaveInput pixels");
        }
        ico_images.push((size, pixmap.encode_png().expect("encode WaveInput PNG")));
    }

    let ico_path = output_dir.join("waveinput.ico");
    fs::write(&ico_path, encode_ico(&ico_images)).expect("write WaveInput ICO");
    if env::var("CARGO_CFG_WINDOWS").is_ok() {
        winresource::WindowsResource::new()
            .set_icon(ico_path.to_str().expect("ICO path is UTF-8"))
            .compile()
            .expect("embed WaveInput Windows resources");
    }
}

fn render_icon(tree: &resvg::Tree, size: u32) -> Pixmap {
    let mut pixmap = Pixmap::new(size, size).expect("valid icon dimensions");
    let scale = size as f32 / tree.size.width();
    tree.render(Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap
}

fn unpremultiply(pixels: &[u8]) -> Vec<u8> {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| {
            let alpha = pixel[3];
            if alpha == 0 {
                [0, 0, 0, 0]
            } else {
                [
                    ((pixel[0] as u16 * 255) / alpha as u16).min(255) as u8,
                    ((pixel[1] as u16 * 255) / alpha as u16).min(255) as u8,
                    ((pixel[2] as u16 * 255) / alpha as u16).min(255) as u8,
                    alpha,
                ]
            }
        })
        .collect()
}

fn encode_ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let header_size = 6 + images.len() * 16;
    let mut output =
        Vec::with_capacity(header_size + images.iter().map(|(_, png)| png.len()).sum::<usize>());
    output.extend_from_slice(&0u16.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&(images.len() as u16).to_le_bytes());

    let mut offset = header_size as u32;
    for (size, png) in images {
        output.push(if *size == 256 { 0 } else { *size as u8 });
        output.push(if *size == 256 { 0 } else { *size as u8 });
        output.extend_from_slice(&[0, 0]);
        output.extend_from_slice(&1u16.to_le_bytes());
        output.extend_from_slice(&32u16.to_le_bytes());
        output.extend_from_slice(&(png.len() as u32).to_le_bytes());
        output.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        output.extend_from_slice(png);
    }
    output
}
