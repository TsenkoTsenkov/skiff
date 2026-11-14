//! Command palette: jump to any resource type, object, namespace, cluster
//! or action with fuzzy search.

use std::sync::Arc;

use egui::{Align2, CornerRadius, FontId, Key, Modifiers, RichText, Sense, Vec2};
use egui_material_icons::{icons, MaterialIcon};
use skiff_core::kinds;
use skiff_core::{Contexts, ResourceKey, Session, BUILTINS};

use crate::app::{kind_title, View};
use crate::theme::{self, Palette};

#[derive(Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
    last_query: String,
}

#[derive(Clone, Debug)]
pub enum Command {
    Go(View),
    Context(String),
    /// `None` clears the namespace filter.
    Namespace(Option<String>),
    Object { key: ResourceKey, ns: String, name: String },
    ToggleTheme,
    ToggleSidebar,
    PortForwards,
    CreateResource,
    Reconnect,
}

pub enum Outcome {
    Open,
    Close,
    Run(Command),
}

struct Item {
    icon: MaterialIcon,
    title: String,
    subtitle: String,
    cmd: Command,
    score: i32,
}

/// Case-insensitive subsequence match. Higher is better; `None` = no match.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<u8> = query.bytes().filter(|b| *b != b' ').map(|b| b.to_ascii_lowercase()).collect();
    let t = text.as_bytes();
    let mut score = 0i32;
    let mut qi = 0;
    let mut prev_match: Option<usize> = None;
    for (ti, &raw) in t.iter().enumerate() {
        if qi == q.len() {
            break;
        }
        if raw.to_ascii_lowercase() != q[qi] {
            continue;
        }
        let at_word_start = ti == 0 || matches!(t[ti - 1], b'-' | b'.' | b'/' | b' ' | b'_' | b':');
        score += 10;
        if at_word_start {
            score += 14;
        }
        match prev_match {
            Some(p) if p + 1 == ti => score += 12,
            Some(p) => score -= ((ti - p - 1) as i32).min(8),
            None => score -= (ti as i32).min(10),
        }
        prev_match = Some(ti);
        qi += 1;
    }
    if qi < q.len() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let plain: String = query.to_ascii_lowercase();
    if lower == plain {
        score += 120;
    } else if lower.starts_with(&plain) {
        score += 60;
    } else if lower.contains(&plain) {
        score += 30;
    }
    Some(score - (t.len() as i32) / 6)
}

fn kind_icon_for(key: &ResourceKey) -> MaterialIcon {
    if key.is("", "pods") {
        icons::ICON_DEPLOYED_CODE
    } else {
        icons::ICON_TABLE_ROWS
    }
}

fn collect(query: &str, contexts: &Contexts, active: Option<&str>, session: Option<&Arc<Session>>) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let mut push = |icon, title: String, subtitle: String, cmd, bonus: i32, extra_terms: &[&str]| {
        let mut best = fuzzy(query, &title);
        for term in extra_terms {
            if let Some(s) = fuzzy(query, term) {
                best = Some(best.map_or(s, |b| b.max(s)));
            }
        }
        if let Some(score) = best {
            items.push(Item { icon, title, subtitle, cmd, score: score + bonus });
        }
    };

    push(icons::ICON_DASHBOARD, "Overview".into(), "View".into(), Command::Go(View::Overview), 20, &[]);
    for b in BUILTINS {
        let key = b.key();
        push(
            icons::ICON_TABLE_ROWS,
            kind_title(&key),
            b.category.title().to_string(),
            Command::Go(View::Resource(key)),
            20,
            b.aliases,
        );
    }
    if let Some(s) = session {
        for d in s.discovered().iter() {
            if kinds::builtin(&d.key.group, &d.key.plural).is_some() {
                continue;
            }
            push(
                icons::ICON_DATA_OBJECT,
                d.key.kind.to_string(),
                if d.key.group.is_empty() { "core".to_string() } else { d.key.group.to_string() },
                Command::Go(View::Resource(d.key.clone())),
                10,
                &[&d.key.plural],
            );
        }
        push(
            icons::ICON_FOLDER,
            "All namespaces".into(),
            "Namespace filter".into(),
            Command::Namespace(None),
            5,
            &[],
        );
        for row in s.table(&kinds::namespaces()).snapshot().rows.iter() {
            push(
                icons::ICON_FOLDER,
                row.name.to_string(),
                "Namespace".into(),
                Command::Namespace(Some(row.name.to_string())),
                0,
                &[],
            );
        }
    }
    for c in &contexts.items {
        if Some(c.name.as_str()) == active {
            continue;
        }
        push(icons::ICON_HUB, c.name.clone(), "Switch cluster".into(), Command::Context(c.name.clone()), 5, &["cluster", "context"]);
    }
    push(icons::ICON_DARK_MODE, "Toggle light / dark theme".into(), "Action".into(), Command::ToggleTheme, 0, &[]);
    push(icons::ICON_MENU, "Toggle sidebar".into(), "Action".into(), Command::ToggleSidebar, 0, &[]);
    push(icons::ICON_SWAP_HORIZ, "Port forwards".into(), "Action".into(), Command::PortForwards, 0, &[]);
    push(icons::ICON_ADD, "Create resource from YAML".into(), "Action".into(), Command::CreateResource, 0, &["apply", "new"]);
    push(icons::ICON_REFRESH, "Reconnect to cluster".into(), "Action".into(), Command::Reconnect, 0, &[]);

    // Objects in tables that are already loaded: instant, no extra requests.
    if let (Some(s), true) = (session, query.len() >= 2) {
        let mut found: Vec<Item> = Vec::new();
        for store in s.active_tables() {
            if store.scope.is_some() || store.key.is("", "events") || store.key.is("", "namespaces") {
                continue;
            }
            let snap = store.peek();
            let title = kind_title(&store.key);
            for row in snap.rows.iter() {
                if let Some(score) = fuzzy(query, &row.name) {
                    found.push(Item {
                        icon: kind_icon_for(&store.key),
                        title: row.name.to_string(),
                        subtitle: if row.namespace.is_empty() {
                            title.clone()
                        } else {
                            format!("{} · {}", title, row.namespace)
                        },
                        cmd: Command::Object {
                            key: store.key.clone(),
                            ns: row.namespace.to_string(),
                            name: row.name.to_string(),
                        },
                        score: score - 15,
                    });
                }
            }
        }
        found.sort_by(|a, b| b.score.cmp(&a.score));
        found.truncate(40);
        items.extend(found);
    }

    items.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
    items.truncate(60);
    items
}

