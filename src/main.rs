//! Mappa Mundi: an offline map of where your photos were taken.

mod app;
mod basemap;
mod cluster;
mod geo;
mod imageio;
mod library;
mod map;
mod media;
mod mvt;
mod pmtiles;
mod quicktime;
mod streets;
mod texcache;
mod theme;
mod tiles;
mod timeline;
mod video;
mod viewer;
mod work;
mod xmp;

use std::path::PathBuf;

const USAGE: &str = "\
Usage: mappa-mundi [FOLDER] [--tiles FILE.mbtiles|TILE_DIR]

  FOLDER        Photo library to scan (remembered for next time)
  --tiles PATH  Optional offline raster tiles for street-level detail:
                an .mbtiles file or a folder laid out as z/x/y.png";

fn main() -> eframe::Result {
    let mut opts = app::Options { root: None, tiles: None };
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return Ok(());
            }
            Some("--tiles") => match args.next() {
                Some(p) => opts.tiles = Some(absolute(p.into())),
                None => {
                    eprintln!("--tiles needs a path\n\n{USAGE}");
                    std::process::exit(2);
                }
            },
            Some(a) if a.starts_with('-') => {
                eprintln!("unknown option {a}\n\n{USAGE}");
                std::process::exit(2);
            }
            _ => opts.root = Some(absolute(arg.into())),
        }
    }

    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mappa Mundi")
            .with_app_id("mappa-mundi")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native("Mappa Mundi", native, Box::new(|cc| Ok(Box::new(app::MappaMundiApp::new(cc, opts)))))
}

fn absolute(p: PathBuf) -> PathBuf {
    std::path::absolute(&p).unwrap_or(p)
}
