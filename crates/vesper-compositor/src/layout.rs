//! Scrollable-tiling layout: each workspace is an infinite horizontal strip of columns,
//! the screen is a viewport onto it. Workspaces are stacked vertically (switching between
//! them is up/down), like niri.

use smithay::{
    desktop::Window,
    wayland::seat::WaylandFocus,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle},
};

/// Where a strip of columns narrower than the screen sits.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub enum Align {
    #[default]
    Left,
    Right,
}

#[derive(Default)]
pub struct Workspace {
    /// Placement of the columns when they do not fill the screen (set by dragging a window sideways).
    pub align: Align,
    pub columns: Vec<Window>,
    /// Width of each column as a fraction of the usable width; 0.0 = use the configured default.
    pub ratios: Vec<f64>,
    /// `stacked[i]`: window i sits below window i-1 in the same column (a column is a run of windows).
    pub stacked: Vec<bool>,
    /// Share of the column height each window takes (1.0 = an equal share).
    pub weights: Vec<f64>,
    pub focused: usize,
    /// Horizontal scroll of the viewport, in logical pixels.
    pub scroll: i32,
    /// Window currently fullscreen on this workspace, if any.
    pub fullscreen: Option<Window>,
    /// Floating windows with their rectangle, bottom to top.
    pub floating: Vec<(Window, Rectangle<i32, Logical>)>,
    /// The topmost floating window has the focus (instead of the focused column).
    pub float_focus: bool,
    /// Floating mode (Mod+V): every window of this workspace floats, new ones too.
    pub float_mode: bool,
    /// The windows the mode floated (and not the ones you floated yourself): they tile again when the mode goes off.
    pub float_forced: Vec<Window>,
    /// Where the mode's windows floated when it last went off (even if that is off screen): they go back there when it is turned on again.
    pub float_saved: Vec<(Window, Rectangle<i32, Logical>)>,
}

pub struct Layout {
    pub workspaces: Vec<Workspace>,
    pub active: usize,
    /// Minimized windows with the workspace they came from, oldest first.
    pub minimized: Vec<(usize, Window)>,
    /// Rectangles of minimized windows that were floating, so they come back floating.
    pub minimized_float: Vec<(Window, Rectangle<i32, Logical>)>,
}

impl Layout {
    pub fn new(workspaces: usize) -> Self {
        Self { workspaces: (0..workspaces.max(1)).map(|_| Workspace::default()).collect(), active: 0, minimized: Vec::new(), minimized_float: Vec::new() }
    }

    /// Change the number of workspaces; windows on removed ones move to the last one kept.
    pub fn resize_workspaces(&mut self, n: usize) {
        let n = n.max(1);
        while self.workspaces.len() < n {
            self.workspaces.push(Workspace::default());
        }
        while self.workspaces.len() > n {
            let gone = self.workspaces.pop().unwrap();
            let keep = self.workspaces.last_mut().unwrap();
            keep.columns.extend(gone.columns);
            keep.ratios.extend(gone.ratios);
            keep.stacked.extend(gone.stacked);
            keep.weights.extend(gone.weights);
            keep.floating.extend(gone.floating);
        }
        for (i, w) in self.minimized.iter_mut().enumerate() {
            let _ = i;
            w.0 = w.0.min(n - 1);
        }
        self.active = self.active.min(n - 1);
    }

