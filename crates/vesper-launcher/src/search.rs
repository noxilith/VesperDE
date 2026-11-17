//! Ranking of apps against a typed query.

use vesper_apps::DesktopEntry;
use vesper_gfx::Rgba;

/// Indices into `apps`, best match first. An empty query lists everything alphabetically (the caller's order).
pub fn rank(apps: &[DesktopEntry], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..apps.len()).collect();
    }
    let mut scored: Vec<(u32, usize)> =
        apps.iter().enumerate().filter_map(|(i, a)| score(a, &q).map(|s| (s, i))).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Higher is better; None = no match. Every whitespace-separated word of the query must match.
fn score(app: &DesktopEntry, q: &str) -> Option<u32> {
    let name = app.name.to_lowercase();
    let id = app.id.to_lowercase();
    let mut total = 0;
    for word in q.split_whitespace() {
        total += word_score(&name, word).or_else(|| word_score(&id, word).map(|s| s / 2))?;
    }
    Some(total)
}

fn word_score(hay: &str, word: &str) -> Option<u32> {
    if hay == word {
        return Some(1000);
    }
    if hay.starts_with(word) {
        return Some(800);
    }
    // start of any word inside the name ("code" matches "Visual Studio Code")
    if hay.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(word)) {
        return Some(600);
    }
    if hay.contains(word) {
        return Some(400);
    }
    // in-order subsequence ("ffx" -> "firefox"), weaker the more spread out
    let mut it = hay.chars();
    let mut matched = 0;
    for ch in word.chars() {
        it.find(|c| *c == ch)?;
        matched += 1;
    }
    Some(100 + matched)
}

/// A stable, pleasant colour for an app name (used when it has no icon).
pub fn tile_color(name: &str) -> Rgba {
    let hash = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    let hue = (hash % 360) as f32;
    let (s, l) = (0.45f32, 0.42f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8, 0xff]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> DesktopEntry {
        DesktopEntry { id: name.to_lowercase().replace(' ', "-"), name: name.into(), exec: "x".into(), ..Default::default() }
    }

    #[test]
    fn prefix_beats_substring_beats_fuzzy() {
        let apps = [app("Gimp Firefox Helper"), app("Firefox"), app("Fast Image Reader Overlay Xtra")];
        let r = rank(&apps, "firefox");
        assert_eq!(r[0], 1);
        assert_eq!(r[1], 0);
        let fuzzy = rank(&apps, "ffx");
        assert!(fuzzy.contains(&1));
    }

    #[test]
    fn word_start_matches() {
        let apps = [app("Visual Studio Code"), app("Codeblocks Tool")];
        assert_eq!(rank(&apps, "code")[0], 1); // prefix of whole name wins
        assert_eq!(rank(&apps, "studio code"), vec![0]);
    }

    #[test]
    fn no_match_is_empty() {
        assert!(rank(&[app("Firefox")], "zzzz").is_empty());
    }
}
