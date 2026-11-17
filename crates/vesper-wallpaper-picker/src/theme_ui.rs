//! The Theme screen of the wallpaper changer: matugen settings and templates.

use std::path::PathBuf;

use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};
use vesper_config::theme::{self, alpha, with_alpha, Palette, Template, ThemeSettings, SCHEMES};
use vesper_gfx::{Canvas, Text};

/// "scheme-tonal-spot" -> "Tonal spot".
pub fn scheme_label(s: &str) -> String {
    let name = s.trim_start_matches("scheme-").replace('-', " ");
    let mut chars = name.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

const PREFER: [(&str, &str); 5] = [
    ("saturation", "Most saturated"),
    ("less-saturation", "Least saturated"),
    ("darkness", "Darkest"),
    ("lightness", "Lightest"),
    ("value", "Brightest"),
];
const MODES: [(&str, &str); 3] = [("dark", "Dark"), ("light", "Light"), ("smart", "Smart")];

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Working,
    Ok(String),
    Err(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Row {
    Enabled,
    Mode,
    Scheme,
    Contrast,
    Prefer,
    Template(usize),
    Add,
    Import,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Act {
    Focus(usize),
    SetMode(usize),
    Step(usize, i32),
    Slider(i32, i32),
    Remove(usize),
    Back,
    FormFocus(usize),
    FormSave,
    FormCancel,
}

struct Form {
    /// Index of the template being edited, or None when adding.
    editing: Option<usize>,
    /// Name, input path, output path, post hook.
    fields: [String; 4],
    focus: usize,
    error: String,
}

/// What the screen wants the picker to do after an event.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    None,
    Redraw,
    /// Leave the Theme screen.
    Back,
    /// Settings or templates changed: save them and run matugen again.
    Regenerate,
}

pub struct ThemeUi {
    pub settings: ThemeSettings,
    templates: Vec<Template>,
    focus: usize,
    scroll: usize,
    form: Option<Form>,
    pub status: Status,
    hits: Vec<([i32; 4], Act)>,
    /// Where `Import` looks for templates.
    import_from: PathBuf,
}

pub struct Fonts<'a> {
    pub title: &'a Text,
    pub body: &'a Text,
    pub small: &'a Text,
}

impl ThemeUi {
    pub fn new() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        Self {
            settings: ThemeSettings::load(),
            templates: theme::read_templates(),
            focus: 0,
            scroll: 0,
            form: None,
            status: Status::Idle,
            hits: Vec::new(),
            import_from: home.join(".config/matugen/config.toml"),
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut r = vec![Row::Enabled, Row::Mode, Row::Scheme, Row::Contrast, Row::Prefer];
        r.extend((0..self.templates.len()).map(Row::Template));
        r.push(Row::Add);
        r.push(Row::Import);
        r
    }

    /// Templates in your existing matugen config that Vesper doesn't have yet.
    fn importable(&self) -> Vec<Template> {
        theme::read_templates_from(&self.import_from).into_iter().filter(|t| !self.templates.iter().any(|m| m.name == t.name)).collect()
    }

    fn save_settings(&mut self) -> Outcome {
        if let Err(e) = self.settings.save() {
            self.status = Status::Err(format!("could not save the settings: {e}"));
            return Outcome::Redraw;
        }
        Outcome::Regenerate
    }

    fn save_templates(&mut self) -> Outcome {
        if let Err(e) = theme::write_templates(&self.templates) {
            self.status = Status::Err(format!("could not save the templates: {e}"));
            return Outcome::Redraw;
        }
        Outcome::Regenerate
    }

    /// Change the focused row's value by `dir` (-1 or +1).
    fn adjust(&mut self, row: Row, dir: i32) -> Outcome {
        match row {
            Row::Enabled => self.settings.enabled = !self.settings.enabled,
            Row::Mode => {
                let at = MODES.iter().position(|(k, _)| *k == self.settings.mode).unwrap_or(0) as i32;
                self.settings.mode = MODES[(at + dir).rem_euclid(MODES.len() as i32) as usize].0.to_owned();
            }
            Row::Scheme => {
                let at = SCHEMES.iter().position(|s| *s == self.settings.scheme).unwrap_or(0) as i32;
                self.settings.scheme = SCHEMES[(at + dir).rem_euclid(SCHEMES.len() as i32) as usize].to_owned();
            }
            Row::Contrast => self.settings.contrast = (((self.settings.contrast + dir as f32 * 0.25) * 4.0).round() / 4.0).clamp(-1.0, 1.0),
            Row::Prefer => {
                let at = PREFER.iter().position(|(k, _)| *k == self.settings.prefer).unwrap_or(0) as i32;
                self.settings.prefer = PREFER[(at + dir).rem_euclid(PREFER.len() as i32) as usize].0.to_owned();
            }
            _ => return Outcome::None,
        }
        self.save_settings()
    }

    fn activate(&mut self, row: Row) -> Outcome {
        match row {
            Row::Enabled => self.adjust(row, 1),
            Row::Template(i) => {
                let t = self.templates[i].clone();
                self.form = Some(Form { editing: Some(i), fields: [t.name, t.input_path, t.output_path, t.post_hook], focus: 1, error: String::new() });
                Outcome::Redraw
            }
            Row::Add => {
                self.form = Some(Form { editing: None, fields: Default::default(), focus: 0, error: String::new() });
                Outcome::Redraw
            }
            Row::Import => {
                let found = self.importable();
                if found.is_empty() {
                    self.status = Status::Err(format!("nothing new to import from {}", self.import_from.display()));
                    return Outcome::Redraw;
                }
                let n = found.len();
                self.templates.extend(found);
                self.status = Status::Ok(format!("imported {n} template{}", if n == 1 { "" } else { "s" }));
                self.save_templates()
            }
            _ => Outcome::None,
        }
    }

    pub fn key(&mut self, ev: &KeyEvent) -> Outcome {
        if self.form.is_some() {
            return self.form_key(ev);
        }
        let rows = self.rows();
        self.focus = self.focus.min(rows.len() - 1);
        let row = rows[self.focus];
        match ev.keysym {
            Keysym::Escape => Outcome::Back,
            Keysym::Up => {
                self.focus = self.focus.saturating_sub(1);
                Outcome::Redraw
            }
            Keysym::Down | Keysym::Tab => {
                self.focus = (self.focus + 1).min(rows.len() - 1);
                Outcome::Redraw
            }
            Keysym::Left => self.adjust(row, -1),
            Keysym::Right => self.adjust(row, 1),
            Keysym::Return | Keysym::KP_Enter | Keysym::space => self.activate(row),
            Keysym::Delete | Keysym::BackSpace => {
                if let Row::Template(i) = row {
                    self.remove(i)
                } else {
                    Outcome::None
                }
            }
            _ => Outcome::None,
        }
    }

    fn remove(&mut self, i: usize) -> Outcome {
        let gone = self.templates.remove(i);
        self.status = Status::Ok(format!("removed {}", gone.name));
        self.focus = self.focus.min(self.rows().len() - 1);
        self.save_templates()
    }

    fn form_key(&mut self, ev: &KeyEvent) -> Outcome {
        let Some(form) = self.form.as_mut() else { return Outcome::None };
        match ev.keysym {
            Keysym::Escape => {
                self.form = None;
                Outcome::Redraw
            }
            Keysym::Tab | Keysym::Down => {
                form.focus = (form.focus + 1) % 4;
                Outcome::Redraw
            }
            Keysym::Up | Keysym::ISO_Left_Tab => {
                form.focus = (form.focus + 3) % 4;
                Outcome::Redraw
            }
            Keysym::Return | Keysym::KP_Enter => {
                if form.focus < 3 {
                    form.focus += 1;
                    Outcome::Redraw
                } else {
                    self.save_form()
                }
            }
            Keysym::BackSpace => {
                form.fields[form.focus].pop();
                form.error.clear();
                Outcome::Redraw
            }
            _ => {
                if let Some(s) = &ev.utf8 {
                    let printable: String = s.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() {
                        form.fields[form.focus].push_str(&printable);
                        form.error.clear();
                        return Outcome::Redraw;
                    }
                }
                Outcome::None
            }
        }
    }

    fn save_form(&mut self) -> Outcome {
        let Some(form) = self.form.as_mut() else { return Outcome::None };
        let [name, input, output, hook] = form.fields.clone().map(|s| s.trim().to_owned());
        let editing = form.editing;
        let problem = if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            Some("Name: letters, digits, - and _ only")
        } else if name == theme::BUILTIN {
            Some("That name is used by the built-in template")
        } else if self.templates.iter().enumerate().any(|(i, t)| t.name == name && Some(i) != editing) {
            Some("A template with that name already exists")
        } else if input.is_empty() {
            Some("Input path is required")
        } else if !theme::expand_home(&input).is_file() {
            Some("Input file not found")
        } else if output.is_empty() {
            Some("Output path is required")
        } else {
            None
        };
        if let Some(p) = problem {
            form.error = p.to_owned();
            return Outcome::Redraw;
        }
        let t = Template { name, input_path: input, output_path: output, post_hook: hook };
        match editing {
            Some(i) => self.templates[i] = t,
            None => {
                self.templates.push(t);
                self.focus = self.rows().iter().position(|r| *r == Row::Template(self.templates.len() - 1)).unwrap_or(self.focus);
            }
        }
        self.form = None;
        self.status = Status::Ok("template saved".into());
        self.save_templates()
    }

    pub fn click(&mut self, x: i32, y: i32) -> Outcome {
        let hit = self.hits.iter().rev().find(|([rx, ry, rw, rh], _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh).map(|(r, a)| (*r, *a));
        let Some((rect, act)) = hit else { return if self.form.is_some() { Outcome::None } else { Outcome::Back } };
        match act {
            Act::Back => Outcome::Back,
            Act::FormCancel => {
                self.form = None;
                Outcome::Redraw
            }
            Act::FormSave => self.save_form(),
            Act::FormFocus(i) => {
                if let Some(f) = self.form.as_mut() {
                    f.focus = i;
                }
                Outcome::Redraw
            }
            Act::Focus(usize::MAX) => Outcome::None, // inside the panel, on nothing
            Act::Focus(i) => {
                self.focus = i;
                let row = self.rows()[i];
                match row {
                    Row::Enabled | Row::Template(_) | Row::Add | Row::Import => self.activate(row),
                    _ => Outcome::Redraw,
                }
            }
            Act::Step(i, dir) => {
                self.focus = i;
                let row = self.rows()[i];
                self.adjust(row, dir)
            }
            Act::SetMode(m) => {
                self.focus = 1;
                self.settings.mode = MODES[m].0.to_owned();
                self.save_settings()
            }
            Act::Slider(x0, x1) => {
                self.focus = 3;
                let t = ((x - x0) as f32 / (x1 - x0).max(1) as f32).clamp(0.0, 1.0);
                self.settings.contrast = (((t * 2.0 - 1.0) * 4.0).round() / 4.0).clamp(-1.0, 1.0);
                let _ = rect;
                self.save_settings()
            }
            Act::Remove(i) => self.remove(i),
        }
    }

    pub fn generation_finished(&mut self, result: Result<String, String>) {
        self.status = match result {
            Ok(what) => Status::Ok(what),
            Err(e) => Status::Err(e),
        };
    }

    // ------------------------------------------------------------------ drawing

    pub fn draw(&mut self, c: &mut Canvas, w: i32, h: i32, f: &Fonts, pal: &Palette) {
        self.hits.clear();
        let (pw, ph) = ((w - 60).min(800), (h - 100).min(660));
        let (px, py) = ((w - pw) / 2, (h - ph) / 2);
        c.rounded_rect(px, py, pw, ph, 18.0, alpha(pal.surface_container, 0xf4));
        c.rounded_outline(px, py, pw, ph, 18.0, 1.5, with_alpha(pal.outline, 0.45));
        self.hits.push(([px, py, pw, ph], Act::Focus(usize::MAX))); // swallows clicks inside the panel (handled below)

        // header: title, palette swatches, back hint
        f.title.draw_spaced(c, px + 30, py + 38, "THEME", pal.on_surface, 6);
        let back_x = px + 30 + f.title.width_spaced("THEME", 6) + 24;
        c.rounded_rect(back_x, py + 24, 76, 28, 14.0, with_alpha(pal.on_surface, 0.10));
        f.small.draw_centered(c, back_x + 38, py + 38, "‹  Back", pal.on_surface);
        self.hits.push(([back_x, py + 24, 76, 28], Act::Back));
        let swatches = [pal.primary, pal.secondary, pal.tertiary, pal.primary_container, pal.surface_container_highest, pal.on_surface];
        let mut sx = px + pw - 30 - swatches.len() as i32 * 26;
        for s in swatches {
            c.circle(sx + 10, py + 38, 10, s);
            c.rounded_outline(sx, py + 28, 20, 20, 10.0, 1.0, with_alpha(pal.on_surface, 0.25));
            sx += 26;
        }

        let rows = self.rows();
        self.focus = self.focus.min(rows.len() - 1);
        let row_h = 46;
        let top = py + 76;
        let bottom = py + ph - 64;
        let capacity = ((bottom - top) / row_h).max(3) as usize;
        if self.focus < self.scroll {
            self.scroll = self.focus;
        } else if self.focus >= self.scroll + capacity {
            self.scroll = self.focus + 1 - capacity;
        }
        self.scroll = self.scroll.min(rows.len().saturating_sub(capacity));

        let importable = self.importable().len();
        for (n, idx) in (self.scroll..rows.len().min(self.scroll + capacity)).enumerate().map(|(n, k)| (n, k)) {
            let row = rows[idx];
            let y = top + n as i32 * row_h;
            let focused = idx == self.focus && self.form.is_none();
            if focused {
                c.rounded_rect(px + 14, y, pw - 28, row_h - 6, 10.0, with_alpha(pal.primary, 0.16));
                c.rounded_outline(px + 14, y, pw - 28, row_h - 6, 10.0, 1.5, with_alpha(pal.primary, 0.55));
            }
            self.hits.push(([px + 14, y, pw - 28, row_h - 6], Act::Focus(idx)));
            let mid = y + (row_h - 6) / 2;
            let (lx, rx) = (px + 34, px + pw - 34);
            match row {
                Row::Enabled => {
                    f.body.draw(c, lx, mid, "Colours from the wallpaper (matugen)", pal.on_surface);
                    let on = self.settings.enabled;
                    c.pill(rx - 48, mid - 12, 48, 24, if on { pal.primary } else { pal.outline_variant });
                    c.circle(if on { rx - 12 } else { rx - 36 }, mid, 9, if on { pal.on_primary } else { pal.on_surface_variant });
                }
                Row::Mode => {
                    f.body.draw(c, lx, mid, "Mode", pal.on_surface);
                    let mut bx = rx - MODES.len() as i32 * 84;
                    for (m, (key, label)) in MODES.iter().enumerate() {
                        let active = self.settings.mode == *key;
                        c.rounded_rect(bx, mid - 15, 78, 30, 15.0, if active { pal.primary } else { pal.surface_container_high });
                        f.small.draw_centered(c, bx + 39, mid, label, if active { pal.on_primary } else { pal.on_surface_variant });
                        self.hits.push(([bx, mid - 15, 78, 30], Act::SetMode(m)));
                        bx += 84;
                    }
                }
                Row::Scheme | Row::Prefer => {
                    let (name, value) = if row == Row::Scheme {
                        ("Colour scheme", scheme_label(&self.settings.scheme))
                    } else {
                        ("Source colour", PREFER.iter().find(|(k, _)| *k == self.settings.prefer).map_or("Most saturated", |(_, l)| l).to_owned())
                    };
                    f.body.draw(c, lx, mid, name, pal.on_surface);
                    let vw = 190;
                    f.small.draw_centered(c, rx - 20 - vw / 2, mid, &value, pal.primary);
                    f.body.draw_centered(c, rx - vw - 40, mid, "‹", pal.on_surface_variant);
                    f.body.draw_centered(c, rx - 10, mid, "›", pal.on_surface_variant);
                    self.hits.push(([rx - vw - 56, y, 32, row_h - 6], Act::Step(idx, -1)));
                    self.hits.push(([rx - 26, y, 32, row_h - 6], Act::Step(idx, 1)));
                }
                Row::Contrast => {
                    f.body.draw(c, lx, mid, "Contrast", pal.on_surface);
                    let (sx0, sx1) = (rx - 260, rx - 70);
                    c.pill(sx0, mid - 2, sx1 - sx0, 4, pal.outline_variant);
                    let t = (self.settings.contrast + 1.0) / 2.0;
                    let kx = sx0 + ((sx1 - sx0) as f32 * t) as i32;
                    c.pill(sx0, mid - 2, kx - sx0, 4, pal.primary);
                    c.circle(kx, mid, 9, pal.primary);
                    f.small.draw(c, rx - 56, mid, &format!("{:+.2}", self.settings.contrast), pal.on_surface_variant);
                    self.hits.push(([sx0 - 8, mid - 14, sx1 - sx0 + 16, 28], Act::Slider(sx0, sx1)));
                }
                Row::Template(i) => {
                    let t = &self.templates[i];
                    f.body.draw(c, lx, mid, &t.name, pal.primary);
                    let name_w = f.body.width(&t.name) + 18;
                    let detail = format!("{} → {}{}", t.input_path.rsplit('/').next().unwrap_or(&t.input_path), t.output_path, if t.post_hook.is_empty() { "" } else { "   ⚡ hook" });
                    let avail = (rx - 80) - (lx + name_w);
                    f.small.draw(c, lx + name_w, mid, &f.small.fit(&detail, avail), pal.on_surface_variant);
                    f.small.draw(c, rx - 60, mid, "Remove", with_alpha(pal.error, 0.9));
                    self.hits.push(([rx - 66, y, 70, row_h - 6], Act::Remove(i)));
                }
                Row::Add => f.body.draw(c, lx, mid, "+  Add a template", pal.primary),
                Row::Import => {
                    let text = if importable > 0 {
                        format!("Import {importable} template{} from ~/.config/matugen", if importable == 1 { "" } else { "s" })
                    } else {
                        "Import from ~/.config/matugen (nothing new)".to_owned()
                    };
                    f.body.draw(c, lx, mid, &text, if importable > 0 { pal.on_surface } else { pal.outline });
                }
            }
        }
        if rows.len() > capacity {
            let track = capacity as i32 * row_h;
            let th = (track * capacity as i32 / rows.len() as i32).max(24);
            let ty = top + (track - th) * self.scroll as i32 / (rows.len() - capacity).max(1) as i32;
            c.rounded_rect(px + pw - 12, ty, 4, th, 2.0, with_alpha(pal.outline, 0.7));
        }

        // footer: status and key hints
        let (text, color) = match &self.status {
            Status::Idle => (String::new(), pal.on_surface_variant),
            Status::Working => ("Generating colours…".to_owned(), pal.on_surface_variant),
            Status::Ok(s) => (s.clone(), pal.primary),
            Status::Err(s) => (s.clone(), pal.error),
        };
        f.small.draw(c, px + 30, py + ph - 38, &f.small.fit(&text, pw - 60), color);
        let hint = "↑↓ move   ←→ change   Enter select   Del remove   Esc back";
        f.small.draw(c, px + pw - 30 - f.small.width(hint), py + ph - 16, hint, with_alpha(pal.on_surface_variant, 0.8));

        if let Some(form) = &self.form {
            self.draw_form(c, w, h, f, pal, form_snapshot(form));
        }
    }

    fn draw_form(&mut self, c: &mut Canvas, w: i32, h: i32, f: &Fonts, pal: &Palette, form: FormView) {
        // dim what is behind, then the card
        c.rounded_rect(0, 0, w, h, 0.0, with_alpha(pal.scrim, 0.55));
        let (fw, fh) = ((w - 80).min(680), 430);
        let (fx, fy) = ((w - fw) / 2, (h - fh) / 2);
        c.rounded_rect(fx, fy, fw, fh, 16.0, alpha(pal.surface_container_high, 0xfa));
        c.rounded_outline(fx, fy, fw, fh, 16.0, 1.5, with_alpha(pal.primary, 0.6));
        self.hits.clear();
        self.hits.push(([fx, fy, fw, fh], Act::FormFocus(form.focus)));
        f.title.draw_spaced(c, fx + 28, fy + 36, if form.editing { "EDIT TEMPLATE" } else { "ADD TEMPLATE" }, pal.on_surface, 5);
        let labels = ["Name", "Input (a matugen template file)", "Output (where to write it)", "Post hook (optional command)"];
        for (i, label) in labels.iter().enumerate() {
            let y = fy + 70 + i as i32 * 66;
            f.small.draw(c, fx + 28, y, label, pal.on_surface_variant);
            let (bx, by, bw, bh) = (fx + 28, y + 12, fw - 56, 34);
            c.rounded_rect(bx, by, bw, bh, 8.0, pal.surface_container_highest);
            let focused = form.focus == i;
            c.rounded_outline(bx, by, bw, bh, 8.0, if focused { 2.0 } else { 1.0 }, if focused { pal.primary } else { with_alpha(pal.outline, 0.5) });
            // show the end of long text, with a caret on the focused field
            let text = &form.fields[i];
            let mut shown: &str = text;
            while f.body.width(shown) > bw - 28 && !shown.is_empty() {
                shown = &shown[shown.char_indices().nth(1).map_or(shown.len(), |(k, _)| k)..];
            }
            f.body.draw(c, bx + 12, by + bh / 2, shown, pal.on_surface);
            if focused {
                let cx = bx + 12 + f.body.width(shown) + 2;
                c.rounded_rect(cx, by + 7, 2, bh - 14, 1.0, pal.primary);
            }
            self.hits.push(([bx, by, bw, bh], Act::FormFocus(i)));
        }
        if !form.error.is_empty() {
            f.small.draw(c, fx + 28, fy + fh - 78, &form.error, pal.error);
        } else {
            f.small.draw(c, fx + 28, fy + fh - 78, "Paths can start with ~/   •   Tab moves between fields", pal.on_surface_variant);
        }
        let (bw, bh) = (110, 38);
        let by = fy + fh - 28 - bh;
        c.rounded_rect(fx + fw - 28 - bw, by, bw, bh, 19.0, pal.primary);
        f.small.draw_centered(c, fx + fw - 28 - bw / 2, by + bh / 2, "Save", pal.on_primary);
        self.hits.push(([fx + fw - 28 - bw, by, bw, bh], Act::FormSave));
        let cx = fx + fw - 28 - bw - 14 - bw;
        c.rounded_rect(cx, by, bw, bh, 19.0, pal.surface_container_highest);
        f.small.draw_centered(c, cx + bw / 2, by + bh / 2, "Cancel", pal.on_surface);
        self.hits.push(([cx, by, bw, bh], Act::FormCancel));
    }
}

struct FormView {
    editing: bool,
    fields: [String; 4],
    focus: usize,
    error: String,
}

fn form_snapshot(f: &Form) -> FormView {
    FormView { editing: f.editing.is_some(), fields: f.fields.clone(), focus: f.focus, error: f.error.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests point XDG_* at throwaway folders, which is process-wide, so they take turns.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn key(keysym: Keysym, utf8: Option<&str>) -> KeyEvent {
        KeyEvent { time: 0, raw_code: 0, keysym, utf8: utf8.map(str::to_owned) }
    }

    fn type_text(ui: &mut ThemeUi, text: &str) {
        for ch in text.chars() {
            ui.key(&key(Keysym::from_char(ch), Some(&ch.to_string())));
        }
    }

    /// Add, edit and remove a template with key presses only, against throwaway config folders.
    #[test]
    fn template_add_edit_remove_flow() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("vesper-theme-ui-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("in")).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", dir.join("cfg"));
        std::env::set_var("XDG_STATE_HOME", dir.join("state"));
        let input = dir.join("in/kitty.conf");
        std::fs::write(&input, "x").unwrap();

        let mut ui = ThemeUi::new();
        // rows: Enabled, Mode, Scheme, Contrast, Prefer, then Add
        for _ in 0..5 {
            ui.key(&key(Keysym::Down, None));
        }
        assert_eq!(ui.key(&key(Keysym::Return, None)), Outcome::Redraw); // opens the form
        type_text(&mut ui, "kitty");
        ui.key(&key(Keysym::Tab, None));
        type_text(&mut ui, &input.to_string_lossy());
        ui.key(&key(Keysym::Tab, None));
        type_text(&mut ui, "~/.config/kitty/colors.conf");
        ui.key(&key(Keysym::Tab, None));
        type_text(&mut ui, "kitty @ set-colors -a");
        assert_eq!(ui.key(&key(Keysym::Return, None)), Outcome::Regenerate);

        let saved = theme::read_templates();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "kitty");
        assert_eq!(saved[0].post_hook, "kitty @ set-colors -a");
        let config = std::fs::read_to_string(theme::matugen_config_file()).unwrap();
        assert!(config.contains("[templates.vesper]"), "built-in template always present");
        assert!(theme::builtin_template_file().is_file());

        // a missing input file is refused
        ui.key(&key(Keysym::Down, None));
        assert_eq!(ui.key(&key(Keysym::Return, None)), Outcome::Redraw);
        type_text(&mut ui, "oops");
        ui.key(&key(Keysym::Tab, None));
        type_text(&mut ui, "/no/such/file");
        ui.key(&key(Keysym::Tab, None));
        ui.key(&key(Keysym::Tab, None));
        assert_eq!(ui.key(&key(Keysym::Return, None)), Outcome::Redraw);
        assert_eq!(theme::read_templates().len(), 1, "invalid template was not saved");
        ui.key(&key(Keysym::Escape, None)); // cancel the form

        // remove the kitty template (focus it first)
        for _ in 0..8 {
            ui.key(&key(Keysym::Up, None));
        }
        for _ in 0..5 {
            ui.key(&key(Keysym::Down, None));
        }
        assert_eq!(ui.key(&key(Keysym::Delete, None)), Outcome::Regenerate);
        assert!(theme::read_templates().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_change_saves_and_asks_for_regeneration() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("vesper-theme-ui2-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", dir.join("cfg"));
        std::env::set_var("XDG_STATE_HOME", dir.join("state"));
        let mut ui = ThemeUi::new();
        ui.key(&key(Keysym::Down, None)); // mode
        assert_eq!(ui.key(&key(Keysym::Right, None)), Outcome::Regenerate);
        assert_eq!(ThemeSettings::load().mode, "light");
        ui.key(&key(Keysym::Down, None)); // scheme
        ui.key(&key(Keysym::Right, None));
        assert_eq!(ThemeSettings::load().scheme, "scheme-vibrant");
        ui.key(&key(Keysym::Down, None)); // contrast
        ui.key(&key(Keysym::Right, None));
        ui.key(&key(Keysym::Right, None));
        assert!((ThemeSettings::load().contrast - 0.5).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scheme_names_are_readable() {
        assert_eq!(scheme_label("scheme-tonal-spot"), "Tonal spot");
        assert_eq!(scheme_label("scheme-fruit-salad"), "Fruit salad");
        assert_eq!(scheme_label("scheme-monochrome"), "Monochrome");
    }

    #[test]
    fn every_scheme_has_a_label() {
        for s in SCHEMES {
            assert!(!scheme_label(s).is_empty());
        }
    }
}