    pub fn ws(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    pub fn ws_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active]
    }

    pub fn focused_window(&self) -> Option<&Window> {
        let ws = self.ws();
        if ws.float_focus {
            if let Some((w, _)) = ws.floating.last() {
                return Some(w);
            }
        }
        ws.columns.get(ws.focused)
    }

    /// New windows open to the right of the focused column and take focus.
    pub fn add_window(&mut self, window: Window) {
        let ws = self.ws_mut();
        ws.fullscreen = None;
        ws.float_focus = false;
        let at = if ws.columns.is_empty() { 0 } else { ws.col_end(ws.focused) + 1 };
        ws.insert_column(at, window);
        ws.focused = at;
    }

    pub fn find(&self, surface: &WlSurface) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(w, ws)| {
            ws.columns
                .iter()
                .position(|win| is_surface(win, surface))
                .map(|c| (w, c))
        })
    }

    /// Position of a floating window: (workspace, index).
    pub fn find_floating(&self, surface: &WlSurface) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(w, ws)| {
            ws.floating
                .iter()
                .position(|(win, _)| is_surface(win, surface))
                .map(|i| (w, i))
        })
    }

    /// The window owning `surface`, whether tiled, floating or minimized.
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<&Window> {
        self.find(surface)
            .map(|(w, c)| &self.workspaces[w].columns[c])
            .or_else(|| self.find_floating(surface).map(|(w, i)| &self.workspaces[w].floating[i].0))
            .or_else(|| self.minimized.iter().map(|(_, w)| w).find(|w| is_surface(w, surface)))
    }

    /// Remove the window owning `surface`. Returns true if it was in this layout.
    pub fn remove(&mut self, surface: &WlSurface) -> bool {
        if let Some(i) = self.minimized.iter().position(|(_, w)| is_surface(w, surface)) {
            let gone = self.minimized.remove(i).1;
            self.minimized_float.retain(|(w, _)| *w != gone);
            return true;
        }
        if let Some((w, i)) = self.find_floating(surface) {
            let ws = &mut self.workspaces[w];
            ws.floating.remove(i);
            if ws.floating.is_empty() {
                ws.float_focus = false;
            }
            return true;
        }
        let Some((w, c)) = self.find(surface) else { return false };
        let ws = &mut self.workspaces[w];
        ws.remove_at(c);
        if ws.fullscreen.as_ref().is_some_and(|f| !ws.columns.contains(f)) {
            ws.fullscreen = None;
        }
        if ws.focused > c || ws.focused >= ws.columns.len() {
            ws.focused = ws.focused.saturating_sub(1);
        }
        true
    }

    /// Make `window` the focused column (switching workspace if needed).
    pub fn focus_window(&mut self, window: &Window) {
        for (w, ws) in self.workspaces.iter_mut().enumerate() {
            if let Some(i) = ws.floating.iter().position(|(x, _)| x == window) {
                let entry = ws.floating.remove(i);
                ws.floating.push(entry); // raise
                ws.float_focus = true;
                self.active = w;
                return;
            }
            if let Some(c) = ws.columns.iter().position(|x| x == window) {
                ws.focused = c;
                ws.float_focus = false;
                self.active = w;
                return;
            }
        }
    }

    pub fn focus_column(&mut self, delta: isize) {
        let ws = self.ws_mut();
        ws.fullscreen = None;
        ws.float_focus = false;
        if ws.columns.is_empty() {
            return;
        }
        let ranges = ws.column_ranges();
        let cur = ranges.iter().position(|(s, e)| (*s..=*e).contains(&ws.focused)).unwrap_or(0);
        let row = ws.focused - ranges[cur].0;
        let next = (cur as isize + delta).clamp(0, ranges.len() as isize - 1) as usize;
        let (s, e) = ranges[next];
        ws.focused = s + row.min(e - s);
    }

    pub fn move_column(&mut self, delta: isize) {
        let ws = self.ws_mut();
        ws.fullscreen = None;
        ws.float_focus = false;
        let mut ranges = ws.column_ranges();
        let Some(cur) = ranges.iter().position(|(s, e)| (*s..=*e).contains(&ws.focused)) else { return };
        let to = cur as isize + delta;
        if to < 0 || to >= ranges.len() as isize {
            return;
        }
        let row = ws.focused - ranges[cur].0;
        ranges.swap(cur, to as usize);
        ws.reorder(&ranges);
        let start = ranges.iter().take(to as usize).map(|(s, e)| e - s + 1).sum::<usize>();
        ws.focused = start + row;
    }

    /// Take `window` out of wherever it is and make it its own column at column index `to` (counted among the other columns).
    pub fn move_column_to(&mut self, window: &Window, to: usize) {
        let ws = self.ws_mut();
        let Some(from) = ws.columns.iter().position(|w| w == window) else { return };
        let ratio = ws.ratios[from];
        let was_leader = !ws.stacked[from];
        ws.remove_at(from);
        let ranges = ws.column_ranges();
        let at = ranges.get(to).map(|(s, _)| *s).unwrap_or(ws.columns.len());
        ws.insert_column(at, window.clone());
        if was_leader {
            ws.ratios[at] = ratio;
        }
        ws.focused = at;
        ws.float_focus = false;
    }

    /// Drop `window` onto `target`: it joins that window's column, above or below it, splitting the height.
    pub fn stack_onto(&mut self, window: &Window, target: &Window, below: bool) {
        if window == target {
            return;
        }
        let ws = self.ws_mut();
        let Some(from) = ws.columns.iter().position(|w| w == window) else { return };
        ws.remove_at(from);
        let Some(t) = ws.columns.iter().position(|w| w == target) else { return };
        let at = if below {
            ws.insert_stacked(t + 1, window.clone());
            t + 1
        } else {
            // above: take the target's place, and the target moves down
            let leader = !ws.stacked[t];
            let ratio = ws.ratios[t];
            ws.insert_stacked(t, window.clone());
            if leader {
                ws.stacked[t] = false;
                ws.ratios[t] = ratio;
                ws.stacked[t + 1] = true;
                ws.ratios[t + 1] = 0.0;
            }
            t
        };
        ws.focused = at;
        ws.float_focus = false;
    }

    /// Take `window` out and make it its own column right before or after the column holding `target`.
    pub fn move_beside(&mut self, window: &Window, target: &Window, after: bool) {
        if window == target {
            return;
        }
        let ws = self.ws_mut();
        let Some(from) = ws.columns.iter().position(|w| w == window) else { return };
        let ratio = ws.ratios[from];
        let was_leader = !ws.stacked[from];
        ws.remove_at(from);
        let Some(t) = ws.columns.iter().position(|w| w == target) else { return };
        let at = if after { ws.col_end(t) + 1 } else { ws.col_start(t) };
        ws.insert_column(at, window.clone());
        if was_leader {
            ws.ratios[at] = ratio;
        }
        ws.focused = at;
        ws.float_focus = false;
    }

    pub fn switch_workspace(&mut self, delta: isize) {
        self.active = (self.active as isize + delta).clamp(0, self.workspaces.len() as isize - 1) as usize;
    }

    /// Send the focused window to the workspace above/below and follow it.
    pub fn move_to_workspace(&mut self, delta: isize) {
        let to = (self.active as isize + delta).clamp(0, self.workspaces.len() as isize - 1) as usize;
        if self.ws().float_focus && to != self.active {
            if let Some(entry) = self.ws_mut().floating.pop() {
                if self.ws().floating.is_empty() {
                    self.ws_mut().float_focus = false;
                }
                self.active = to;
                let ws = self.ws_mut();
                ws.floating.push(entry);
                ws.float_focus = true;
            }
            return;
        }
        if to == self.active || self.ws().columns.is_empty() {
            return;
        }
        let from = self.active;
        let ws = &mut self.workspaces[from];
        let window = ws.remove_at(ws.focused);
        ws.focused = ws.focused.min(ws.columns.len().saturating_sub(1));
        self.active = to;
        self.add_window(window);
    }
}

