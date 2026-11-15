//! Turning a raw log line into coloured text: ANSI colours the program
//! printed, a level colour for plain lines, and highlighted search matches.

use std::ops::Range;

use egui::text::LayoutJob;
use egui::{Color32, FontId, TextFormat};

/// A log line with its escape sequences removed, and the colour runs they
/// described (byte ranges into `text`; `None` = the default colour).
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub text: String,
    pub runs: Vec<(Range<usize>, Option<Color32>, bool)>,
}

/// The 16 basic colours, chosen to read well on a dark or light background.
pub fn ansi16(dark: bool) -> [Color32; 16] {
    let c = |hex: u32| Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    let t = if dark {
        [
            0x5C6473, 0xF0616D, 0x3FB97A, 0xE3A545, 0x5B9DFF, 0xC792EA, 0x58B4E8, 0xC8CED8, 0x7A8394, 0xFF7B86,
            0x5FD498, 0xF2BD68, 0x82B4FF, 0xD9B0F5, 0x7CCBF2, 0xF2F4F8,
        ]
    } else {
        [
            0x24292F, 0xCF222E, 0x116329, 0x7D4E00, 0x0969DA, 0x8250DF, 0x1B7C83, 0x6E7781, 0x57606A, 0xA40E26,
            0x1A7F37, 0x633C01, 0x1F6FD0, 0x8B5CF0, 0x1A7F8E, 0x57606A,
        ]
    };
    t.map(c)
}

