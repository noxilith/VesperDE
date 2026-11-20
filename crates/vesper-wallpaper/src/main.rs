//! vesper-wallpaper: shows an image (or a plain colour) on the background layer.
//!
//! The image comes from the wallpaper changer's saved choice (Mod+W) if there is one, else from `[wallpaper]` in the
//! config. The saved choice is watched, so picking a new wallpaper takes effect almost at once. Decoding and scaling
//! happen on a worker thread, so the old wallpaper stays up (and nothing freezes) until the new one is ready.

use std::{borrow::Cow, num::NonZeroU32, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{anyhow, Context, Result};
use image::{imageops::FilterType, RgbaImage};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel,
            timer::{TimeoutAction, Timer},
            EventLoop,
        },
        calloop_wayland_source::WaylandSource,
        client::{
            globals::registry_queue_init,
            protocol::{wl_output, wl_shm, wl_surface},
            Connection, QueueHandle,
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use vesper_config::WallpaperChoice;

/// What a worker renders from.
enum Source {
    /// Read and decode this file first.
    Path(String),
    /// Re-render an image we already have (after a resize).
    Image(Arc<RgbaImage>),
}

/// A finished render, sent back to the main loop.
struct Done {
    head: usize,
    generation: u64,
    size: (u32, u32),
    /// BGRX pixels, `size.0 * size.1 * 4` bytes.
    pixels: Vec<u8>,
}

/// One monitor's wallpaper surface.
struct Head {
    id: usize,
    output: wl_output::WlOutput,
    layer: LayerSurface,
    pool: SlotPool,
    size: (u32, u32),
    /// Bumped for every render request; results from older requests are dropped.
    generation: u64,
}

struct Wallpaper {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    compositor: CompositorState,
    layer_shell: LayerShell,
    qh: QueueHandle<Wallpaper>,
    /// Every monitor gets the wallpaper.
    heads: Vec<Head>,
    next_head: usize,
    /// The file on screen. We do not keep the decoded picture (it is 16 MB or more); a resize decodes it again.
    current_path: Option<String>,
    tx: channel::Sender<Done>,
    mode: String,
    color: [u8; 3],
    config_path: String,
    config_mode: String,
    /// Modification time of the saved choice when we last read it.
    choice_stamp: Option<std::time::SystemTime>,
    exit: bool,
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).unwrap_or_else(|| p.into()),
        None => p.into(),
    }
}

fn parse_color(s: &str) -> [u8; 3] {
    let h = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0x14141a);
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// The path and mode to show: the changer's saved choice if any, else the config.
fn effective(config: &vesper_config::Wallpaper) -> (String, String) {
    match WallpaperChoice::load() {
        Some(c) if !c.path.trim().is_empty() => (c.path, if c.mode.is_empty() { config.mode.clone() } else { c.mode }),
        _ => (config.path.clone(), config.mode.clone()),
    }
}

fn choice_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(WallpaperChoice::file()).and_then(|m| m.modified()).ok()
}

/// Re-run matugen for `path` in the background, so new colours reach the bar, launcher and your templates.
fn regenerate_colours(path: &str) {
    if path.trim().is_empty() || !expand_home(path).is_file() {
        return;
    }
    let image = expand_home(path);
    std::thread::spawn(move || {
        let settings = vesper_config::theme::ThemeSettings::load();
        if !settings.enabled {
            return;
        }
        match vesper_config::theme::regenerate(&image, &settings) {
            Ok(()) => tracing::info!("colours generated from {}", image.display()),
            Err(err) => tracing::warn!("matugen: {err}"),
        }
    });
}

/// Are the generated colours missing, or older than the wallpaper or the theme settings?
fn colours_stale(path: &str) -> bool {
    use vesper_config::theme as t;
    let Some(colours) = t::Palette::stamp() else { return true };
    let newest = [
        std::fs::metadata(expand_home(path)).and_then(|m| m.modified()).ok(),
        choice_stamp(),
        t::ThemeSettings::stamp(),
        std::fs::metadata(t::matugen_config_file()).and_then(|m| m.modified()).ok(),
    ]
    .into_iter()
    .flatten()
    .max();
    newest.is_some_and(|n| n > colours)
}

fn load_image(path: &str) -> Option<RgbaImage> {
    if path.trim().is_empty() {
        return None;
    }
    let path = expand_home(path);
    match image::open(&path).with_context(|| format!("loading {}", path.display())) {
        Ok(img) => {
            let img = img.to_rgba8();
            tracing::info!("wallpaper {} ({}x{})", path.display(), img.width(), img.height());
            Some(img)
        }
        Err(err) => {
            tracing::warn!("{err:#}");
            None
        }
    }
}