/// Width presets that `Mod+R` cycles through, as fractions of the usable width.
pub const PRESETS: [f64; 4] = [1.0 / 3.0, 0.5, 2.0 / 3.0, 1.0];

/// Pixel width for a column taking `ratio` of the usable width, with gaps at both sides and between columns.
pub fn column_width(out_w: i32, gap: i32, ratio: f64) -> i32 {
    (((out_w - gap) as f64 * ratio) as i32 - gap).max(50)
}

pub fn column_x(widths: &[i32], index: usize, gap: i32) -> i32 {
    gap + widths[..index].iter().sum::<i32>() + index as i32 * gap
}

pub fn column_height(out_h: i32, gap: i32) -> i32 {
    (out_h - 2 * gap).max(50)
}

/// Smallest scroll change that brings the focused column fully into view.
pub fn solve_scroll(widths: &[i32], focused: usize, scroll: i32, out_w: i32, gap: i32) -> i32 {
    if widths.is_empty() {
        return 0;
    }
    let total = column_x(widths, widths.len(), gap);
    if total <= out_w {
        return 0;
    }
    let x = column_x(widths, focused, gap);
    let w = widths[focused];
    let mut s = scroll;
    if x - s < gap {
        s = x - gap;
    }
    if x + w - s > out_w - gap {
        s = x + w - out_w + gap;
    }
    s.clamp(0, total - out_w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_half_columns_fill_screen() {
        let w = column_width(1920, 8, 0.5);
        assert_eq!(w, 948);
        assert_eq!(column_x(&[w, w], 2, 8), 1920);
    }

    #[test]
    fn no_scroll_when_everything_fits() {
        assert_eq!(solve_scroll(&[948, 948], 1, 0, 1920, 8), 0);
    }

    #[test]
    fn scrolls_to_reveal_focused_and_back() {
        let w = [948; 5];
        let s = solve_scroll(&w, 4, 0, 1920, 8);
        let x = column_x(&w, 4, 8) - s;
        assert!(x >= 8 && x + 948 <= 1920 - 8);
        assert_eq!(solve_scroll(&w, 0, s, 1920, 8), 0);
    }

    #[test]
    fn mixed_widths() {
        let w = [column_width(1920, 8, 1.0 / 3.0), column_width(1920, 8, 1.0)];
        let s = solve_scroll(&w, 1, 0, 1920, 8);
        assert_eq!(column_x(&w, 1, 8) - s, 8);
    }

    #[test]
    fn presets_cycle() {
        assert!(PRESETS.windows(2).all(|p| p[0] < p[1]));
    }
}

impl Layout {
    /// Cycle the focused column through the width presets. `default` is the configured ratio.
    pub fn cycle_width(&mut self, default: f64) {
        let ws = self.ws_mut();
        let lead = ws.col_start(ws.focused);
        let Some(r) = ws.ratios.get_mut(lead) else { return };
        let cur = if *r == 0.0 { default } else { *r };
        *r = PRESETS.iter().copied().find(|p| *p > cur + 0.01).unwrap_or(PRESETS[0]);
    }

    /// Nudge the focused column's width by `delta` (fraction of usable width).
    pub fn adjust_width(&mut self, default: f64, delta: f64) {
        let ws = self.ws_mut();
        let lead = ws.col_start(ws.focused);
        let Some(r) = ws.ratios.get_mut(lead) else { return };
        let cur = if *r == 0.0 { default } else { *r };
        *r = (cur + delta).clamp(0.1, 1.0);
    }

    pub fn toggle_fullscreen(&mut self) {
        let ws = self.ws_mut();
        ws.fullscreen = match (ws.fullscreen.take(), ws.columns.get(ws.focused)) {
            (None, Some(w)) => Some(w.clone()),
            _ => None,
        };
    }
}

impl Layout {
    /// Take the focused window off the workspace and remember it as minimized.
    pub fn minimize_focused(&mut self) -> bool {
        let active = self.active;
        if self.ws().float_focus {
            let ws = self.ws_mut();
            if let Some((window, rect)) = ws.floating.pop() {
                if ws.floating.is_empty() {
                    ws.float_focus = false;
                }
                if ws.fullscreen.as_ref() == Some(&window) {
                    ws.fullscreen = None;
                }
                self.minimized_float.push((window.clone(), rect));
                self.minimized.push((active, window));
                return true;
            }
        }
        let ws = self.ws_mut();
        if ws.columns.is_empty() {
            return false;
        }
        let at = ws.focused;
        let window = ws.remove_at(at);
        if ws.fullscreen.as_ref() == Some(&window) {
            ws.fullscreen = None;
        }
        ws.focused = ws.focused.min(ws.columns.len().saturating_sub(1));
        self.minimized.push((active, window));
        true
    }

    /// Bring minimized window `index` back on the workspace it left, focused.
    pub fn restore(&mut self, index: usize) -> Option<Window> {
        if index >= self.minimized.len() {
            return None;
        }
        let (ws, window) = self.minimized.remove(index);
        self.active = ws.min(self.workspaces.len() - 1);
        if let Some(pos) = self.minimized_float.iter().position(|(w, _)| *w == window) {
            let (_, rect) = self.minimized_float.remove(pos);
            let ws = self.ws_mut();
            ws.floating.push((window.clone(), rect));
            ws.float_focus = true;
            return Some(window);
        }
        self.add_window(window.clone());
        Some(window)
    }

    pub fn restore_last(&mut self) -> Option<Window> {
        self.restore(self.minimized.len().checked_sub(1)?)
    }
}

impl Layout {
    /// Float the focused window (at `rect`), or tile it again if it already floats.
    pub fn toggle_floating(&mut self, rect: Rectangle<i32, Logical>) {
        let ws = self.ws_mut();
        if ws.float_focus {
            if let Some((window, _)) = ws.floating.pop() {
                if ws.floating.is_empty() {
                    ws.float_focus = false;
                }
                self.add_window(window);
            }
        } else if let Some(window) = ws.columns.get(ws.focused).cloned() {
            self.float_window(&window, rect);
        }
    }

    /// Float `window` because its workspace is in floating mode (it tiles again when the mode goes off).
    pub fn float_by_mode(&mut self, window: &Window, rect: Rectangle<i32, Logical>) {
        self.float_window(window, rect);
        if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.floating.iter().any(|(w, _)| w == window)) {
            ws.float_forced.push(window.clone());
        }
    }

    /// Turn floating mode on or off for the active workspace. On: every tiled window floats where `rect_of(window, index)` says.
    /// Off: the windows the mode floated go back into columns. Returns the new state.
    pub fn toggle_float_workspace(&mut self, mut rect_of: impl FnMut(&Window, usize) -> Rectangle<i32, Logical>) -> bool {
        if self.ws().float_mode {
            let forced = std::mem::take(&mut self.ws_mut().float_forced);
            self.ws_mut().float_mode = false;
            for w in forced {
                let ws = self.ws_mut();
                if let Some(i) = ws.floating.iter().position(|(x, _)| *x == w) {
                    let (_, rect) = ws.floating.remove(i);
                    // remember the place, so turning the mode on again brings the window back to it
                    ws.float_saved.retain(|(x, _)| *x != w);
                    ws.float_saved.push((w.clone(), rect));
                    if ws.floating.is_empty() {
                        ws.float_focus = false;
                    }
                    self.add_window(w);
                }
            }
            false
        } else {
            self.ws_mut().float_mode = true;
            // the focused window floats last, so it ends up on top; `rect_of` gets each window's place in that order
            let mut wins: Vec<Window> = self.ws().columns.clone();
            if let Some(f) = wins.get(self.ws().focused).cloned() {
                wins.retain(|w| *w != f);
                wins.push(f);
            }
            // windows that closed meanwhile are forgotten
            let alive = wins.clone();
            self.ws_mut().float_saved.retain(|(x, _)| alive.contains(x));
            for (i, w) in wins.iter().enumerate() {
                let saved = self.ws().float_saved.iter().find(|(x, _)| x == w).map(|(_, r)| *r);
                let r = saved.unwrap_or_else(|| rect_of(w, i));
                self.float_by_mode(w, r);
            }
            true
        }
    }

    /// Take `window` out of the tiling strip and float it at `rect`, focused.
    pub fn float_window(&mut self, window: &Window, rect: Rectangle<i32, Logical>) {
        for ws in &mut self.workspaces {
            if let Some(c) = ws.columns.iter().position(|w| w == window) {
                ws.remove_at(c);
                if ws.fullscreen.as_ref() == Some(window) {
                    ws.fullscreen = None;
                }
                if ws.focused > c || ws.focused >= ws.columns.len() {
                    ws.focused = ws.focused.saturating_sub(1);
                }
                ws.floating.push((window.clone(), rect));
                ws.float_focus = true;
                return;
            }
        }
    }

    /// Mutable access to a floating window's rectangle on the active workspace.
    pub fn float_rect_mut(&mut self, window: &Window) -> Option<&mut Rectangle<i32, Logical>> {
        self.ws_mut().floating.iter_mut().find(|(w, _)| w == window).map(|(_, r)| r)
    }

    pub fn is_floating(&self, window: &Window) -> bool {
        self.workspaces.iter().any(|ws| ws.floating.iter().any(|(w, _)| w == window))
    }
}

