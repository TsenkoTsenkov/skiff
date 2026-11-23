//! A small time-series chart, drawn with egui's painter.
//!
//! Filled lines for usage, dashed lines for references (requests, limits),
//! a legend with the latest values, axis ticks on round numbers, and a
//! crosshair that reads out every series under the pointer. Everything that
//! decides what is drawn (ticks, units, decimation, the value range) is a
//! plain function, tested below; [`show`] only paints.

use egui::{Align2, Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use skiff_core::prom::{local_time, PlotSeries, Unit};

use crate::theme::Palette;

/// Height of the plot itself, without legend and time axis.
const PLOT_HEIGHT: f32 = 104.0;
/// Usage series drawn in one chart; the busiest ones win.
const MAX_LINES: usize = 12;
/// The value axis is at least this wide, so that the plots of charts
/// stacked above each other start at the same place.
const MIN_GUTTER: f32 = 46.0;
/// Legend entries before "+N more".
const MAX_LEGEND: usize = 6;
const MAX_TOOLTIP_ROWS: usize = 10;
const LABEL_SIZE: f32 = 10.5;

/// What a chart shows instead of data.
pub enum State<'a> {
    Loading,
    /// The message is shown as is.
    Error(&'a str),
    /// Nothing in this time range; the text says what that means here.
    Empty(&'a str),
    Data(&'a [PlotSeries]),
}

// ---------------------------------------------------------------------------
// Numbers

/// Round step sizes for an axis of this unit: powers of two for bytes
/// (so labels are whole Mi and Gi), 1-2-5 for everything else.
fn nice_step(raw: f64, unit: Unit) -> f64 {
    let floor = match unit {
        // A millicore, a byte, a byte per second, one event.
        Unit::Cores => 0.001,
        Unit::Bytes | Unit::BytesPerSec | Unit::Count => 1.0,
    };
    // (Also for NaN: no data, no step.)
    if raw.is_nan() || raw <= floor {
        return floor;
    }
    match unit {
        Unit::Bytes | Unit::BytesPerSec => 2f64.powf(raw.log2().ceil()),
        Unit::Cores | Unit::Count => {
            let magnitude = 10f64.powf(raw.log10().floor());
            let lead = raw / magnitude;
            // (Tolerances keep 0.2 / 0.1 from landing just above 2.)
            let nice = if lead <= 1.0 + 1e-9 {
                1.0
            } else if lead <= 2.0 + 1e-9 {
                2.0
            } else if lead <= 5.0 + 1e-9 {
                5.0
            } else {
                10.0
            };
            nice * magnitude
        }
    }
}

/// Tick values for a value axis that has to show `max`: from zero, in
/// round steps, two to four intervals. The last tick is the top of the
/// axis.
pub fn value_ticks(max: f64, unit: Unit) -> Vec<f64> {
    let max = if max.is_finite() && max > 0.0 { max } else { 0.0 };
    let step = nice_step(max / 4.0, unit);
    // An all-zero chart still gets an axis with one step on it.
    let intervals = ((max / step) - 1e-9).ceil().max(1.0) as usize;
    (0..=intervals).map(|i| i as f64 * step).collect()
}

fn trim_zeros(text: String) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// A number with at most `decimals` decimals and no trailing zeros.
fn compact(value: f64, decimals: usize) -> String {
    trim_zeros(format!("{value:.decimals$}"))
}

fn bytes_text(bytes: f64) -> String {
    const UNITS: [(&str, f64); 4] = [
        ("Ti", 1024.0 * 1024.0 * 1024.0 * 1024.0),
        ("Gi", 1024.0 * 1024.0 * 1024.0),
        ("Mi", 1024.0 * 1024.0),
        ("Ki", 1024.0),
    ];
    for (suffix, size) in UNITS {
        if bytes >= size {
            let n = bytes / size;
            return format!(
                "{}{suffix}",
                compact(
                    n,
                    if n >= 100.0 {
                        0
                    } else if n >= 10.0 {
                        1
                    } else {
                        2
                    }
                )
            );
        }
    }
    compact(bytes, 0)
}

/// A value as it is written next to a series: "250m", "1.5 cores",
/// "312Mi", "1.2Mi/s", "3".
pub fn format_value(value: f64, unit: Unit) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    match unit {
        Unit::Cores => {
            if value < 0.9995 {
                let m = value * 1000.0;
                format!("{}m", compact(m, if m < 10.0 { 1 } else { 0 }))
            } else {
                let cores = compact(value, if value < 10.0 { 2 } else { 1 });
                format!("{cores} {}", if cores == "1" { "core" } else { "cores" })
            }
        }
        Unit::Bytes => bytes_text(value),
        Unit::BytesPerSec => format!("{}/s", bytes_text(value)),
        Unit::Count => {
            if value >= 10_000.0 {
                format!("{}k", compact(value / 1000.0, 1))
            } else {
                // Rates of events are fractional between samples.
                compact(value, if value.fract().abs() < 0.005 { 0 } else { 2 })
            }
        }
    }
}

/// A tick label. `top` is the axis' last tick: it decides whether a CPU
/// axis counts in millicores or cores, so that one axis has one unit.
pub fn format_tick(value: f64, top: f64, unit: Unit) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    match unit {
        Unit::Cores if top < 1.0 => format!("{}m", compact(value * 1000.0, 1)),
        Unit::Cores => compact(value, 2),
        Unit::Bytes => bytes_text(value),
        Unit::BytesPerSec => format!("{}/s", bytes_text(value)),
        Unit::Count if value >= 10_000.0 => format!("{}k", compact(value / 1000.0, 1)),
        Unit::Count => compact(value, 0),
    }
}

// ---------------------------------------------------------------------------
// Time

const TIME_STEPS: &[i64] = &[
    60,
    120,
    300,
    600,
    900,
    1800,
    3600,
    7200,
    10_800,
    21_600,
    43_200,
    86_400,
    2 * 86_400,
    7 * 86_400,
];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Where the time axis gets labels: round local times (`utc_offset` is the
/// zone's offset in seconds), at most `max_labels` of them.
pub fn time_ticks(start: i64, end: i64, utc_offset: i32, max_labels: usize) -> (i64, Vec<i64>) {
    let span = (end - start).max(1);
    let wanted = max_labels.max(2) as i64;
    let step = TIME_STEPS
        .iter()
        .copied()
        .find(|s| span / s <= wanted)
        // Longer than the table goes: whole days.
        .unwrap_or_else(|| (span / wanted / 86_400 + 1) * 86_400);
    let offset = i64::from(utc_offset);
    // Multiples of the step on the local clock: 3-hour ticks land on
    // 0:00, 3:00, ... where the user is, not in UTC.
    let mut t = (start + offset).div_euclid(step) * step - offset;
    if t < start {
        t += step;
    }
    let mut ticks = Vec::new();
    while t <= end && ticks.len() < 64 {
        ticks.push(t);
        t += step;
    }
    (step, ticks)
}

fn clock(unix: i64) -> String {
    let t = local_time(unix);
    format!("{:02}:{:02}", t.hour, t.minute)
}

fn date(unix: i64) -> String {
    let t = local_time(unix);
    format!("{} {}", MONTHS[usize::from(t.month.clamp(1, 12)) - 1], t.day)
}

/// The label of a time tick: the time of day, or the date for ticks a
/// day apart and for the tick at midnight.
fn tick_label(unix: i64, step: i64) -> String {
    let t = local_time(unix);
    if step >= 86_400 || (t.hour == 0 && t.minute == 0) {
        date(unix)
    } else {
        clock(unix)
    }
}

/// The time under the crosshair: with the date when the chart spans more
/// than a day.
fn moment(unix: i64, span: i64) -> String {
    if span > 86_400 {
        format!("{} {}", date(unix), clock(unix))
    } else {
        clock(unix)
    }
}

// ---------------------------------------------------------------------------
// Series

/// Reduces a series to what `columns` pixel columns can show: the lowest
/// and the highest sample of every column, in time order. Spikes survive;
/// a week of samples costs no more to draw than an hour.
pub fn decimate(points: &[(f64, f64)], start: f64, end: f64, columns: usize) -> Vec<(f64, f64)> {
    if columns == 0 || points.len() <= columns * 2 || end.is_nan() || end <= start {
        return points.to_vec();
    }
    let column = |t: f64| {
        (((t - start) / (end - start)) * columns as f64)
            .floor()
            .clamp(0.0, columns as f64 - 1.0) as usize
    };
    let mut out = Vec::with_capacity(columns * 2);
    let mut i = 0;
    while i < points.len() {
        let col = column(points[i].0);
        let (mut lo, mut hi) = (points[i], points[i]);
        while i < points.len() && column(points[i].0) == col {
            if points[i].1 < lo.1 {
                lo = points[i];
            }
            if points[i].1 > hi.1 {
                hi = points[i];
            }
            i += 1;
        }
        if lo.0 <= hi.0 {
            out.push(lo);
            if hi != lo {
                out.push(hi);
            }
        } else {
            out.push(hi);
            out.push(lo);
        }
    }
    out
}

/// The usual distance between two samples of a series (its median), used
/// to tell a gap from the next sample.
fn sample_spacing(points: &[(f64, f64)]) -> f64 {
    let mut gaps: Vec<f64> = points
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .filter(|d| *d > 0.0)
        .collect();
    if gaps.is_empty() {
        return f64::INFINITY;
    }
    gaps.sort_by(f64::total_cmp);
    gaps[gaps.len() / 2]
}

/// Splits a series where samples are missing, so a line is not drawn
/// across the time a pod did not exist.
pub fn segments(points: &[(f64, f64)]) -> Vec<&[(f64, f64)]> {
    let limit = sample_spacing(points) * 1.75;
    let mut out = Vec::new();
    let mut from = 0;
    for i in 1..points.len() {
        if points[i].0 - points[i - 1].0 > limit {
            out.push(&points[from..i]);
            from = i;
        }
    }
    if from < points.len() {
        out.push(&points[from..]);
    }
    out
}

/// The sample of a series closest to `t`, if one is within `reach` seconds.
pub fn nearest(points: &[(f64, f64)], t: f64, reach: f64) -> Option<(f64, f64)> {
    let i = points.partition_point(|p| p.0 < t);
    [i.checked_sub(1), Some(i)]
        .into_iter()
        .flatten()
        .filter_map(|i| points.get(i).copied())
        .min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs()))
        .filter(|p| (p.0 - t).abs() <= reach)
}