/// Scale `img` for `mode`. Borrowed when no scaling is needed.
fn scale<'a>(img: &'a RgbaImage, w: u32, h: u32, mode: &str) -> Cow<'a, RgbaImage> {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let (tw, th) = (w as f32, h as f32);
    // Triangle is plenty for wallpapers and several times quicker than Lanczos on a 4K image.
    let resize = |nw: f32, nh: f32| {
        Cow::Owned(image::imageops::resize(img, nw.round().max(1.0) as u32, nh.round().max(1.0) as u32, FilterType::Triangle))
    };
    match mode {
        "stretch" => resize(tw, th),
        "center" => Cow::Borrowed(img),
        "fit" => {
            let s = (tw / iw).min(th / ih);
            resize(iw * s, ih * s)
        }
        _ => {
            // fill: cover the whole screen, crop the overflow
            let s = (tw / iw).max(th / ih);
            resize(iw * s, ih * s)
        }
    }
}

/// A `w` x `h` BGRX picture: the background colour with `img` scaled and centred on it.
fn render(img: Option<&RgbaImage>, w: u32, h: u32, mode: &str, bg: [u8; 3]) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    for px in out.chunks_exact_mut(4) {
        px.copy_from_slice(&[bg[2], bg[1], bg[0], 0xff]);
    }
    let Some(img) = img else { return out };
    let scaled = scale(img, w, h, mode);
    let (sw, sh) = (scaled.width() as i64, scaled.height() as i64);
    let (ox, oy) = ((w as i64 - sw) / 2, (h as i64 - sh) / 2);
    let raw = scaled.as_raw();
    for y in 0..sh {
        let dy = y + oy;
        if dy < 0 || dy >= h as i64 {
            continue;
        }
        // the part of this row that lands on screen
        let (x0, x1) = ((-ox).max(0), (w as i64 - ox).min(sw));
        if x0 >= x1 {
            continue;
        }
        let src = &raw[((y * sw + x0) * 4) as usize..((y * sw + x1) * 4) as usize];
        let dst_start = ((dy * w as i64 + (x0 + ox)) * 4) as usize;
        let dst = &mut out[dst_start..dst_start + src.len()];
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            let a = s[3] as u32;
            if a == 255 {
                d.copy_from_slice(&[s[2], s[1], s[0], 0xff]);
            } else if a > 0 {
                // blend over the background colour (wallpapers are normally opaque, so this is rare)
                d[0] = ((s[2] as u32 * a + d[0] as u32 * (255 - a)) / 255) as u8;
                d[1] = ((s[1] as u32 * a + d[1] as u32 * (255 - a)) / 255) as u8;
                d[2] = ((s[0] as u32 * a + d[2] as u32 * (255 - a)) / 255) as u8;
            }
        }
    }
    out
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = vesper_config::Config::load().wallpaper;
    // With no image anywhere we just paint the colour, and stay running so a later choice shows up.
    let (path, mode) = effective(&config);
    if colours_stale(&path) {
        regenerate_colours(&path);
    }

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell is not available: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;

    let (tx, rx) = channel::channel::<Done>();
    let mut app = Wallpaper {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        compositor,
        layer_shell,
        qh: qh.clone(),
        heads: Vec::new(),
        next_head: 0,
        current_path: Some(path),
        tx,
        mode,
        color: parse_color(&config.color),
        config_path: config.path.clone(),
        config_mode: config.mode.clone(),
        choice_stamp: choice_stamp(),
        exit: false,
    };

    let mut event_loop: EventLoop<Wallpaper> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn, queue).insert(handle.clone()).map_err(|e| anyhow!("{e}"))?;
    handle
        .insert_source(rx, |event, _, app| {
            if let channel::Event::Msg(done) = event {
                app.finished(done);
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    // Looking at one file's timestamp is nearly free, so check often: a new choice shows up within a tenth of a second.
    handle
        .insert_source(Timer::from_duration(Duration::from_millis(200)), |_, _, app| {
            app.check_choice();
            TimeoutAction::ToDuration(Duration::from_millis(200))
        })
        .map_err(|e| anyhow!("{e}"))?;
    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }
    Ok(())
}

impl Wallpaper {
    /// Start rendering on a worker thread; the result comes back through `finished`.
    fn start_render(&mut self, head: usize, source: Source) {
        let Some(h) = self.heads.iter_mut().find(|h| h.id == head) else { return };
        let (w, h_px) = h.size;
        if w == 0 || h_px == 0 {
            return;
        }
        h.generation += 1;
        let h = h_px;
        let generation = self.heads.iter().find(|x| x.id == head).map_or(0, |x| x.generation);
        if let Source::Path(p) = &source {
            self.current_path = Some(p.clone());
        }
        let (mode, color, tx) = (self.mode.clone(), self.color, self.tx.clone());
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let image = match source {
                Source::Path(p) => load_image(&p).map(Arc::new),
                Source::Image(img) => Some(img),
            };
            let pixels = render(image.as_deref(), w, h, &mode, color);
            tracing::info!("wallpaper ready in {:?}", started.elapsed());
            let _ = tx.send(Done { head, generation, size: (w, h), pixels });
        });
    }

    /// A render finished: show it, unless something newer has been asked for since.
    fn finished(&mut self, done: Done) {
        let Some(h) = self.heads.iter().find(|h| h.id == done.head) else { return };
        if done.generation != h.generation || done.size != h.size {
            return;
        }
        self.present(done.head, &done.pixels);
    }

    fn present(&mut self, head: usize, pixels: &[u8]) {
        let Some(head) = self.heads.iter_mut().find(|h| h.id == head) else { return };
        let (w, h) = head.size;
        let Ok((buffer, canvas)) = head.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Xrgb8888) else {
            tracing::warn!("could not allocate a {w}x{h} buffer");
            return;
        };
        // the slot can be slightly longer than the picture (alignment), so copy what we have, never skip
        let n = canvas.len().min(pixels.len());
        canvas[..n].copy_from_slice(&pixels[..n]);
        head.layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(head.layer.wl_surface()).is_ok() {
            head.layer.commit();
        }
    }

    /// Has the changer saved a new choice? If so, load and show it.
    fn check_choice(&mut self) {
        let stamp = choice_stamp();
        if stamp == self.choice_stamp {
            return;
        }
        self.choice_stamp = stamp;
        let cfg = vesper_config::Wallpaper { path: self.config_path.clone(), mode: self.config_mode.clone(), ..Default::default() };
        let (path, mode) = effective(&cfg);
        tracing::info!("wallpaper changed to {path:?} ({mode})");
        self.mode = mode;
        regenerate_colours(&path);
        let ids: Vec<usize> = self.heads.iter().map(|h| h.id).collect();
        for id in ids {
            self.start_render(id, Source::Path(path.clone()));
        }
    }
}