impl Workspace {
    /// First window of the column holding window `i`.
    pub fn col_start(&self, mut i: usize) -> usize {
        while i > 0 && self.stacked.get(i).copied().unwrap_or(false) {
            i -= 1;
        }
        i
    }

    /// Last window of the column holding window `i`.
    pub fn col_end(&self, mut i: usize) -> usize {
        while i + 1 < self.columns.len() && self.stacked[i + 1] {
            i += 1;
        }
        i
    }

    /// Each column as an inclusive range of window indices, left to right.
    pub fn column_ranges(&self) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for i in 0..self.columns.len() {
            if self.stacked[i] && !out.is_empty() {
                out.last_mut().unwrap().1 = i;
            } else {
                out.push((i, i));
            }
        }
        out
    }

    /// Put `window` in a column of its own at window index `at` (which must be the start of a column).
    pub fn insert_column(&mut self, at: usize, window: Window) {
        self.columns.insert(at, window);
        self.ratios.insert(at, 0.0);
        self.stacked.insert(at, false);
        self.weights.insert(at, 1.0);
    }

    /// Put `window` into the column of the window before index `at`, at row position `at`.
    pub fn insert_stacked(&mut self, at: usize, window: Window) {
        self.columns.insert(at, window);
        self.ratios.insert(at, 0.0);
        self.stacked.insert(at, at > 0);
        self.weights.insert(at, 1.0);
    }

    /// Remove window `i`, keeping the rest of its column together (and its width).
    pub fn remove_at(&mut self, i: usize) -> Window {
        let window = self.columns.remove(i);
        let ratio = self.ratios.remove(i);
        let was_leader = !self.stacked.remove(i);
        self.weights.remove(i);
        if was_leader && i < self.columns.len() && self.stacked[i] {
            self.stacked[i] = false;
            self.ratios[i] = ratio;
        }
        if i < self.stacked.len() && i == 0 {
            self.stacked[0] = false;
        }
        window
    }

    /// Rebuild the window order from column ranges (each range a whole column of the current order).
    pub fn reorder(&mut self, ranges: &[(usize, usize)]) {
        let order: Vec<usize> = ranges.iter().flat_map(|(s, e)| *s..=*e).collect();
        self.columns = order.iter().map(|&i| self.columns[i].clone()).collect();
        self.ratios = order.iter().map(|&i| self.ratios[i]).collect();
        self.stacked = order.iter().map(|&i| self.stacked[i]).collect();
        self.weights = order.iter().map(|&i| self.weights[i]).collect();
    }
}