fn peak(points: &[(f64, f64)]) -> f64 {
    points.iter().map(|p| p.1).fold(0.0, f64::max)
}

/// The highest value the axis has to reach. Usage always fits. A
/// reference line (a limit far above what is used) is left off the scale
/// when it would squash the usage into the bottom of the chart; its value
/// is still in the legend.
pub fn axis_max(series: &[PlotSeries]) -> f64 {
    let usage = series
        .iter()
        .filter(|s| !s.reference)
        .map(|s| peak(&s.points))
        .fold(0.0, f64::max);
    let mut max = usage;
    for s in series.iter().filter(|s| s.reference) {
        let value = peak(&s.points);
        if usage <= 0.0 || value <= usage * 3.0 {
            max = max.max(value);
        }
    }
    max
}

/// What several names share at the start, up to and including the last
/// `-` (the workload and ReplicaSet part of pod names), so a legend can
/// show only what differs.
pub fn common_prefix(names: &[&str]) -> usize {
    let Some(first) = names.first() else { return 0 };
    if names.len() < 2 {
        return 0;
    }
    let mut len = first.len();
    for name in &names[1..] {
        len = first
            .bytes()
            .zip(name.bytes())
            .take(len)
            .take_while(|(a, b)| a == b)
            .count();
    }
    // (Names come from Prometheus labels: do not assume ASCII.)
    let cut = first[..first.floor_char_boundary(len)]
        .rfind('-')
        .map(|i| i + 1)
        .unwrap_or(0);
    // Never shorten a name to nothing.
    if names.iter().any(|n| n.len() <= cut) {
        0
    } else {
        cut
    }
}

