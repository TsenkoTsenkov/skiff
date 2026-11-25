//! "What's new": the changelog's entries for this version, shown once
//! after an update and on request (palette, Settings > Updates).
//!
//! The text is `CHANGELOG.md` itself, compiled in, so it needs no network
//! and always matches the build. Only its `## [X.Y.Z]` sections are read;
//! within one, `### Added` and the like are the groups and each `- ` item
//! (with its indented continuation lines) is one entry.

use egui::text::LayoutJob;
use egui::{Align, FontId, Layout, RichText, TextFormat};
use egui_material_icons::icons;

use crate::theme::{self, Palette};
use crate::update::{self, Version};
use crate::widgets;

/// The changelog, as of this build.
pub const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// Where the whole changelog can be read.
pub fn changelog_url() -> String {
    format!("https://github.com/{}/blob/main/CHANGELOG.md", update::repo())
}

/// One `## [...]` section of the changelog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// `0.1.1`, or `Unreleased`.
    pub name: String,
    pub date: Option<String>,
    /// The paragraphs before the first group.
    pub intro: Vec<String>,
    pub groups: Vec<Group>,
}

/// `### Added` and its entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    pub items: Vec<String>,
}

impl Release {
    pub fn version(&self) -> Option<Version> {
        Version::parse_tag(&format!("v{}", self.name))
    }

    pub fn is_empty(&self) -> bool {
        self.intro.is_empty() && self.groups.iter().all(|g| g.items.is_empty())
    }
}

/// Every `## [...]` section, in the order of the file (newest first).
pub fn parse(text: &str) -> Vec<Release> {
    let mut out: Vec<Release> = Vec::new();
    // Inside a `## [...]` section (not the preamble, not "## Before ...").
    let mut inside = false;
    let mut paragraph = String::new();
    let flush = |out: &mut Vec<Release>, paragraph: &mut String| {
        let text = paragraph.trim().to_string();
        paragraph.clear();
        let Some(release) = out.last_mut() else { return };
        if text.is_empty() {
            return;
        }
        match release.groups.last_mut() {
            Some(group) => group.items.push(text),
            None => release.intro.push(text),
        }
    };
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            flush(&mut out, &mut paragraph);
            inside = false;
            let Some(rest) = head.strip_prefix('[') else { continue };
            let Some((name, after)) = rest.split_once(']') else { continue };
            let date = after.trim().trim_start_matches(['-', '–']).trim();
            out.push(Release {
                name: name.trim().to_string(),
                date: (!date.is_empty()).then(|| date.to_string()),
                intro: Vec::new(),
                groups: Vec::new(),
            });
            inside = true;
        } else if !inside || line.starts_with('[') && line.contains("]: ") {
            // The preamble, a section that is not a version, and the link
            // definitions at the end.
        } else if let Some(title) = line.strip_prefix("### ") {
            flush(&mut out, &mut paragraph);
            if let Some(release) = out.last_mut() {
                release.groups.push(Group { title: title.trim().to_string(), items: Vec::new() });
            }
        } else if let Some(item) = line.strip_prefix("- ") {
            flush(&mut out, &mut paragraph);
            paragraph.push_str(item.trim());
        } else if line.trim().is_empty() {
            flush(&mut out, &mut paragraph);
        } else {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(line.trim());
        }
    }
    flush(&mut out, &mut paragraph);
    out
}

/// The released versions newer than `after` and up to `upto`, newest
/// first. With no `after`, only `upto` itself.
pub fn between(releases: &[Release], after: Option<Version>, upto: Version) -> Vec<Release> {
    releases
        .iter()
        .filter(|r| match r.version() {
            Some(v) => v <= upto && after.map_or(v == upto, |a| v > a),
            None => false,
        })
        .cloned()
        .collect()
}

/// What to show after a start: the versions since the one last seen, when
/// this one is newer. `seen` is `None` when no version was ever recorded:
/// then `existing_user` (state saved by an older Skiff) means the app was
/// updated from a version that did not record it, and this version's
/// entries are shown; a first start shows nothing.
pub fn after_update(releases: &[Release], seen: Option<&str>, existing_user: bool, current: Version) -> Vec<Release> {
    match seen.and_then(Version::parse_lenient) {
        Some(seen) if seen < current => between(releases, Some(seen), current),
        Some(_) => Vec::new(),
        None if existing_user && seen.is_none() => between(releases, None, current),
        None => Vec::new(),
    }
}