impl Layout {
    /// Move focus up or down inside the focused column.
    pub fn focus_in_column(&mut self, delta: isize) {
        let ws = self.ws_mut();
        if ws.columns.is_empty() {
            return;
        }
        ws.float_focus = false;
        let (s, e) = (ws.col_start(ws.focused), ws.col_end(ws.focused));
        ws.focused = (ws.focused as isize + delta).clamp(s as isize, e as isize) as usize;
    }

    /// Put the focused window into the column to its left, below that column's windows.
    pub fn consume_left(&mut self) {
        let ws = self.ws_mut();
        if ws.columns.is_empty() {
            return;
        }
        let i = ws.focused;
        let s = ws.col_start(i);
        if s == 0 {
            return; // no column to the left
        }
        let w = ws.remove_at(i);
        // the column on the left ends right before `s`; the window joins it below that
        ws.insert_stacked(s, w);
        ws.focused = s;
        ws.float_focus = false;
    }

    /// Take the focused window out of its column into a column of its own, to the right.
    pub fn expel(&mut self) {
        let ws = self.ws_mut();
        if ws.columns.is_empty() {
            return;
        }
        let i = ws.focused;
        if !ws.stacked[i] && (i + 1 >= ws.columns.len() || !ws.stacked[i + 1]) {
            return; // already alone
        }
        let was_leader = !ws.stacked[i];
        let w = ws.remove_at(i);
        let old_start = if was_leader { i } else { ws.col_start(i - 1) };
        let end = ws.col_end(old_start);
        ws.insert_column(end + 1, w);
        ws.focused = end + 1;
        ws.float_focus = false;
    }
}