fn xterm256(n: u8, base: &[Color32; 16]) -> Color32 {
    match n {
        0..=15 => base[n as usize],
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        _ => {
            let g = 8 + (n - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
    }
}

/// Strips escape sequences, keeping what SGR said about the foreground
/// colour and boldness.
pub fn parse(line: &str, base: &[Color32; 16]) -> Parsed {
    let mut out = Parsed { text: String::with_capacity(line.len()), runs: Vec::new() };
    if !line.as_bytes().contains(&0x1b) {
        out.text.push_str(line);
        return out;
    }
    let mut fg: Option<Color32> = None;
    let mut bold = false;
    let mut run_start = 0;
    let mut chars = line.char_indices().peekable();
    let flush = |out: &mut Parsed, start: &mut usize, fg: Option<Color32>, bold: bool| {
        let end = out.text.len();
        if end > *start && (fg.is_some() || bold) {
            out.runs.push((*start..end, fg, bold));
        }
        *start = end;
    };
    while let Some((_, c)) = chars.next() {
        if c != '\u{1b}' {
            out.text.push(c);
            continue;
        }
        match chars.peek().map(|(_, c)| *c) {
            Some('[') => {
                chars.next();
                let mut params = String::new();
                let mut fin = None;
                for (_, n) in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        fin = Some(n);
                        break;
                    }
                    params.push(n);
                }
                if fin != Some('m') {
                    continue;
                }
                flush(&mut out, &mut run_start, fg, bold);
                let nums: Vec<u16> =
                    params.split([';', ':']).map(|p| if p.is_empty() { 0 } else { p.parse().unwrap_or(u16::MAX) }).collect();
                let mut i = 0;
                while i < nums.len() {
                    match nums[i] {
                        0 => {
                            fg = None;
                            bold = false;
                        }
                        1 => bold = true,
                        22 => bold = false,
                        n @ 30..=37 => fg = Some(base[(n - 30) as usize]),
                        n @ 90..=97 => fg = Some(base[(n - 90 + 8) as usize]),
                        39 => fg = None,
                        38 => match nums.get(i + 1) {
                            Some(5) => {
                                if let Some(n) = nums.get(i + 2) {
                                    fg = Some(xterm256((*n).min(255) as u8, base));
                                }
                                i += 2;
                            }
                            Some(2) => {
                                if let (Some(r), Some(g), Some(b)) = (nums.get(i + 2), nums.get(i + 3), nums.get(i + 4)) {
                                    fg = Some(Color32::from_rgb((*r).min(255) as u8, (*g).min(255) as u8, (*b).min(255) as u8));
                                }
                                i += 4;
                            }
                            _ => {}
                        },
                        // Background: skip its arguments.
                        48 => match nums.get(i + 1) {
                            Some(5) => i += 2,
                            Some(2) => i += 4,
                            _ => {}
                        },
                        _ => {}
                    }
                    i += 1;
                }
            }
            Some(']') => {
                chars.next();
                while let Some((_, n)) = chars.next() {
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    flush(&mut out, &mut run_start, fg, bold);
    out
}

/// Byte ranges of the case-insensitive (ASCII) matches of `needle_lower`.
pub fn find_all(hay: &str, needle_lower: &str) -> Vec<Range<usize>> {
    let (h, n) = (hay.as_bytes(), needle_lower.as_bytes());
    let mut out = Vec::new();
    if n.is_empty() || n.len() > h.len() {
        return out;
    }
    let mut i = 0;
    while i + n.len() <= h.len() {
        if h[i..i + n.len()].iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b) {
            out.push(i..i + n.len());
            i += n.len();
        } else {
            i += 1;
        }
    }
    out
}

#[derive(Clone)]
pub struct Look {
    pub font: FontId,
    pub default: Color32,
    pub match_fg: Color32,
    pub match_bg: Color32,
    pub current_bg: Color32,
}

/// Appends `parsed` to `job`, colouring runs and highlighting matches.
pub fn append(job: &mut LayoutJob, parsed: &Parsed, matches: &[Range<usize>], current: bool, look: &Look) {
    let text = &parsed.text;
    if text.is_empty() {
        return;
    }
    // Every place where the style may change.
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for (r, _, _) in &parsed.runs {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    for r in matches {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut run = 0;
    let mut m = 0;
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        while run < parsed.runs.len() && parsed.runs[run].0.end <= a {
            run += 1;
        }
        while m < matches.len() && matches[m].end <= a {
            m += 1;
        }
        let (fg, _bold) = match parsed.runs.get(run) {
            Some((r, c, bold)) if r.start <= a => (c.unwrap_or(look.default), *bold),
            _ => (look.default, false),
        };
        let hit = matches.get(m).is_some_and(|r| r.start <= a);
        let format = if hit {
            TextFormat {
                font_id: look.font.clone(),
                color: look.match_fg,
                background: if current { look.current_bg } else { look.match_bg },
                ..Default::default()
            }
        } else {
            TextFormat { font_id: look.font.clone(), color: fg, ..Default::default() }
        };
        job.append(&text[a..b], 0.0, format);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_lines_pass_through() {
        let p = parse("hello", &ansi16(true));
        assert_eq!(p.text, "hello");
        assert!(p.runs.is_empty());
    }

    #[test]
    fn sgr_colours_become_runs() {
        let base = ansi16(true);
        let p = parse("a\x1b[31mred\x1b[0m b \x1b[38;5;196mx\x1b[m\x1b]0;title\x07", &base);
        assert_eq!(p.text, "ared b x");
        assert_eq!(p.runs[0], (1..4, Some(base[1]), false));
        assert_eq!(p.runs[1].0, 7..8);
    }

    #[test]
    fn matches_ignore_case() {
        assert_eq!(find_all("Error error ERR", "err"), vec![0..3, 6..9, 12..15]);
        assert!(find_all("abc", "").is_empty());
    }

    #[test]
    fn highlighted_segments() {
        let look = Look {
            font: FontId::monospace(12.0),
            default: Color32::WHITE,
            match_fg: Color32::BLACK,
            match_bg: Color32::YELLOW,
            current_bg: Color32::RED,
        };
        let p = parse("x\x1b[32mgreen\x1b[0m done", &ansi16(true));
        let m = find_all(&p.text, "een d");
        let mut job = LayoutJob::default();
        append(&mut job, &p, &m, false, &look);
        let parts: Vec<&str> = job.sections.iter().map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0]).collect();
        // (Neighbours that look the same are merged into one section.)
        assert_eq!(parts, vec!["x", "gr", "een d", "one"]);
        assert_eq!(job.sections[1].format.color, ansi16(true)[2]);
        assert_eq!(job.sections[2].format.background, Color32::YELLOW);
        assert_eq!(job.sections[3].format.color, Color32::WHITE);
    }
}