pub fn show(
    ctx: &egui::Context,
    pal: &Palette,
    state: &mut PaletteState,
    contexts: &Contexts,
    active: Option<&str>,
    session: Option<&Arc<Session>>,
) -> Outcome {
    if state.query != state.last_query {
        state.last_query = state.query.clone();
        state.selected = 0;
    }
    let items = collect(state.query.trim(), contexts, active, session);
    let mut moved = false;
    ctx.input_mut(|i| {
        if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
            state.selected += 1;
            moved = true;
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
            state.selected = state.selected.saturating_sub(1);
            moved = true;
        }
    });
    state.selected = state.selected.min(items.len().saturating_sub(1));
    let enter = ctx.input(|i| i.key_pressed(Key::Enter));

    let mut chosen: Option<usize> = None;
    let resp = egui::Modal::new(egui::Id::unique("command-palette"))
        .area(
            egui::Modal::default_area(egui::Id::unique("command-palette-area"))
                .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 90.0)),
        )
        .show(ctx, |ui| {
            ui.set_width(580.0);
            let edit = ui.add(
                egui::TextEdit::singleline(&mut state.query)
                    .hint_text("Search resources, namespaces, clusters, actions…")
                    .font(FontId::proportional(15.0))
                    .desired_width(f32::INFINITY)
                    .frame(egui::Frame::NONE)
                    .margin(egui::Margin::symmetric(6, 8)),
            );
            edit.request_focus();
            ui.separator();
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                if items.is_empty() {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| ui.label(RichText::new("No matches").color(pal.muted)));
                    ui.add_space(12.0);
                }
                for (i, item) in items.iter().enumerate() {
                    let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
                    let selected = i == state.selected;
                    if ui.is_rect_visible(rect) {
                        let p = ui.painter();
                        if selected {
                            p.rect_filled(rect, CornerRadius::same(6), pal.selected);
                        } else if r.hovered() {
                            p.rect_filled(rect, CornerRadius::same(6), pal.hover);
                        }
                        p.text(
                            egui::pos2(rect.left() + 10.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            item.icon.codepoint,
                            FontId::new(16.0, item.icon.font_family()),
                            if selected { pal.accent } else { pal.faint },
                        );
                        let sub = p.layout_no_wrap(item.subtitle.clone(), FontId::proportional(12.0), pal.muted);
                        let sub_w = sub.size().x;
                        p.galley(
                            egui::pos2(rect.right() - 10.0 - sub_w, rect.center().y - sub.size().y / 2.0),
                            sub,
                            pal.muted,
                        );
                        let title_rect = egui::Rect::from_min_max(
                            egui::pos2(rect.left() + 36.0, rect.top()),
                            egui::pos2(rect.right() - 20.0 - sub_w, rect.bottom()),
                        );
                        p.with_clip_rect(title_rect).text(
                            egui::pos2(title_rect.left(), rect.center().y),
                            Align2::LEFT_CENTER,
                            &item.title,
                            FontId::proportional(13.5),
                            pal.text,
                        );
                    }
                    let label = item.title.clone();
                    r.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
                    if selected && moved {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() {
                        chosen = Some(i);
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let hint = |t: &str| RichText::new(t).color(pal.faint).font(theme::mono(11.0));
                ui.label(hint("↑↓ navigate"));
                ui.label(hint("enter open"));
                ui.label(hint("esc close"));
            });
        });
    if enter && !items.is_empty() {
        chosen = Some(state.selected);
    }
    if let Some(i) = chosen {
        if let Some(item) = items.into_iter().nth(i) {
            return Outcome::Run(item.cmd);
        }
    }
    if resp.should_close() {
        return Outcome::Close;
    }
    Outcome::Open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ranking() {
        assert!(fuzzy("pod", "Pods").is_some());
        assert!(fuzzy("xyz", "Pods").is_none());
        // Exact and prefix matches beat scattered ones.
        let exact = fuzzy("pods", "Pods").unwrap();
        let prefix = fuzzy("pod", "Pod Disruption Budgets").unwrap();
        let scattered = fuzzy("pod", "Priority Classes Of Doom").unwrap_or(i32::MIN);
        assert!(exact > prefix, "{exact} vs {prefix}");
        assert!(prefix > scattered);
        // Word-start matches beat mid-word matches.
        assert!(fuzzy("cm", "config-map").unwrap() > fuzzy("cm", "document").unwrap_or(i32::MIN));
        assert_eq!(fuzzy("", "anything"), Some(0));
    }

    #[test]
    fn alias_finds_kind() {
        let items = collect("deploy", &Contexts::default(), None, None);
        assert_eq!(items[0].title, "Deployments");
        let items = collect("svc", &Contexts::default(), None, None);
        assert_eq!(items[0].title, "Services");
        let items = collect("", &Contexts::default(), None, None);
        assert!(items.len() > 10);
    }
}
