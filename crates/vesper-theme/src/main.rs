//! vesper-theme: drive Vesper's matugen theming from a terminal.
//!
//!   vesper-theme regenerate [IMAGE]   run matugen on IMAGE (default: the current wallpaper) and render all templates
//!   vesper-theme show                 print the palette in use
//!   vesper-theme templates            list your templates
//!   vesper-theme files                print where everything lives

use std::path::PathBuf;

use vesper_config::{
    theme::{self, Palette, ThemeSettings},
    Config, WallpaperChoice,
};

fn current_wallpaper() -> Option<PathBuf> {
    let path = match WallpaperChoice::load() {
        Some(c) if !c.path.trim().is_empty() => c.path,
        _ => Config::load().wallpaper.path,
    };
    (!path.trim().is_empty()).then(|| theme::expand_home(&path))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("regenerate") | Some("apply") => {
            let image = args.get(1).map(|p| theme::expand_home(p)).or_else(current_wallpaper);
            let settings = ThemeSettings::load();
            match image {
                None => {
                    eprintln!("no wallpaper set: pick one with Mod+W or pass an image path");
                    1
                }
                Some(image) if !settings.enabled => {
                    eprintln!("theming is switched off (enabled = false in {})", ThemeSettings::file().display());
                    let _ = image;
                    1
                }
                Some(image) => match theme::regenerate(&image, &settings) {
                    Ok(()) => {
                        println!("generated colours from {} ({}, {}, contrast {:.2})", image.display(), settings.mode, settings.scheme, settings.contrast);
                        0
                    }
                    Err(err) => {
                        eprintln!("matugen failed: {err}");
                        1
                    }
                },
            }
        }
        Some("show") => {
            let p = Palette::load();
            let line = |name: &str, c: [u8; 4]| println!("{name:<28} #{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
            line("surface", p.surface);
            line("surface_container", p.surface_container);
            line("on_surface", p.on_surface);
            line("on_surface_variant", p.on_surface_variant);
            line("primary", p.primary);
            line("on_primary", p.on_primary);
            line("secondary", p.secondary);
            line("tertiary", p.tertiary);
            line("error", p.error);
            0
        }
        Some("templates") => {
            for t in theme::read_templates() {
                println!("{}: {} -> {}{}", t.name, t.input_path, t.output_path, if t.post_hook.is_empty() { String::new() } else { format!("  [hook: {}]", t.post_hook) });
            }
            0
        }
        Some("files") => {
            println!("settings:        {}", ThemeSettings::file().display());
            println!("matugen config:  {}", theme::matugen_config_file().display());
            println!("palette:         {}", theme::colors_file().display());
            0
        }
        _ => {
            print!("{}", include_str!("main.rs").lines().take_while(|l| l.starts_with("//!")).map(|l| format!("{}\n", l.trim_start_matches("//!").trim_start())).collect::<String>());
            2
        }
    };
    std::process::exit(code);
}