impl Workspace {
    /// Nothing on it: no tiled or floating windows.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.floating.is_empty()
    }
}

impl Layout {
    /// Move unused workspaces to the end, keeping the order of the others (and of the current one, even if empty), so the
    /// workspaces in use are always the first ones. Returns whether anything moved.
    pub fn compact(&mut self) -> bool {
        let n = self.workspaces.len();
        let active = self.active;
        let keep: Vec<bool> =
            (0..n).map(|i| i == active || !self.workspaces[i].is_empty() || self.minimized.iter().any(|(w, _)| *w == i)).collect();
        let mut order: Vec<usize> = (0..n).filter(|i| keep[*i]).collect();
        order.extend((0..n).filter(|i| !keep[*i]));
        if order.iter().enumerate().all(|(pos, old)| pos == *old) {
            return false;
        }
        let mut old: Vec<Option<Workspace>> = std::mem::take(&mut self.workspaces).into_iter().map(Some).collect();
        let mut new_index = vec![0usize; n];
        for (pos, &o) in order.iter().enumerate() {
            new_index[o] = pos;
            self.workspaces.push(old[o].take().expect("each workspace is moved once"));
        }
        self.active = new_index[active];
        for m in &mut self.minimized {
            m.0 = new_index[m.0.min(n - 1)];
        }
        true
    }
}

impl Layout {
    /// Move `window` (tiled) to the end of workspace `to` without changing which workspace is active.
    pub fn send_to_workspace(&mut self, window: &Window, to: usize) {
        if to >= self.workspaces.len() {
            return;
        }
        for from in 0..self.workspaces.len() {
            if from == to {
                continue;
            }
            if let Some(c) = self.workspaces[from].columns.iter().position(|w| w == window) {
                let ws = &mut self.workspaces[from];
                let w = ws.remove_at(c);
                if ws.fullscreen.as_ref() == Some(&w) {
                    ws.fullscreen = None;
                }
                if ws.focused > c || ws.focused >= ws.columns.len() {
                    ws.focused = ws.focused.saturating_sub(1);
                }
                let dest = &mut self.workspaces[to];
                let at = dest.columns.len();
                dest.insert_column(at, w);
                return;
            }
        }
    }
}

/// Does `win` draw on `surface`? (An X11 window has none until Xwayland has associated one.)
pub fn is_surface(win: &Window, surface: &WlSurface) -> bool {
    win.wl_surface().is_some_and(|s| &*s == surface)
}

impl Layout {
    /// Position of a tiled window: (workspace, column index).
    pub fn find_window(&self, window: &Window) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(w, ws)| ws.columns.iter().position(|x| x == window).map(|c| (w, c)))
    }

    /// Is `window` anywhere in the layout (tiled, floating or minimized)?
    pub fn contains(&self, window: &Window) -> bool {
        self.find_window(window).is_some()
            || self.workspaces.iter().any(|ws| ws.floating.iter().any(|(w, _)| w == window))
            || self.minimized.iter().any(|(_, w)| w == window)
    }

    /// The layout's window for an X11 window id.
    pub fn window_for_x11(&self, id: u32) -> Option<&Window> {
        let is = |w: &&Window| w.x11_surface().is_some_and(|x| x.window_id() == id);
        self.workspaces
            .iter()
            .flat_map(|ws| ws.columns.iter().chain(ws.floating.iter().map(|(w, _)| w)))
            .chain(self.minimized.iter().map(|(_, w)| w))
            .find(is)
    }

    /// Remove `window` wherever it is. Returns true if it was in this layout.
    pub fn remove_window(&mut self, window: &Window) -> bool {
        if let Some(i) = self.minimized.iter().position(|(_, w)| w == window) {
            self.minimized.remove(i);
            self.minimized_float.retain(|(w, _)| w != window);
            return true;
        }
        for ws in &mut self.workspaces {
            if let Some(i) = ws.floating.iter().position(|(w, _)| w == window) {
                ws.floating.remove(i);
                if ws.floating.is_empty() {
                    ws.float_focus = false;
                }
                if ws.fullscreen.as_ref() == Some(window) {
                    ws.fullscreen = None;
                }
                return true;
            }
        }
        let Some((w, c)) = self.find_window(window) else { return false };
        let ws = &mut self.workspaces[w];
        ws.remove_at(c);
        if ws.fullscreen.as_ref().is_some_and(|f| !ws.columns.contains(f)) {
            ws.fullscreen = None;
        }
        if ws.focused > c || ws.focused >= ws.columns.len() {
            ws.focused = ws.focused.saturating_sub(1);
        }
        true
    }
}

impl Layout {
    /// Make window `idx` of workspace `ws` (tiled ones first, then floating, as `ipc_state` lists them) the focused one
    /// on the active workspace.
    pub fn focus_at(&mut self, ws: usize, idx: usize) {
        if ws >= self.workspaces.len() {
            return;
        }
        self.active = ws;
        let w = &mut self.workspaces[ws];
        if idx < w.columns.len() {
            w.focused = idx;
            w.float_focus = false;
        } else if let Some(f) = idx.checked_sub(w.columns.len()).filter(|f| *f < w.floating.len()) {
            // a floating window is raised by moving it to the end
            let entry = w.floating.remove(f);
            w.floating.push(entry);
            w.float_focus = true;
        }
    }
}

