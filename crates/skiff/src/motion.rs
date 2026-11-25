//! Motion: how long things take to move, and the one switch that stops it.
//!
//! egui animates its own widgets (dialogs fade in, panels slide, a table
//! glides to the selected row) over `Style::animation_time`. Skiff's own
//! animations (notifications, the detail panel, the dock) read the same
//! value, so the `animations` setting turns all of it off in one place.

use egui::style::ScrollAnimation;
use egui::{Context, Id, Rangef};

/// How long a panel takes to slide in and a dialog to fade in, in seconds.
/// Short on purpose: motion says where something came from, it must not
/// make anyone wait.
pub const TIME: f32 = 0.15;

/// Turns animations on or off for everything drawn with `ctx`.
pub fn apply(ctx: &Context, on: bool) {
    ctx.all_styles_mut(|style| {
        style.animation_time = if on { TIME } else { 0.0 };
        style.scroll_animation = if on {
            // egui's speed, with a lower ceiling: a jump across a long
            // table should not take a third of a second.
            ScrollAnimation::new(1000.0, Rangef::new(0.08, 0.2))
        } else {
            ScrollAnimation::none()
        };
    });
}

/// Whether animations are on.
pub fn on(ctx: &Context) -> bool {
    ctx.global_style().animation_time > 0.0
}

/// How far something is on its way in (0 = gone, 1 = fully there), moving
/// towards `shown`: quick at first, settling at the end. Keeps the window
/// repainting while it moves. The first call for an `id` answers `shown`
/// at once, so something that should arrive animated asks with `false`
/// for one frame first.
pub fn presence(ctx: &Context, id: Id, shown: bool) -> f32 {
    ctx.animate_bool_responsive(id, shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one frame `dt` seconds after the previous one.
    fn frame<R>(ctx: &Context, time: &mut f64, dt: f64, run: impl FnOnce(&Context) -> R) -> R {
        *time += dt;
        let input = egui::RawInput { time: Some(*time), predicted_dt: dt as f32, ..Default::default() };
        let mut run = Some(run);
        let mut result = None;
        let mut output = ctx.run_ui(input, |ui| result = run.take().map(|run| run(ui.ctx())));
        // (Nothing draws this frame; egui wants that said.)
        output.textures_delta.clear();
        result.expect("the frame ran")
    }

    #[test]
    fn presence_moves_over_the_animation_time_and_then_rests() {
        let ctx = Context::default();
        apply(&ctx, true);
        assert!(on(&ctx));
        let (id, mut time) = (Id::unique("thing"), 0.0);
        // Asked with `false` first, it starts from nothing...
        assert_eq!(frame(&ctx, &mut time, 0.01, |ctx| presence(ctx, id, false)), 0.0);
        let start = frame(&ctx, &mut time, 0.01, |ctx| presence(ctx, id, true));
        assert!(start < 0.3, "{start}");
        // ...is part of the way a moment later, further along than the
        // time that has passed (quick first, settling at the end)...
        let half = frame(&ctx, &mut time, f64::from(TIME) / 2.0, |ctx| presence(ctx, id, true));
        assert!(half > 0.5 && half < 1.0, "{half}");
        assert!(ctx.has_requested_repaint(), "the window keeps repainting while something moves");
        // ...and there, and still, once the time is up.
        assert_eq!(frame(&ctx, &mut time, f64::from(TIME), |ctx| presence(ctx, id, true)), 1.0);
        frame(&ctx, &mut time, 0.01, |ctx| presence(ctx, id, true));
        assert!(!ctx.has_requested_repaint(), "nothing moves: an idle window must cost nothing");
    }

    #[test]
    fn turned_off_everything_is_immediate() {
        let ctx = Context::default();
        apply(&ctx, false);
        assert!(!on(&ctx));
        let (id, mut time) = (Id::unique("thing"), 0.0);
        assert_eq!(frame(&ctx, &mut time, 0.01, |ctx| presence(ctx, id, false)), 0.0);
        assert_eq!(frame(&ctx, &mut time, 0.001, |ctx| presence(ctx, id, true)), 1.0);
        assert_eq!(frame(&ctx, &mut time, 0.001, |ctx| presence(ctx, id, false)), 0.0);
        assert_eq!(ctx.global_style().scroll_animation, ScrollAnimation::none());
    }
}