/// Characters of a series name shown in the legend.
const MAX_LEGEND_NAME: usize = 32;
/// Characters of a series name shown in the readout under the crosshair
/// and in hover texts.
const MAX_READOUT_NAME: usize = 64;

/// `name` in at most `max` characters: the start and the end with `…`
/// between them, as the end is what tells pod names apart. Series names
/// are label values, which can be as long as whoever wrote them liked.
pub fn clip_name(name: &str, max: usize) -> std::borrow::Cow<'_, str> {
    let count = name.chars().count();
    if count <= max.max(3) {
        return name.into();
    }
    let keep = max.max(3) - 1;
    let (head, tail) = (keep / 2, keep - keep / 2);
    let head_end = name.char_indices().nth(head).map_or(name.len(), |(i, _)| i);
    let tail_start = name.char_indices().nth(count - tail).map_or(name.len(), |(i, _)| i);
    format!("{}…{}", &name[..head_end], &name[tail_start..]).into()
}

// ---------------------------------------------------------------------------
// Painting

struct Line<'a> {
    name: &'a str,
    /// The name as the legend shows it.
    short: String,
    color: Color32,
    reference: bool,
    points: Vec<(f64, f64)>,
    /// All samples, for the readout under the crosshair.
    full: &'a [(f64, f64)],
}

fn reference_color(name: &str, pal: &Palette) -> Color32 {
    match name {
        "limit" => pal.err,
        _ => pal.muted,
    }
}

fn swatch(ui: &mut Ui, color: Color32, dashed: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 10.0), Sense::hover());
    let y = ui.painter().round_to_pixel_center(rect.center().y);
    let (a, b) = (Pos2::new(rect.left(), y), Pos2::new(rect.right(), y));
    if dashed {
        ui.painter()
            .add(Shape::dashed_line(&[a, b], Stroke::new(1.5, color), 3.0, 2.0));
    } else {
        ui.painter().line_segment([a, b], Stroke::new(2.0, color));
    }
}

fn legend(ui: &mut Ui, pal: &Palette, lines: &[Line<'_>], hidden: usize, unit: Unit) {
    let font = FontId::proportional(11.5);
    let usage: Vec<&Line<'_>> = lines.iter().filter(|l| !l.reference).collect();
    let more = usage.len().saturating_sub(MAX_LEGEND) + hidden;
    // Usage first, then how many are left out, then the references.
    let mut entries: Vec<Option<&Line<'_>>> = usage.into_iter().take(MAX_LEGEND).map(Some).collect();
    if more > 0 {
        entries.push(None);
    }
    entries.extend(lines.iter().filter(|l| l.reference).map(Some));

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 2.0);
        for (i, entry) in entries.into_iter().enumerate() {
            // Measured first: an entry never breaks across two rows.
            let (name, value) = match entry {
                Some(line) => (
                    line.short.clone(),
                    line.full.last().map(|p| format_value(p.1, unit)).unwrap_or_default(),
                ),
                None => (format!("+{more} more"), String::new()),
            };
            let name =
                ui.painter()
                    .layout_no_wrap(name, font.clone(), if entry.is_some() { pal.muted } else { pal.faint });
            let value = ui.painter().layout_no_wrap(value, font.clone(), pal.text);
            let text_width = name.size().x
                + if value.size().x > 0.0 {
                    5.0 + value.size().x
                } else {
                    0.0
                };
            let width = text_width + if entry.is_some() { 16.0 } else { 0.0 };
            if i > 0 {
                if ui.available_size_before_wrap().x < width + 10.0 {
                    ui.end_row();
                } else {
                    ui.add_space(10.0);
                }
            }
            if let Some(line) = entry {
                swatch(ui, line.color, line.reference);
            }
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(text_width, 14.0), Sense::hover());
            let y = rect.center().y;
            let name_width = name.size().x;
            ui.painter()
                .galley(Pos2::new(rect.left(), y - name.size().y / 2.0), name, pal.muted);
            ui.painter().galley(
                Pos2::new(rect.left() + name_width + 5.0, y - value.size().y / 2.0),
                value,
                pal.text,
            );
            match entry {
                Some(line) if line.short != line.name => {
                    resp.on_hover_text(clip_name(line.name, 4 * MAX_READOUT_NAME));
                }
                _ => {}
            }
        }
    });
}

