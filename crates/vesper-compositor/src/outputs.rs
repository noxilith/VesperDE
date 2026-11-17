//! Monitor settings that need no hardware to work out: mode strings, automatic scale, placement.

/// `"2560x1440"` or `"2560x1440@144"`.
pub fn parse_mode(s: &str) -> Option<(u32, u32, Option<f64>)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (size, hz) = match s.split_once('@') {
        Some((a, b)) => (a, Some(b.trim_end_matches("Hz").trim().parse::<f64>().ok()?)),
        None => (s, None),
    };
    let (w, h) = size.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?, hz))
}

/// Pick the entry of `modes` (width, height, refresh in Hz) that matches `want`; with no refresh given, the fastest one.
pub fn pick_mode(want: &str, modes: &[(u32, u32, f64)]) -> Option<usize> {
    let (w, h, hz) = parse_mode(want)?;
    modes
        .iter()
        .enumerate()
        .filter(|(_, m)| m.0 == w && m.1 == h)
        .min_by(|(_, a), (_, b)| match hz {
            Some(hz) => (a.2 - hz).abs().partial_cmp(&(b.2 - hz).abs()).unwrap(),
            None => b.2.partial_cmp(&a.2).unwrap(),
        })
        .map(|(i, _)| i)
}

/// A sensible scale when none is configured: sharp panels get 2x or 1.5x, ordinary ones 1x.
pub fn auto_scale(px_w: i32, mm_w: i32) -> f64 {
    if mm_w <= 0 || px_w <= 0 {
        return 1.0;
    }
    let dpi = px_w as f64 / (mm_w as f64 / 25.4);
    if dpi >= 192.0 {
        2.0
    } else if dpi >= 150.0 {
        1.5
    } else {
        1.0
    }
}

/// Clamp to what a compositor can do and round to the 1/120 steps Wayland fractional scale uses.
pub fn clean_scale(s: f64) -> f64 {
    ((s.clamp(0.5, 4.0)) * 120.0).round() / 120.0
}

/// One monitor to place: its logical size and an explicit position if the user gave one.
pub struct Place {
    pub w: i32,
    pub h: i32,
    pub at: Option<(i32, i32)>,
}

/// Left to right in the given order; explicit positions win and the rest continue after the rightmost edge.
pub fn arrange(items: &[Place]) -> Vec<(i32, i32)> {
    let mut next_x = 0;
    let mut out = Vec::new();
    for p in items {
        let pos = p.at.unwrap_or((next_x, 0));
        next_x = next_x.max(pos.0 + p.w);
        out.push(pos);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(parse_mode("2560x1440@144"), Some((2560, 1440, Some(144.0))));
        assert_eq!(parse_mode("1920x1080"), Some((1920, 1080, None)));
        assert_eq!(parse_mode("nope"), None);
        let list = [(1920, 1080, 60.0), (1920, 1080, 144.0), (2560, 1440, 60.0)];
        assert_eq!(pick_mode("1920x1080", &list), Some(1));
        assert_eq!(pick_mode("1920x1080@60", &list), Some(0));
        assert_eq!(pick_mode("800x600", &list), None);
    }

    #[test]
    fn scale_and_placement() {
        assert_eq!(auto_scale(3840, 344), 2.0); // 13.6" 4K
        assert_eq!(auto_scale(1920, 527), 1.0); // 24" 1080p
        assert_eq!(auto_scale(1920, 0), 1.0);
        assert_eq!(clean_scale(9.0), 4.0);
        let p = arrange(&[Place { w: 1920, h: 1080, at: None }, Place { w: 1280, h: 720, at: None }, Place { w: 800, h: 600, at: Some((0, -600)) }]);
        assert_eq!(p, vec![(0, 0), (1920, 0), (0, -600)]);
    }
}
