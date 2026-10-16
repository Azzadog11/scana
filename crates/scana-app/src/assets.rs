//! The logo and the menu icons: PNGs drawn in VectorCraft (sources in `design/`), built into the
//! program. Icons are white on transparent, so they take any colour (tinted when drawn).

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use std::collections::HashMap;

macro_rules! icon {
    ($name:literal) => {
        ($name, include_bytes!(concat!("../assets/icons/icon-", $name, ".png")).as_slice())
    };
}

const ICONS: [(&str, &[u8]); 17] = [
    icon!("home"),
    icon!("health"),
    icon!("codes"),
    icon!("diagnose"),
    icon!("live"),
    icon!("graphs"),
    icon!("readiness"),
    icon!("tests"),
    icon!("info"),
    icon!("modules"),
    icon!("explorer"),
    icon!("monitor"),
    icon!("sessions"),
    icon!("library"),
    icon!("glossary"),
    icon!("settings"),
    icon!("search"),
];

const LOGO: &[u8] = include_bytes!("../assets/logo-64.png");

/// Decodes an 8-bit RGBA PNG.
fn decode(bytes: &[u8]) -> Option<ColorImage> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let size = [info.width as usize, info.height as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, &buf[..info.buffer_size()]))
}

pub struct Assets {
    pub logo: Option<TextureHandle>,
    icons: HashMap<&'static str, TextureHandle>,
}

impl Assets {
    pub fn load(ctx: &Context) -> Self {
        let tex =
            |name: &str, bytes: &[u8]| decode(bytes).map(|img| ctx.load_texture(name, img, TextureOptions::LINEAR));
        Assets {
            logo: tex("scana-logo", LOGO),
            icons: ICONS.iter().filter_map(|(n, b)| tex(n, b).map(|t| (*n, t))).collect(),
        }
    }

    pub fn icon(&self, name: &str) -> Option<&TextureHandle> {
        self.icons.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_picture_decodes() {
        assert!(decode(LOGO).is_some_and(|i| i.size == [64, 64]));
        for (name, bytes) in ICONS {
            assert!(decode(bytes).is_some_and(|i| i.size == [48, 48]), "icon {name}");
        }
    }
}