fn centered(ui: &Ui, rect: Rect, text: &str, color: Color32) {
    let galley = ui
        .painter()
        .layout(text.to_owned(), FontId::proportional(12.0), color, rect.width() - 16.0);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
}

/// Draws one chart: title, legend, plot and time axis, at the available
/// width. `window` is the time range shown, in unix seconds; `id` tells
/// the charts of a view apart.
pub fn show(
    ui: &mut Ui,
    pal: &Palette,
    id: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    unit: Unit,
    window: (i64, i64),
    state: State<'_>,
) {
    let id = ui.scope_id().with(("chart", id));
    let (start, end) = (window.0 as f64, (window.1.max(window.0 + 1)) as f64);
    let series: &[PlotSeries] = match &state {
        State::Data(series) => series,
        _ => &[],
    };

    // The busiest usage series, then the references. Among those drawn
    // the order (and with it the colour) goes by name, so a series keeps
    // its colour from one refresh to the next.
    let mut usage: Vec<&PlotSeries> = series.iter().filter(|s| !s.reference && !s.points.is_empty()).collect();
    let hidden = usage.len().saturating_sub(MAX_LINES);
    if hidden > 0 {
        usage.sort_by(|a, b| {
            peak(&b.points)
                .total_cmp(&peak(&a.points))
                .then_with(|| a.name.cmp(&b.name))
        });
        usage.truncate(MAX_LINES);
    }
    usage.sort_by(|a, b| a.name.cmp(&b.name));
    let cut = common_prefix(&usage.iter().map(|s| s.name.as_str()).collect::<Vec<_>>());
    let references = series.iter().filter(|s| s.reference && !s.points.is_empty());

    ui.horizontal(|ui| {
        ui.label(crate::widgets::semibold(title, 13.0).color(pal.text));
        if matches!(state, State::Loading) {
            ui.add(egui::Spinner::new().size(12.0));
        }
    });

    // Sizes first: the value axis' labels decide where the plot starts.
    let ticks = value_ticks(axis_max(series), unit);
    let top = ticks.last().copied().unwrap_or(1.0).max(f64::MIN_POSITIVE);
    let label_font = FontId::proportional(LABEL_SIZE);
    let tick_labels: Vec<_> = ticks
        .iter()
        .map(|t| {
            ui.painter()
                .layout_no_wrap(format_tick(*t, top, unit), label_font.clone(), pal.faint)
        })
        .collect();
    let gutter = (tick_labels.iter().map(|g| g.size().x).fold(0.0, f32::max).ceil() + 6.0).max(MIN_GUTTER);

    let mut lines: Vec<Line<'_>> = Vec::new();
    let columns = (ui.available_width() - gutter).max(1.0) as usize;
    for (i, s) in usage.iter().enumerate() {
        lines.push(Line {
            name: &s.name,
            short: if cut > 0 {
                format!("…{}", clip_name(&s.name[cut..], MAX_LEGEND_NAME - 1))
            } else {
                clip_name(&s.name, MAX_LEGEND_NAME).into_owned()
            },
            color: pal.series(i),
            reference: false,
            points: decimate(&s.points, start, end, columns),
            full: &s.points,
        });
    }
    for s in references {
        lines.push(Line {
            name: &s.name,
            short: clip_name(&s.name, MAX_LEGEND_NAME).into_owned(),
            color: reference_color(&s.name, pal),
            reference: true,
            points: vec![],
            full: &s.points,
        });
    }
    if !lines.is_empty() {
        legend(ui, pal, &lines, hidden, unit);
    }

    let width = ui.available_width();
    let (outer, resp) = ui.allocate_exact_size(Vec2::new(width, PLOT_HEIGHT + 18.0), Sense::hover());
    // The chart in words (the latest value of every series), for screen
    // readers; the picture itself is only painted.
    let spoken = std::iter::once(format!("{title} chart"))
        .chain(
            lines
                .iter()
                .filter_map(|l| Some(format!("{} {}", clip_name(l.name, MAX_READOUT_NAME), format_value(l.full.last()?.1, unit)))),
        )
        .collect::<Vec<_>>()
        .join(", ");
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Image, true, &spoken));
    if !ui.is_rect_visible(outer) {
        return;
    }
    let plot = Rect::from_min_max(
        Pos2::new(outer.left() + gutter, outer.top() + 6.0),
        Pos2::new(outer.right() - 2.0, outer.top() + PLOT_HEIGHT),
    );
    let painter = ui.painter_at(outer);
    let x_of = |t: f64| plot.left() + ((t - start) / (end - start)) as f32 * plot.width();
    let y_of = |v: f64| plot.bottom() - (v / top).clamp(0.0, 1.0) as f32 * plot.height();

    // Grid and value labels.
    let grid = Stroke::new(1.0, pal.tint(pal.muted, if pal.dark { 0.16 } else { 0.2 }));
    for (value, label) in ticks.iter().zip(tick_labels) {
        let y = painter.round_to_pixel_center(y_of(*value));
        painter.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], grid);
        painter.galley(
            Pos2::new(plot.left() - 6.0 - label.size().x, y - label.size().y / 2.0),
            label,
            pal.faint,
        );
    }

    // Time labels.
    let offset = local_time(window.1).utc_offset;
    let (step, time) = time_ticks(window.0, window.1, offset, (plot.width() / 68.0) as usize);
    let mut last_right = f32::NEG_INFINITY;
    for t in time {
        let x = painter.round_to_pixel_center(x_of(t as f64));
        let label = painter.layout_no_wrap(tick_label(t, step), label_font.clone(), pal.faint);
        let left = (x - label.size().x / 2.0).clamp(plot.left(), plot.right() - label.size().x);
        painter.line_segment([Pos2::new(x, plot.bottom()), Pos2::new(x, plot.bottom() + 3.0)], grid);
        if left > last_right + 6.0 {
            last_right = left + label.size().x;
            painter.galley(Pos2::new(left, plot.bottom() + 5.0), label, pal.faint);
        }
    }

    match state {
        State::Loading if lines.is_empty() => {}
        State::Error(message) => {
            centered(ui, plot, message, pal.warn);
            return;
        }
        State::Empty(message) => {
            centered(ui, plot, message, pal.muted);
            return;
        }
        State::Data(_) if lines.iter().all(|l| l.reference) => {
            centered(ui, plot, "No data in this time range", pal.muted);
        }
        _ => {}
    }

    // Usage: a soft fill under each line while there are few enough to
    // tell apart, then the lines on top.
    let clip = painter.with_clip_rect(plot.expand2(Vec2::new(0.0, 1.0)));
    // (A lone sample sits on the plot's edge: its dot may reach over it.)
    let dots = painter.with_clip_rect(plot.expand(4.0));
    let usage_count = lines.iter().filter(|l| !l.reference).count();
    let fill_alpha = match usage_count {
        0..=1 => 0.26,
        2..=3 => 0.16,
        4..=6 => 0.08,
        _ => 0.0,
    };
    for line in lines.iter().filter(|l| !l.reference).rev() {
        for segment in segments(&line.points) {
            let points: Vec<Pos2> = segment.iter().map(|(t, v)| Pos2::new(x_of(*t), y_of(*v))).collect();
            if fill_alpha > 0.0 && points.len() > 1 {
                let mut mesh = Mesh::default();
                let top_color = line.color.gamma_multiply(fill_alpha);
                for (i, p) in points.iter().enumerate() {
                    mesh.colored_vertex(*p, top_color);
                    mesh.colored_vertex(Pos2::new(p.x, plot.bottom()), Color32::TRANSPARENT);
                    if i > 0 {
                        let n = (i * 2) as u32;
                        mesh.add_triangle(n - 2, n - 1, n);
                        mesh.add_triangle(n - 1, n, n + 1);
                    }
                }
                clip.add(Shape::mesh(mesh));
            }
            match points.as_slice() {
                // One sample (history that has only just begun): a dot.
                [p] => dots.circle_filled(*p, 2.5, line.color),
                _ => clip.add(Shape::line(points, Stroke::new(1.5, line.color))),
            };
        }
    }
    // References: dashed, at their latest value, when they are on the scale.
    for line in lines.iter().filter(|l| l.reference) {
        let Some((_, value)) = line.full.last().copied() else {
            continue;
        };
        if value > top * 1.0001 {
            continue;
        }
        let y = painter.round_to_pixel_center(y_of(value));
        clip.add(Shape::dashed_line(
            &[Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(1.0, line.color.gamma_multiply(0.9)),
            5.0,
            4.0,
        ));
    }

    // Crosshair and readout.
    let Some(pointer) = resp.hover_pos().filter(|p| plot.expand(4.0).contains(*p)) else {
        return;
    };
    let Some(anchor) = lines.iter().find(|l| !l.reference) else {
        return;
    };
    let t_pointer = start + f64::from((pointer.x - plot.left()) / plot.width()) * (end - start);
    let reach = (sample_spacing(anchor.full) * 1.5).min(end - start);
    let Some((t, _)) = nearest(anchor.full, t_pointer, f64::INFINITY) else {
        return;
    };
    let x = painter.round_to_pixel_center(x_of(t));
    painter.line_segment(
        [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
        Stroke::new(1.0, pal.faint),
    );
    let mut rows: Vec<(&Line<'_>, f64)> = Vec::new();
    for line in &lines {
        let value = if line.reference {
            line.full.last().map(|p| p.1)
        } else {
            nearest(line.full, t, reach).map(|p| p.1)
        };
        let Some(value) = value else { continue };
        if !line.reference {
            let p = Pos2::new(x_of(t), y_of(value));
            painter.circle_filled(p, 4.0, pal.raised);
            painter.circle_filled(p, 2.5, line.color);
        }
        rows.push((line, value));
    }
    // Most used first; references keep their place at the end.
    rows.sort_by(|a, b| a.0.reference.cmp(&b.0.reference).then(b.1.total_cmp(&a.1)));
    let right_half = x > plot.center().x;
    let pos = Pos2::new(if right_half { x - 10.0 } else { x + 10.0 }, plot.top() + 4.0);
    egui::Area::new(id.with("readout"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .pivot(if right_half {
            Align2::RIGHT_TOP
        } else {
            Align2::LEFT_TOP
        })
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(pal.raised)
                .stroke(Stroke::new(1.0, pal.border))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .shadow(egui::Shadow {
                    offset: [0, 2],
                    blur: 8,
                    spread: 0,
                    color: Color32::from_black_alpha(if pal.dark { 90 } else { 30 }),
                })
                .show(ui, |ui| {
                    // Painted by hand: a name column, a right-aligned
                    // value column, rows on an even pitch.
                    const ROW: f32 = 17.0;
                    let font = FontId::proportional(11.5);
                    let shown = &rows[..rows.len().min(MAX_TOOLTIP_ROWS)];
                    let time = ui.painter().layout_no_wrap(
                        moment(t as i64, window.1 - window.0),
                        FontId::proportional(11.0),
                        pal.muted,
                    );
                    let texts: Vec<_> = shown
                        .iter()
                        .map(|(line, value)| {
                            (
                                ui.painter()
                                    .layout_no_wrap(clip_name(line.name, MAX_READOUT_NAME).into_owned(), font.clone(), pal.muted),
                                ui.painter()
                                    .layout_no_wrap(format_value(*value, unit), font.clone(), pal.text),
                            )
                        })
                        .collect();
                    let more = (rows.len() > shown.len()).then(|| {
                        let text = format!("… and {} more", rows.len() - shown.len());
                        ui.painter().layout_no_wrap(text, FontId::proportional(11.0), pal.faint)
                    });
                    let names = texts.iter().map(|(n, _)| n.size().x).fold(0.0, f32::max);
                    let values = texts.iter().map(|(_, v)| v.size().x).fold(0.0, f32::max);
                    let width = (18.0 + names + 12.0 + values)
                        .max(time.size().x)
                        .max(more.as_ref().map_or(0.0, |m| m.size().x));
                    let height = ROW * (1 + shown.len() + usize::from(more.is_some())) as f32 - 2.0;
                    let (rect, readout) = ui.allocate_exact_size(Vec2::new(width.ceil(), height), Sense::hover());
                    // The same in words, for screen readers.
                    let spoken = std::iter::once(time.text().to_string())
                        .chain(
                            shown
                                .iter()
                                .map(|(line, value)| format!("{} {}", clip_name(line.name, MAX_READOUT_NAME), format_value(*value, unit))),
                        )
                        .collect::<Vec<_>>()
                        .join(" · ");
                    readout.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Label, true, &spoken));
                    let p = ui.painter();
                    let middle = |row: usize| rect.top() + ROW * row as f32 + (ROW - 2.0) / 2.0;
                    p.galley(Pos2::new(rect.left(), middle(0) - time.size().y / 2.0), time, pal.muted);
                    for (i, ((line, _), (name, value))) in shown.iter().zip(texts).enumerate() {
                        let y = middle(i + 1);
                        let line_y = p.round_to_pixel_center(y);
                        let (a, b) = (Pos2::new(rect.left(), line_y), Pos2::new(rect.left() + 12.0, line_y));
                        if line.reference {
                            p.add(Shape::dashed_line(&[a, b], Stroke::new(1.5, line.color), 3.0, 2.0));
                        } else {
                            p.line_segment([a, b], Stroke::new(2.0, line.color));
                        }
                        p.galley(Pos2::new(rect.left() + 18.0, y - name.size().y / 2.0), name, pal.muted);
                        p.galley(
                            Pos2::new(rect.right() - value.size().x, y - value.size().y / 2.0),
                            value,
                            pal.text,
                        );
                    }
                    if let Some(more) = more {
                        p.galley(
                            Pos2::new(rect.left(), middle(shown.len() + 1) - more.size().y / 2.0),
                            more,
                            pal.faint,
                        );
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MI: f64 = 1024.0 * 1024.0;
    const GI: f64 = 1024.0 * MI;

    fn labels(max: f64, unit: Unit) -> Vec<String> {
        let ticks = value_ticks(max, unit);
        let top = *ticks.last().unwrap();
        ticks.iter().map(|t| format_tick(*t, top, unit)).collect()
    }

    #[test]
    fn value_axes_use_round_steps_and_always_fit_the_data() {
        for unit in [Unit::Cores, Unit::Bytes, Unit::BytesPerSec, Unit::Count] {
            for max in [
                0.0, 0.0004, 0.003, 0.07, 0.25, 0.9, 1.0, 1.01, 3.7, 16.0, 97.0, 1500.0, 3.3e6, 7.9e9, 2.5e13,
            ] {
                let ticks = value_ticks(max, unit);
                let top = *ticks.last().unwrap();
                assert!(top >= max, "{unit:?} {max}: top {top}");
                assert!((2..=5).contains(&ticks.len()), "{unit:?} {max}: {ticks:?}");
                assert_eq!(ticks[0], 0.0);
                // Evenly spaced.
                let step = ticks[1] - ticks[0];
                assert!(
                    ticks.windows(2).all(|w| ((w[1] - w[0]) - step).abs() < step * 1e-9),
                    "{ticks:?}"
                );
            }
        }
        // Garbage in, a sane axis out.
        for max in [f64::NAN, f64::INFINITY, -4.0] {
            assert_eq!(value_ticks(max, Unit::Count), [0.0, 1.0]);
        }
    }

    #[test]
    fn cpu_axes_count_in_millicores_or_cores_never_both() {
        assert_eq!(labels(0.23, Unit::Cores), ["0", "100m", "200m", "300m"]);
        assert_eq!(labels(0.0031, Unit::Cores), ["0", "1m", "2m", "3m", "4m"]);
        assert_eq!(labels(0.9, Unit::Cores), ["0", "0.5", "1"]);
        assert_eq!(labels(1.0, Unit::Cores), ["0", "0.5", "1"]);
        assert_eq!(labels(3.7, Unit::Cores), ["0", "1", "2", "3", "4"]);
        assert_eq!(labels(16.0, Unit::Cores), ["0", "5", "10", "15", "20"]);
        assert_eq!(labels(0.0, Unit::Cores), ["0", "1m"]);
    }

    #[test]
    fn byte_axes_land_on_whole_binary_units() {
        assert_eq!(
            labels(500.0 * MI, Unit::Bytes),
            ["0", "128Mi", "256Mi", "384Mi", "512Mi"]
        );
        assert_eq!(labels(1.4 * GI, Unit::Bytes), ["0", "512Mi", "1Gi", "1.5Gi"]);
        assert_eq!(labels(64.0 * GI, Unit::Bytes), ["0", "16Gi", "32Gi", "48Gi", "64Gi"]);
        assert_eq!(labels(3000.0, Unit::BytesPerSec), ["0", "1Ki/s", "2Ki/s", "3Ki/s"]);
        assert_eq!(labels(0.0, Unit::Bytes), ["0", "1"]);
    }

    #[test]
    fn count_axes_use_whole_numbers() {
        assert_eq!(labels(1.0, Unit::Count), ["0", "1"]);
        assert_eq!(labels(3.0, Unit::Count), ["0", "1", "2", "3"]);
        assert_eq!(labels(0.2, Unit::Count), ["0", "1"]);
        assert_eq!(labels(37.0, Unit::Count), ["0", "10", "20", "30", "40"]);
        assert_eq!(labels(45_000.0, Unit::Count), ["0", "20k", "40k", "60k"]);
    }

    #[test]
    fn values_are_written_the_way_kubernetes_writes_them() {
        assert_eq!(format_value(0.25, Unit::Cores), "250m");
        assert_eq!(format_value(0.0042, Unit::Cores), "4.2m");
        assert_eq!(format_value(0.9999, Unit::Cores), "1 core");
        assert_eq!(format_value(1.0, Unit::Cores), "1 core");
        assert_eq!(format_value(1.256, Unit::Cores), "1.26 cores");
        assert_eq!(format_value(16.0, Unit::Cores), "16 cores");
        assert_eq!(format_value(0.0, Unit::Cores), "0m");
        assert_eq!(format_value(312.4 * MI, Unit::Bytes), "312Mi");
        assert_eq!(format_value(1.5 * GI, Unit::Bytes), "1.5Gi");
        assert_eq!(format_value(12.34 * GI, Unit::Bytes), "12.3Gi");
        assert_eq!(format_value(900.0, Unit::Bytes), "900");
        assert_eq!(format_value(2048.0, Unit::BytesPerSec), "2Ki/s");
        assert_eq!(format_value(3.0, Unit::Count), "3");
        assert_eq!(format_value(0.5, Unit::Count), "0.5");
        assert_eq!(format_value(f64::NAN, Unit::Count), "—");
    }

    #[test]
    fn time_ticks_are_round_on_the_local_clock() {
        // 2023-11-14 22:13:20 UTC.
        let end = 1_700_000_000;
        // One hour, room for six labels: every ten minutes.
        let (step, ticks) = time_ticks(end - 3600, end, 0, 6);
        assert_eq!(step, 600);
        assert_eq!(ticks.len(), 6);
        assert!(ticks.iter().all(|t| t % 600 == 0 && (end - 3600..=end).contains(t)));
        // A day: every six hours, on the local clock (UTC+2 here).
        let (step, ticks) = time_ticks(end - 86_400, end, 7200, 5);
        assert_eq!(step, 21_600);
        assert!(ticks.iter().all(|t| (t + 7200) % 21_600 == 0), "{ticks:?}");
        // A week in a narrow chart: every other day, at local midnight.
        let (step, ticks) = time_ticks(end - 7 * 86_400, end, -18_000, 4);
        assert_eq!(step, 2 * 86_400);
        assert!(ticks.iter().all(|t| (t - 18_000) % 86_400 == 0), "{ticks:?}");
        assert!((3..=4).contains(&ticks.len()));
        // Never more labels than asked for, whatever the range.
        for span in [60, 300, 3600, 6 * 3600, 86_400, 7 * 86_400, 90 * 86_400] {
            for max in [2usize, 3, 5, 9] {
                let (_, ticks) = time_ticks(end - span, end, 3600, max);
                assert!(ticks.len() <= max + 1, "{span} s with {max} labels: {}", ticks.len());
            }
        }
        // A degenerate window does not loop or panic.
        assert!(time_ticks(end, end, 0, 5).1.len() <= 1);
    }

    #[test]
    fn decimation_keeps_the_extremes_of_every_column() {
        let points: Vec<(f64, f64)> = (0..10_000)
            .map(|i| (i as f64, if i == 4321 { 99.0 } else { (i % 7) as f64 }))
            .collect();
        let out = decimate(&points, 0.0, 10_000.0, 200);
        assert!(out.len() <= 400, "{}", out.len());
        assert!(out.iter().any(|p| p.1 == 99.0), "the spike is kept");
        assert!(out.windows(2).all(|w| w[0].0 < w[1].0), "still in time order");
        assert_eq!(out.first().map(|p| p.0), Some(0.0));
        // Few points: untouched.
        assert_eq!(decimate(&points[..50], 0.0, 50.0, 200), &points[..50]);
        assert!(decimate(&[], 0.0, 1.0, 10).is_empty());
        assert_eq!(
            decimate(&points[..5], 3.0, 3.0, 1).len(),
            5,
            "an empty window changes nothing"
        );
    }

    #[test]
    fn lines_break_at_gaps_and_the_crosshair_snaps_to_samples() {
        let points = [(0.0, 1.0), (30.0, 1.0), (60.0, 2.0), (300.0, 3.0), (330.0, 3.0)];
        let parts = segments(&points);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 3);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(segments(&points[..1]).len(), 1);
        assert!(segments(&[]).is_empty());

        assert_eq!(nearest(&points, 40.0, 45.0), Some((30.0, 1.0)));
        assert_eq!(nearest(&points, 50.0, 45.0), Some((60.0, 2.0)));
        assert_eq!(nearest(&points, 180.0, 45.0), None, "inside the gap there is no value");
        assert_eq!(nearest(&points, -100.0, f64::INFINITY), Some((0.0, 1.0)));
        assert_eq!(nearest(&[], 1.0, 10.0), None);
    }

    fn series(name: &str, reference: bool, values: &[f64]) -> PlotSeries {
        PlotSeries {
            name: name.into(),
            reference,
            points: values.iter().enumerate().map(|(i, v)| (i as f64, *v)).collect(),
        }
    }

    #[test]
    fn a_far_away_limit_does_not_flatten_the_usage() {
        let usage = series("app", false, &[0.1, 0.2, 0.15]);
        // A request just above the usage is on the scale.
        assert_eq!(axis_max(&[usage.clone(), series("request", true, &[0.25])]), 0.25);
        // A limit ten times the usage is not; one within reach is.
        assert_eq!(
            axis_max(&[
                usage.clone(),
                series("request", true, &[0.25]),
                series("limit", true, &[2.0])
            ]),
            0.25
        );
        assert_eq!(axis_max(&[usage.clone(), series("limit", true, &[0.5])]), 0.5);
        // Without usage the references set the scale.
        assert_eq!(axis_max(&[series("limit", true, &[2.0])]), 2.0);
        assert_eq!(axis_max(&[]), 0.0);
    }

    #[test]
    fn long_series_names_are_cut_in_the_middle() {
        assert_eq!(clip_name("app", 32), "app");
        assert_eq!(clip_name(&"a".repeat(32), 32), "a".repeat(32));
        let long = format!("{}-tail", "x".repeat(500));
        let short = clip_name(&long, 32);
        assert_eq!(short.chars().count(), 32);
        assert!(short.starts_with("xxx") && short.ends_with("x-tail") && short.contains('…'), "{short}");
        // Not ASCII, and a limit too small to cut at.
        let accents = "é".repeat(100);
        assert_eq!(clip_name(&accents, 9), format!("{}…{}", "é".repeat(4), "é".repeat(4)));
        assert_eq!(clip_name("abcdef", 0).chars().count(), 3);
    }

    #[test]
    fn pod_names_are_shortened_to_what_differs() {
        let names = [
            "gateway-77c47474cd-292q8",
            "gateway-77c47474cd-7dls6",
            "gateway-77c47474cd-dh24n",
        ];
        let cut = common_prefix(&names);
        assert_eq!(&names[0][cut..], "292q8");
        assert_eq!(common_prefix(&["kafka-0", "kafka-1"]), 6);
        assert_eq!(common_prefix(&["app", "sidecar"]), 0);
        assert_eq!(common_prefix(&["only-one"]), 0);
        assert_eq!(
            common_prefix(&["web-", "web-1"]),
            0,
            "nothing would be left of the first name"
        );
        assert_eq!(common_prefix(&[]), 0);
    }
}