/// The text of an entry with `**bold**` and `` `code` `` drawn as such and
/// `[text](url)` reduced to its text.
fn entry_job(text: &str, pal: &Palette, size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat { font_id: FontId::proportional(size), color: pal.text, ..Default::default() };
    let bold = TextFormat { font_id: FontId::new(size, theme::semibold()), ..plain.clone() };
    let code = TextFormat { font_id: FontId::monospace(size - 1.0), color: pal.text, background: pal.raised, ..plain.clone() };
    let text = strip_links(text);
    let (mut in_bold, mut in_code) = (false, false);
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let next = if in_code { rest.find('`') } else { [rest.find('`'), rest.find("**")].into_iter().flatten().min() };
        let end = next.unwrap_or(rest.len());
        if end > 0 {
            let format = if in_code { &code } else if in_bold { &bold } else { &plain };
            job.append(&rest[..end], 0.0, format.clone());
        }
        rest = &rest[end..];
        if let Some(r) = rest.strip_prefix('`') {
            in_code = !in_code;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("**") {
            in_bold = !in_bold;
            rest = r;
        }
    }
    job
}

/// `[text](url)` becomes `text`.
fn strip_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| after[mid + 2..].find(')').map(|close| (mid, mid + 2 + close))) {
            Some((mid, close)) if !after[..mid].contains(']') => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..mid]);
                rest = &after[close + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The window. Returns true when it is to close.
pub fn show(ctx: &egui::Context, pal: &Palette, releases: &[Release]) -> bool {
    let mut close = false;
    let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(egui::Margin::symmetric(18, 14));
    let resp = egui::Modal::new(egui::Id::unique("whats-new")).frame(frame).show(ctx, |ui| {
        let screen = ui.ctx().content_rect();
        ui.set_width(560.0_f32.min(screen.width() - 48.0));
        ui.horizontal(|ui| {
            ui.label(widgets::icon(icons::ICON_NEW_RELEASES, 18.0).color(pal.accent));
            let title = match releases.first() {
                Some(r) if releases.len() == 1 && r.version().is_some() => format!("What's new in Skiff {}", r.name),
                _ => format!("What's new in Skiff {}", update::current_version()),
            };
            ui.label(widgets::semibold(title, 15.0).color(pal.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, icons::ICON_CLOSE, "Close").clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(6.0);
        let room = (screen.height() - 170.0).max(160.0);
        egui::ScrollArea::vertical().max_height(room).show(ui, |ui| {
            if releases.is_empty() {
                ui.label(RichText::new("The changelog has no entries for this version.").color(pal.muted));
            }
            for (i, release) in releases.iter().enumerate() {
                if releases.len() > 1 || release.date.is_some() {
                    if i > 0 {
                        ui.add_space(10.0);
                    }
                    let head = match (release.version(), &release.date) {
                        (None, _) => "In nightly builds, not released yet".to_string(),
                        // (The title names the version already.)
                        (Some(_), Some(date)) if releases.len() == 1 => format!("Released {date}"),
                        (Some(_), Some(date)) => format!("Skiff {}  ·  {date}", release.name),
                        (Some(_), None) => format!("Skiff {}", release.name),
                    };
                    ui.label(widgets::semibold(head, 13.0).color(pal.muted));
                }
                for para in &release.intro {
                    ui.add(egui::Label::new(entry_job(para, pal, 13.0)).wrap());
                    ui.add_space(4.0);
                }
                for group in release.groups.iter().filter(|g| !g.items.is_empty()) {
                    ui.add_space(6.0);
                    ui.label(widgets::semibold(&group.title, 13.5).color(pal.accent));
                    ui.add_space(2.0);
                    for item in &group.items {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(RichText::new("•").color(pal.muted));
                            ui.add(egui::Label::new(entry_job(item, pal, 13.0)).wrap());
                        });
                        ui.add_space(3.0);
                    }
                }
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.link("Full changelog").clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(changelog_url()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    });
    close || resp.should_close()
}

/// The notes of a release on GitHub, shortened for a tooltip: the part
/// before the downloads table, one line per heading and per entry (an
/// entry's wrapped lines joined, cut after its first sentence), without
/// Markdown marks.
pub fn notes_summary(notes: &str, max_lines: usize) -> String {
    let clean = |t: &str| strip_links(t).replace("**", "").replace('`', "");
    let mut lines: Vec<String> = Vec::new();
    // The entry being read, until a blank line, heading or the next one.
    let mut entry: Option<String> = None;
    let finish = |entry: &mut Option<String>, lines: &mut Vec<String>| {
        if let Some(text) = entry.take() {
            let text = clean(&text);
            let first = match text.find(". ") {
                Some(i) => &text[..=i],
                None => text.as_str(),
            };
            let short: String = first.chars().take(110).collect();
            let cut = if short.len() < first.len() { "…" } else { "" };
            lines.push(format!("• {short}{cut}"));
        }
    };
    for line in notes.lines() {
        let t = line.trim();
        if t.starts_with("## Downloads") || t.starts_with("## Is it signed") {
            break;
        }
        if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            finish(&mut entry, &mut lines);
            entry = Some(item.to_string());
        } else if t.is_empty() || t.starts_with('#') || t.starts_with("<!--") {
            finish(&mut entry, &mut lines);
            let heading = t.trim_start_matches('#').trim();
            if t.starts_with('#') && heading != "What's new" && !heading.is_empty() {
                lines.push(clean(heading));
            }
        } else if let Some(text) = &mut entry {
            text.push(' ');
            text.push_str(t);
        } else {
            lines.push(clean(t));
        }
    }
    finish(&mut entry, &mut lines);
    let more = lines.len() > max_lines;
    lines.truncate(max_lines);
    if more {
        lines.push("…".to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Changelog\n\nIntro text.\n\n## [Unreleased]\n\n### Added\n\n- Next thing.\n\n\
        ## [0.2.0] - 2026-12-01\n\n### Fixed\n\n- **Bold** fix with `code` and a\n  [link](https://x) that wraps.\n- Second.\n\n\
        ## [0.1.1] - 2026-11-25\n\n### Added\n\n- Column filters.\n\n\
        ## [0.1.0] - 2026-11-24\n\nThe first stable release.\n\n### Added\n\n- Everything.\n\n\
        ## Before 0.1.0\n\nOld builds.\n\n[Unreleased]: https://example/compare\n[0.1.0]: https://example/tag\n";

    fn v(s: &str) -> Version {
        Version::parse_lenient(s).unwrap()
    }

    #[test]
    fn sections_groups_and_items_are_read() {
        let all = parse(SAMPLE);
        let names: Vec<&str> = all.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Unreleased", "0.2.0", "0.1.1", "0.1.0"]);
        let r = &all[1];
        assert_eq!(r.date.as_deref(), Some("2026-12-01"));
        assert_eq!(r.groups[0].title, "Fixed");
        assert_eq!(r.groups[0].items, ["**Bold** fix with `code` and a [link](https://x) that wraps.", "Second."]);
        assert_eq!(all[3].intro, ["The first stable release."]);
        assert_eq!(all[3].groups[0].items, ["Everything."]);
    }

    #[test]
    fn the_real_changelog_has_this_version() {
        let all = parse(CHANGELOG);
        let current = update::current_version();
        let mine = between(&all, None, current);
        assert_eq!(mine.len(), 1, "CHANGELOG.md has no ## [{current}] section");
        assert!(!mine[0].is_empty());
    }

    #[test]
    fn an_update_shows_every_version_since_the_last_one_seen() {
        let all = parse(SAMPLE);
        let names = |list: Vec<Release>| list.into_iter().map(|r| r.name).collect::<Vec<_>>();
        assert_eq!(names(after_update(&all, Some("0.1.0"), true, v("0.2.0"))), ["0.2.0", "0.1.1"]);
        assert_eq!(names(after_update(&all, Some("0.1.1"), true, v("0.2.0"))), ["0.2.0"]);
        // Same version, or going back: nothing.
        assert!(after_update(&all, Some("0.2.0"), true, v("0.2.0")).is_empty());
        assert!(after_update(&all, Some("0.2.0"), true, v("0.1.1")).is_empty());
        // Updated from a Skiff that did not record what it showed.
        assert_eq!(names(after_update(&all, None, true, v("0.1.1"))), ["0.1.1"]);
        // A first start shows nothing.
        assert!(after_update(&all, None, false, v("0.1.1")).is_empty());
    }

    #[test]
    fn links_lose_their_target() {
        assert_eq!(strip_links("see [the docs](https://x/y) and [x]"), "see the docs and [x]");
    }

    #[test]
    fn markdown_marks_become_formatting() {
        let job = entry_job("**Bold** and `code`", &Palette::dark(), 13.0);
        assert_eq!(job.text, "Bold and code");
        assert_eq!(job.sections.len(), 3);
    }

    #[test]
    fn notes_summary_stops_at_the_downloads() {
        let notes = "## What's new\n\n#### Fixed\n\n- **Dock** icon shows the\n  boat. More words here.\n- `x` too.\n\n## Downloads\n\n| a | b |";
        assert_eq!(notes_summary(notes, 10), "Fixed\n• Dock icon shows the boat.\n• x too.");
        assert_eq!(notes_summary("a\nb\nc", 2), "a\nb\n…");
    }
}