// ------------------------------------------------------------------------ one layout per monitor

/// The layouts of all monitors: each monitor has its own workspaces. It derefs to the layout of the *focused* monitor
/// (the one the pointer is on), so code that works on "the" layout acts on that monitor; the methods defined here
/// look through every monitor instead.
pub struct Layouts {
    monitors: Vec<(String, Layout)>,
    focus: usize,
}

impl std::ops::Deref for Layouts {
    type Target = Layout;
    fn deref(&self) -> &Layout {
        &self.monitors[self.focus].1
    }
}

impl std::ops::DerefMut for Layouts {
    fn deref_mut(&mut self) -> &mut Layout {
        &mut self.monitors[self.focus].1
    }
}

impl Layout {
    /// Every window of the layout: tiled, floating and minimized.
    pub fn all_windows(&self) -> Vec<Window> {
        let mut all: Vec<Window> = self.workspaces.iter().flat_map(|ws| ws.columns.iter().cloned()).collect();
        all.extend(self.workspaces.iter().flat_map(|ws| ws.floating.iter().map(|(w, _)| w.clone())));
        all.extend(self.minimized.iter().map(|(_, w)| w.clone()));
        all
    }
}

impl Layouts {
    /// Before any monitor is known there is one unnamed layout; it is given to the first monitor that appears.
    pub fn new(workspaces: usize) -> Self {
        Self { monitors: vec![(String::new(), Layout::new(workspaces))], focus: 0 }
    }

    /// Name of the focused monitor ("" before the first one appears).
    pub fn focused_name(&self) -> &str {
        &self.monitors[self.focus].0
    }

    /// Names of all monitors that have a layout.
    pub fn names(&self) -> Vec<String> {
        self.monitors.iter().map(|(n, _)| n.clone()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Layout> {
        self.monitors.iter().find(|(n, _)| n == name).map(|(_, l)| l)
    }

    /// Make `name` the focused monitor. Returns whether it changed.
    pub fn focus_output(&mut self, name: &str) -> bool {
        match self.monitors.iter().position(|(n, _)| n == name) {
            Some(i) if i != self.focus => {
                self.focus = i;
                true
            }
            _ => false,
        }
    }

    /// Per monitor: its name and layout.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Layout)> {
        self.monitors.iter().map(|(n, l)| (n.as_str(), l))
    }

    /// Match the layouts to the monitors that exist: `wanted` is the list of monitor names (the first one is where
    /// windows of a vanished monitor go). Returns whether anything changed.
    pub fn sync(&mut self, wanted: &[String], workspaces: usize) -> bool {
        if wanted.is_empty() {
            return false;
        }
        let before = self.names();
        // the unnamed starting layout belongs to the first monitor
        if self.monitors.len() == 1 && self.monitors[0].0.is_empty() {
            self.monitors[0].0 = wanted[0].clone();
        }
        let focused = self.monitors[self.focus].0.clone();
        // monitors that went away hand their windows to the first remaining one
        let mut orphans: Vec<Window> = Vec::new();
        self.monitors.retain(|(n, l)| {
            let keep = wanted.contains(n);
            if !keep {
                orphans.extend(l.all_windows());
            }
            keep
        });
        for name in wanted {
            if !self.monitors.iter().any(|(n, _)| n == name) {
                self.monitors.push((name.clone(), Layout::new(workspaces)));
            }
        }
        let target = self.monitors.iter().position(|(n, _)| n == &wanted[0]).unwrap_or(0);
        for w in orphans {
            self.monitors[target].1.add_window(w);
        }
        self.focus = self.monitors.iter().position(|(n, _)| *n == focused).unwrap_or(target);
        self.names() != before
    }

    /// Move `window` to monitor `to` (as a tiled window on that monitor's current workspace) and focus it there.
    pub fn move_window_to(&mut self, window: &Window, to: &str) -> bool {
        let Some(dst) = self.monitors.iter().position(|(n, _)| n == to) else { return false };
        let Some(src) = self.monitors.iter().position(|(_, l)| l.contains(window)) else { return false };
        if src == dst {
            return false;
        }
        self.monitors[src].1.remove_window(window);
        let l = &mut self.monitors[dst].1;
        l.add_window(window.clone());
        self.focus = dst;
        true
    }

    // --- lookups that look through every monitor (they shadow the focused monitor's ones of the same name)

    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<&Window> {
        self.monitors.iter().find_map(|(_, l)| l.window_for_surface(surface))
    }

    pub fn window_for_x11(&self, id: u32) -> Option<&Window> {
        self.monitors.iter().find_map(|(_, l)| l.window_for_x11(id))
    }

    pub fn contains(&self, window: &Window) -> bool {
        self.monitors.iter().any(|(_, l)| l.contains(window))
    }

    pub fn is_floating(&self, window: &Window) -> bool {
        self.monitors.iter().any(|(_, l)| l.is_floating(window))
    }

    pub fn remove(&mut self, surface: &WlSurface) -> bool {
        self.monitors.iter_mut().any(|(_, l)| l.remove(surface))
    }

    pub fn remove_window(&mut self, window: &Window) -> bool {
        self.monitors.iter_mut().any(|(_, l)| l.remove_window(window))
    }

    /// Focus `window`, which also makes its monitor the focused one.
    pub fn focus_window(&mut self, window: &Window) {
        if let Some(i) = self.monitors.iter().position(|(_, l)| l.contains(window)) {
            self.focus = i;
        }
        self.monitors[self.focus].1.focus_window(window);
    }

    pub fn float_rect_mut(&mut self, window: &Window) -> Option<&mut Rectangle<i32, Logical>> {
        self.monitors.iter_mut().find_map(|(_, l)| l.float_rect_mut(window))
    }

    /// Float `window` on the monitor that has it (else on the focused one).
    pub fn float_window(&mut self, window: &Window, rect: Rectangle<i32, Logical>) {
        let i = self.monitors.iter().position(|(_, l)| l.contains(window)).unwrap_or(self.focus);
        self.monitors[i].1.float_window(window, rect);
    }

    /// Every window on every monitor.
    pub fn all_windows(&self) -> Vec<Window> {
        self.monitors.iter().flat_map(|(_, l)| l.all_windows()).collect()
    }

    /// Tidy up the workspaces of every monitor.
    pub fn compact(&mut self) -> bool {
        let mut changed = false;
        for (_, l) in &mut self.monitors {
            changed |= l.compact();
        }
        changed
    }

    pub fn resize_workspaces(&mut self, n: usize) {
        for (_, l) in &mut self.monitors {
            l.resize_workspaces(n);
        }
    }
}

impl Layouts {
    /// The tiled window owning `surface`, on any monitor.
    pub fn tiled_window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.monitors.iter().find_map(|(_, l)| l.find(surface).map(|(w, c)| l.workspaces[w].columns[c].clone()))
    }