impl LayerShellHandler for Wallpaper {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        // the monitor went away: drop its surface; the last one closing ends the program
        self.heads.retain(|h| h.layer.wl_surface() != layer.wl_surface());
        if self.heads.is_empty() {
            self.exit = true;
        }
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        let Some(head) = self.heads.iter_mut().find(|h| h.layer.wl_surface() == layer.wl_surface()) else { return };
        let new = match (NonZeroU32::new(c.new_size.0), NonZeroU32::new(c.new_size.1)) {
            (Some(w), Some(h)) => (w.get(), h.get()),
            _ => head.size,
        };
        if new == head.size && head.generation > 0 {
            return;
        }
        head.size = new;
        let id = head.id;
        match self.current_path.clone() {
            Some(path) => self.start_render(id, Source::Path(path)), // first configure or a resize: decode for this monitor
            None => self.start_render(id, Source::Image(Arc::new(RgbaImage::new(1, 1)))), // no image: just the colour
        }
    }
}

impl CompositorHandler for Wallpaper {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Wallpaper {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if self.heads.iter().any(|h| h.output == output) {
            return;
        }
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Background, Some("vesper-wallpaper"), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        let Ok(pool) = SlotPool::new(1920 * 1080 * 4, &self.shm) else { return };
        self.heads.push(Head { id: self.next_head, output, layer, pool, size: (0, 0), generation: 0 });
        self.next_head += 1;
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.heads.retain(|h| h.output != output);
    }
}

impl ShmHandler for Wallpaper {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Wallpaper);
delegate_output!(Wallpaper);
delegate_shm!(Wallpaper);
delegate_layer!(Wallpaper);
delegate_registry!(Wallpaper);

impl ProvidesRegistryState for Wallpaper {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba(rgba))
    }

    #[test]
    fn fill_covers_the_screen_with_the_image() {
        let out = render(Some(&solid(40, 20, [200, 100, 50, 255])), 16, 16, "fill", [1, 2, 3]);
        assert_eq!(out.len(), 16 * 16 * 4);
        // BGRX: every pixel is the image colour, none is background
        assert!(out.chunks_exact(4).all(|p| p[..3] == [50, 100, 200]));
    }

    #[test]
    fn fit_letterboxes_with_the_background() {
        let out = render(Some(&solid(40, 20, [200, 100, 50, 255])), 16, 16, "fit", [1, 2, 3]);
        let px = |x: usize, y: usize| &out[(y * 16 + x) * 4..(y * 16 + x) * 4 + 3];
        assert_eq!(px(8, 8), [50, 100, 200], "image in the middle");
        assert_eq!(px(8, 0), [3, 2, 1], "background above the image");
    }

    #[test]
    fn center_crops_a_big_image_and_blends_transparency() {
        let out = render(Some(&solid(100, 100, [255, 255, 255, 128])), 10, 10, "center", [0, 0, 0]);
        let p = &out[..4];
        assert!((p[0] as i32 - 128).abs() <= 2, "half-transparent white over black is mid gray, got {}", p[0]);
    }

    #[test]
    fn no_image_is_just_the_colour() {
        let out = render(None, 4, 4, "fill", [9, 8, 7]);
        assert!(out.chunks_exact(4).all(|p| p == [7, 8, 9, 255]));
    }
}