    /// Take `surface` out of fullscreen wherever it is. Returns whether it was fullscreen.
    pub fn unfullscreen_surface(&mut self, surface: &WlSurface) -> bool {
        for (_, l) in &mut self.monitors {
            if let Some((w, _)) = l.find(surface) {
                return l.workspaces[w].fullscreen.take().is_some();
            }
        }
        false
    }

    /// Is `window` tiled or floating (not minimized) on some monitor?
    pub fn is_placed(&self, window: &Window) -> bool {
        self.monitors.iter().any(|(_, l)| l.find_window(window).is_some() || l.workspaces.iter().any(|ws| ws.floating.iter().any(|(w, _)| w == window)))
    }

    /// Take `window` out of fullscreen on every monitor. Returns whether any was.
    pub fn unfullscreen_window(&mut self, window: &Window) -> bool {
        let mut changed = false;
        for (_, l) in &mut self.monitors {
            for ws in &mut l.workspaces {
                if ws.fullscreen.as_ref() == Some(window) {
                    ws.fullscreen = None;
                    changed = true;
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;

    fn names(l: &Layouts) -> Vec<String> {
        let mut n = l.names();
        n.sort();
        n
    }

    #[test]
    fn first_monitor_adopts_the_starting_layout() {
        let mut l = Layouts::new(4);
        assert_eq!(l.focused_name(), "");
        assert!(l.sync(&["DP-1".into()], 4));
        assert_eq!(l.focused_name(), "DP-1");
        assert_eq!(l.workspaces.len(), 4);
    }

    #[test]
    fn each_monitor_has_its_own_workspaces() {
        let mut l = Layouts::new(3);
        l.sync(&["DP-1".into(), "HDMI-A-1".into()], 3);
        assert_eq!(names(&l), ["DP-1", "HDMI-A-1"]);
        l.active = 2; // DP-1 is focused
        assert!(l.focus_output("HDMI-A-1"));
        assert_eq!(l.active, 0, "the other monitor keeps its own active workspace");
        assert!(l.focus_output("DP-1"));
        assert_eq!(l.active, 2);
        assert!(!l.focus_output("DP-1"), "already focused");
        assert!(!l.focus_output("nope"));
    }

    #[test]
    fn unplugging_keeps_focus_and_one_monitor_is_enough() {
        let mut l = Layouts::new(2);
        l.sync(&["DP-1".into(), "HDMI-A-1".into()], 2);
        l.focus_output("HDMI-A-1");
        assert!(l.sync(&["DP-1".into()], 2));
        assert_eq!(l.focused_name(), "DP-1", "the focused monitor went away: fall back to the first");
        assert!(!l.sync(&["DP-1".into()], 2), "nothing changed");
        assert!(!l.sync(&[], 2), "no monitors known: leave everything alone");
    }
}
