//! voice clipper — power button + clip button, nothing else.
//!
//! Green power = listening for your phrases. Grey = muted (mic still runs,
//! speech is ignored). The clip button saves OBS's replay buffer right now,
//! same as saying your phrase or running `clipctl clip`.
//!
//! The mute state lives in the same file `clipctl toggle` uses, so the KDE
//! keyboard shortcut and this button always show the truth.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::Duration;

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2};

/// How often to re-read the mute flag, so a hotkey press updates the button
/// even though this window did nothing.
const POLL: Duration = Duration::from_millis(400);

/// The two states everything in this window reports: green = armed/alive,
/// red = disarmed/won't stay alive. Grey is reserved for "not clickable yet".
const GREEN: Color32 = Color32::from_rgb(40, 200, 120);
const RED: Color32 = Color32::from_rgb(224, 68, 63);
const GREY: Color32 = Color32::from_rgb(122, 128, 136);

struct PowerApp {
    muted: bool,
    /// Decays after a click so the button flashes and feels responsive.
    flash: f32,
    /// Last clip result, shown under the buttons and cleared after a moment.
    status: String,
    status_age: f32,
    /// True while the save request is in flight, so a double-click can't fire
    /// two saves at once.
    clipping: bool,
    /// Cached because it comes from a /proc scan and egui repaints on mouse
    /// movement too — refreshing it on every paint would be wasteful.
    gsr_running: bool,
    gsr_poll: f32,
    /// Whether the listener should keep restarting OBS's replay buffer.
    auto_buffer: bool,
    /// Same for GPU Screen Recorder. The two are mutually exclusive: two
    /// recorders grabbing one screen is double the overhead for one clip.
    gsr_auto: bool,
    /// OBS clip length in seconds, read from OBS. `false` until the first
    /// successful read, so the menu can show "?" instead of lying.
    obs_secs: u32,
    obs_known: bool,
    /// GSR clip length, which lives in our own state file because gsr has no
    /// config we own.
    gsr_secs: u32,
}

impl PowerApp {
    fn new() -> Self {
        let buffer = clipper::obs::Obs::current_buffer_seconds().ok().flatten();
        Self {
            muted: clipper::state::is_muted().unwrap_or(true),
            flash: 0.0,
            status: String::new(),
            status_age: 99.0,
            clipping: false,
            gsr_running: clipper::gsr::pid().is_some(),
            gsr_poll: 0.0,
            auto_buffer: clipper::state::auto_buffer_enabled().unwrap_or(true),
            gsr_auto: clipper::state::gsr_auto_enabled().unwrap_or(false),
            obs_secs: buffer.unwrap_or(180) as u32,
            obs_known: buffer.is_some(),
            gsr_secs: clipper::gsr::seconds(),
        }
    }

    /// Human label for a buffer length: 180 -> "3m", 45 -> "45s".
    fn buffer_label(secs: u32) -> String {
        if secs >= 60 && secs.is_multiple_of(60) {
            format!("{}m", secs / 60)
        } else {
            format!("{secs}s")
        }
    }

    /// Apply a new OBS clip length. Blocking on purpose: it is a localhost call
    /// and the window has nothing else to do, same tradeoff as `clip_now`.
    fn apply_obs_buffer(&mut self, secs: u32) {
        match clipper::obs::Obs::apply_buffer_seconds(secs) {
            Ok(_) => {
                self.obs_secs = secs;
                self.obs_known = true;
                self.set_status(format!("OBS clip length set to {}", Self::buffer_label(secs)));
            }
            Err(e) => self.set_status(format!("could not change OBS length: {e:#}")),
        }
    }

    /// Apply a new GSR clip length. gsr cannot resize a buffer that is already
    /// running, so a live recorder is restarted — which drops what it held.
    fn apply_gsr_buffer(&mut self, secs: u32) {
        match clipper::state::set_gsr_seconds(secs) {
            Ok(()) => {
                self.gsr_secs = secs;
                let restarted = if clipper::gsr::pid().is_some() {
                    clipper::gsr::stop().ok();
                    clipper::gsr::start().is_ok()
                } else {
                    false
                };
                self.gsr_running = clipper::gsr::pid().is_some();
                self.set_status(format!(
                    "GSR clip length set to {}{}",
                    Self::buffer_label(secs),
                    if restarted { " (recorder restarted)" } else { "" }
                ));
            }
            Err(e) => self.set_status(format!("could not change GSR length: {e:#}")),
        }
    }

    /// Turn OBS keep-alive on or off, switching GSR's off if that would mean
    /// both recorders running.
    fn set_keep_obs(&mut self, on: bool) {
        self.auto_buffer = on;
        if on && self.gsr_auto {
            self.gsr_auto = false;
            if let Err(e) = clipper::state::set_gsr_auto(false) {
                self.set_status(format!("could not save setting: {e:#}"));
            }
        }
        if let Err(e) = clipper::state::set_auto_buffer(on) {
            self.set_status(format!("could not save setting: {e:#}"));
        }
    }

    /// Turn GSR keep-alive on or off, switching OBS's off in the same move.
    fn set_keep_gsr(&mut self, on: bool) {
        self.gsr_auto = on;
        if on && self.auto_buffer {
            self.auto_buffer = false;
            if let Err(e) = clipper::state::set_auto_buffer(false) {
                self.set_status(format!("could not save setting: {e:#}"));
            }
        }
        if let Err(e) = clipper::state::set_gsr_auto(on) {
            self.set_status(format!("could not save setting: {e:#}"));
        }
        // Turning it on should take effect now, not at the next 30s tick.
        if on && clipper::gsr::pid().is_none() {
            match clipper::gsr::start() {
                Ok(_) => self.gsr_running = true,
                Err(e) => self.set_status(format!("could not start GSR: {e:#}")),
            }
        }
    }

    fn toggle(&mut self) {
        match clipper::state::toggle() {
            Ok(muted) => self.muted = muted,
            Err(e) => self.set_status(format!("could not toggle: {e:#}")),
        }
        self.flash = 1.0;
    }

    /// Ask OBS to write the replay buffer out. Runs on the UI thread on purpose:
    /// it is a single localhost request that returns in well under a second, and
    /// keeping it synchronous avoids a whole class of threading bugs for one call.
    fn clip_now(&mut self) {
        self.clipping = true;
        let result = clipper::config::ObsWsSettings::load()
            .and_then(|s| clipper::obs::Obs::connect(&s))
            .and_then(|mut obs| obs.save_replay().map(|_| ()));
        self.clipping = false;
        self.status_age = 0.0;
        self.status = match result {
            Ok(()) => {
                if let Err(e) = clipper::sound::play() {
                    eprintln!("voice-clipper: could not play the clip sound: {e:#}");
                }
                "clip saved".to_string()
            }
            Err(e) => format!("{e:#}"),
        };
    }

    /// One button, two jobs: if GPU Screen Recorder is not running, start its
    /// replay buffer; if it is running, save that buffer. Like `clip_now` this
    /// is a sub-second call, so it stays on the UI thread.
    fn gsr_action(&mut self) {
        self.clipping = true;
        let result = if clipper::gsr::pid().is_some() {
            clipper::gsr::save().map(|_| "gsr clip saved".to_string())
        } else {
            clipper::gsr::start().map(|_| "gsr recording started".to_string())
        };
        self.clipping = false;
        self.gsr_running = clipper::gsr::pid().is_some();
        self.gsr_poll = 0.0;
        self.set_status(result.unwrap_or_else(|e| format!("{e:#}")));
    }

    fn stop_gsr(&mut self) {
        self.clipping = true;
        let result = clipper::gsr::stop().map(|_| "gsr stopped".to_string());
        self.clipping = false;
        self.gsr_running = false;
        self.set_status(result.unwrap_or_else(|e| format!("{e:#}")));
    }

    fn set_status(&mut self, s: String) {
        self.status = s;
        self.status_age = 0.0;
    }
}

/// Draw a clapperboard glyph inside `r`.
///
/// egui ships no icon set, so the glyph is built from primitives: a board, a
/// tilted top stick, and three pale slashes across the stick. Hand-drawn beats
/// shipping a font or an image for one glyph.
fn clapper(painter: &egui::Painter, r: Rect, color: Color32) {
    let s = r.height().min(r.width());
    if s <= 1.0 {
        return;
    }
    let left = r.center().x - s * 0.5;
    let top = r.center().y - s * 0.5;
    let body_top = top + s * 0.40;

    // Board body.
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(left, body_top), Pos2::new(left + s, top + s * 0.95)),
        2.0,
        color,
    );

    // The stick, tilted up to the right like a clapper mid-snap.
    let tilt = s * 0.17;
    let stick_bottom_left = Pos2::new(left, body_top - s * 0.03);
    let stick_bottom_right = Pos2::new(left + s, body_top - s * 0.03 - tilt);
    painter.add(Shape::convex_polygon(
        vec![
            stick_bottom_left,
            stick_bottom_right,
            Pos2::new(left + s, stick_bottom_right.y - s * 0.22),
            Pos2::new(left, stick_bottom_left.y - s * 0.22),
        ],
        color,
        Stroke::NONE,
    ));

    // Slashes across the stick. They are drawn slightly proud of the stick and
    // then covered again by nothing — at this size the overlap reads as a stripe.
    let slash = Color32::from_black_alpha(130);
    for i in 0..3 {
        let f = 0.16 + i as f32 * 0.29;
        let x0 = left + s * f;
        let y0 = stick_bottom_left.y - tilt * f;
        let x1 = x0 + s * 0.13;
        let y1 = stick_bottom_left.y - tilt * (f + 0.13);
        painter.line_segment(
            [
                Pos2::new(x0, y0),
                Pos2::new(x1, y1 - s * 0.22),
            ],
            Stroke::new(s * 0.10, slash),
        );
    }
}

/// Width of a label at the size the clip buttons use.
fn label_width(ui: &mut egui::Ui, label: &str) -> f32 {
    ui.fonts(|f| {
        f.layout_no_wrap(
            label.to_owned(),
            egui::FontId::new(15.0, egui::FontFamily::Proportional),
            Color32::WHITE,
        )
        .size()
        .x
    })
}

/// Left padding, glyph size, and glyph-to-label gap for `side_button`.
const GLYPH_PAD: f32 = 10.0;
const GLYPH_W: f32 = 18.0;
const GLYPH_GAP: f32 = 8.0;

/// Button width that leaves the label centred just to the right of the glyph.
fn side_button_width(ui: &mut egui::Ui, label: &str) -> f32 {
    2.0 * (GLYPH_PAD + GLYPH_W + GLYPH_GAP) + label_width(ui, label)
}

/// A dropdown caret, drawn as a filled triangle.
///
/// egui's bundled font has no ▾ or ▴ codepoint, so the obvious unicode caret
/// renders as an empty box. Same reason the power ring and the clapper are
/// drawn by hand: at this size a drawn shape is the only thing that is
/// guaranteed to appear.
fn caret_button(ui: &mut egui::Ui, up: bool) -> egui::Response {
    let btn = ui.add(egui::Button::new(" ").min_size(Vec2::new(22.0, 34.0)));
    let c = btn.rect.center();
    let (half_w, half_h) = (6.0, 3.5);
    let tri = if up {
        vec![
            Pos2::new(c.x - half_w, c.y + half_h),
            Pos2::new(c.x + half_w, c.y + half_h),
            Pos2::new(c.x, c.y - half_h),
        ]
    } else {
        vec![
            Pos2::new(c.x - half_w, c.y - half_h),
            Pos2::new(c.x + half_w, c.y - half_h),
            Pos2::new(c.x, c.y + half_h),
        ]
    };
    ui.painter()
        .add(Shape::convex_polygon(tri, Color32::from_gray(190), Stroke::NONE));
    btn
}

/// A button with a clapperboard painted at its left edge.
///
/// egui centres a button's label in the whole button, so a fixed `min_size`
/// lets a longer label slide left underneath the glyph — that is how "GSR ON"
/// rendered as "SR ON". Sizing from the measured text keeps the label clear of
/// the glyph at any length.
fn side_button(
    ui: &mut egui::Ui,
    label: &str,
    glyph: Color32,
    enabled: bool,
    width: f32,
) -> egui::Response {
    let btn = ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(label).size(15.0).strong())
            .min_size(Vec2::new(width, 34.0)),
    );
    clapper(
        ui.painter(),
        Rect::from_min_size(
            Pos2::new(btn.rect.min.x + GLYPH_PAD, btn.rect.center().y - GLYPH_W / 2.0),
            Vec2::new(GLYPH_W, GLYPH_W),
        ),
        glyph,
    );
    btn
}

/// Sample a circular arc into a polyline.
///
/// Why not a real arc shape: egui 0.29 (the version glaunch-gui uses) has no
/// arc primitive at all, so a 64-point polyline is the portable way to draw a
/// ring with a gap in it.
fn arc_points(center: Pos2, radius: f32, a_min: f32, a_max: f32, steps: usize) -> Vec<Pos2> {
    (0..=steps)
        .map(|i| {
            let t = a_min + (a_max - a_min) * (i as f32 / steps as f32);
            Pos2::new(center.x + radius * t.cos(), center.y + radius * t.sin())
        })
        .collect()
}

impl eframe::App for PowerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Re-read state periodically: the hotkey or `clipctl mute` may have
        // changed it while this window was just sitting here.
        let fresh = clipper::state::is_muted().ok();
        if self.flash <= 0.0 && fresh.is_some_and(|m| m != self.muted) {
            self.muted = fresh.unwrap_or(self.muted);
        }
        self.flash = (self.flash - 0.08).max(0.0);
        self.status_age += 0.4;
        // Re-check the recorder every couple of seconds: it may have been
        // started or stopped from a terminal.
        self.gsr_poll += 0.4;
        if self.gsr_poll >= 2.0 {
            self.gsr_poll = 0.0;
            self.gsr_running = clipper::gsr::pid().is_some();
            self.auto_buffer = clipper::state::auto_buffer_enabled().unwrap_or(self.auto_buffer);
            self.gsr_auto = clipper::state::gsr_auto_enabled().unwrap_or(self.gsr_auto);
            self.gsr_secs = clipper::gsr::seconds();
        }
        ctx.request_repaint_after(POLL);

        // Test hook, not a feature: lets a screenshot or a scripted check see
        // the dropdown open. Nothing sets this variable in normal use.
        if std::env::var("VOICE_CLIPPER_DEBUG_POPUP").is_ok() {
            ctx.memory_mut(|m| {
                m.open_popup(egui::Id::new("obs_menu"));
                m.open_popup(egui::Id::new("gsr_menu"));
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered_justified(|ui| {
                ui.add_space(12.0);

                // The clickable area is the disc itself. Allocating a square
                // and taking clicks anywhere in it meant a big invisible box
                // around the button responded to presses.
                let size = Vec2::new(100.0, 100.0);
                let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                let center = Pos2::new(rect.center().x, rect.center().y);
                let disc_r = size.x * 0.46;
                let disc = Rect::from_center_size(center, Vec2::splat(disc_r * 2.0));
                let response = ui.interact(disc, ui.id().with("power_button"), Sense::click());
                // Round hit area, not the square around it: a click in the
                // corner of that square hits nothing.
                let clicked_in_disc = response.clicked()
                    && response
                        .ctx
                        .input(|i| i.pointer.interact_pos())
                        .map(|p| p.distance(center) <= disc_r)
                        .unwrap_or(false);
                if clicked_in_disc {
                    self.toggle();
                }
                if response.has_focus() {
                    // Space/Enter toggle too, so the button is keyboard usable.
                    let press =
                        ui.input(|i| i.key_pressed(egui::Key::Space) || i.key_pressed(egui::Key::Enter));
                    if press {
                        self.toggle();
                    }
                }

                let on = !self.muted;
                let live = if on { GREEN } else { RED };
                let r = size.x * 0.36;
                let painter = ui.painter();
                let ring_stroke = Stroke::new(7.0_f32, live);

                // Soft disc behind the symbol so the button reads as pressable.
                painter.circle_filled(center, size.x * 0.46, live.linear_multiply(0.15 + 0.35 * self.flash));
                painter.circle_stroke(
                    center,
                    size.x * 0.46,
                    Stroke::new(2.0_f32, live.linear_multiply(0.6 + 0.4 * self.flash)),
                );

                // The power glyph: a ring with a gap at the top, plus a stem.
                // Angles are radians, clockwise, 0 = +x, and -PI/2 points up.
                let gap = 0.55_f32;
                painter.add(Shape::line(
                    arc_points(center, r, -FRAC_PI_2 + gap, -FRAC_PI_2 - gap + TAU, 64),
                    ring_stroke,
                ));
                painter.line_segment(
                    [
                        Pos2::new(center.x, center.y - r * 0.78),
                        Pos2::new(center.x, center.y - r * 0.05),
                    ],
                    ring_stroke,
                );

                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(if on { "Clipper Enabled" } else { "Clipper Disabled" })
                        .size(18.0)
                        .color(live)
                        .strong(),
                );

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    // One width for both clip buttons so they read as a pair.
                    let btn_w =
                        side_button_width(ui, "OBS").max(side_button_width(ui, "GSR"));

                    let clip = side_button(
                        ui,
                        "OBS",
                        // Green while something keeps the OBS buffer alive, red
                        // when it is on its own and can be off without warning.
                        if self.clipping {
                            GREY
                        } else if self.auto_buffer {
                            GREEN
                        } else {
                            RED
                        },
                        !self.clipping,
                        btn_w,
                    );
                    if clip.clicked() {
                        self.clip_now();
                    }

                    // One menu per recorder, hung off the button it belongs to.
                    // Each recorder has its own length and its own keep-alive,
                    // so there is no single "buffer" setting left to share.
                    let obs_open = ui.memory(|m| m.is_popup_open(egui::Id::new("obs_menu")));
                    let obs_arrow = caret_button(ui, obs_open)
                        .on_hover_text("OBS clip length and keep-alive");
                    if obs_arrow.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(egui::Id::new("obs_menu")));
                    }
                    egui::popup::popup_below_widget(
                        ui,
                        egui::Id::new("obs_menu"),
                        &obs_arrow,
                        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
                        |ui| {
                            ui.set_min_width(170.0);
                            ui.label(egui::RichText::new("OBS clip length").weak().small());
                            for secs in [30u32, 60, 120, 180, 300] {
                                let picked = ui.selectable_value(
                                    &mut self.obs_secs,
                                    secs,
                                    Self::buffer_label(secs),
                                );
                                if picked.clicked() {
                                    self.apply_obs_buffer(secs);
                                }
                            }
                            ui.separator();
                            let mut on = self.auto_buffer;
                            let cb = ui
                                .checkbox(&mut on, "keep OBS buffer on")
                                .on_hover_text(
                                    "On: the listener restarts OBS's replay buffer if it is off.\n\
                                     Off: your manual choice in OBS is respected.\n\
                                     Turning this on turns GSR's keep-alive off.",
                                );
                            if cb.changed() {
                                self.set_keep_obs(on);
                            }
                        },
                    );

                    // GPU Screen Recorder: no websocket, just signals. Off means
                    // "start the buffer", on means "save it". The label stays
                    // constant; the clapper colour and the row below carry the
                    // state, so nothing here can overflow the button.
                    let gsr_btn = side_button(
                        ui,
                        "GSR",
                        // Same rule as OBS: the colour reports the keep-alive
                        // setting, so green means "this is the armed recorder".
                        if self.gsr_auto { GREEN } else { RED },
                        !self.clipping,
                        btn_w,
                    );
                    // on_hover_text consumes the Response, so the click check
                    // has to use what it returns.
                    let gsr_btn = gsr_btn.on_hover_text(if self.gsr_running {
                        "GPU Screen Recorder is recording — click to save the last few seconds"
                    } else {
                        "Start GPU Screen Recorder's replay buffer (uses the GPU, not the CPU)"
                    });
                    if gsr_btn.clicked() {
                        self.gsr_action();
                    }

                    let gsr_open = ui.memory(|m| m.is_popup_open(egui::Id::new("gsr_menu")));
                    let gsr_arrow = caret_button(ui, gsr_open)
                        .on_hover_text("GPU Screen Recorder clip length and keep-alive");
                    if gsr_arrow.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(egui::Id::new("gsr_menu")));
                    }
                    egui::popup::popup_below_widget(
                        ui,
                        egui::Id::new("gsr_menu"),
                        &gsr_arrow,
                        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
                        |ui| {
                            ui.set_min_width(170.0);
                            ui.label(egui::RichText::new("GSR clip length").weak().small());
                            for secs in [30u32, 60, 120, 180, 300] {
                                let picked = ui.selectable_value(
                                    &mut self.gsr_secs,
                                    secs,
                                    Self::buffer_label(secs),
                                );
                                if picked.clicked() {
                                    self.apply_gsr_buffer(secs);
                                }
                            }
                            ui.separator();
                            let mut on = self.gsr_auto;
                            let cb = ui
                                .checkbox(&mut on, "keep GSR buffer on")
                                .on_hover_text(
                                    "On: GPU Screen Recorder runs all the time and the voice\n\
                                     phrase clips it instead of OBS.\n\
                                     Turning this on turns OBS's keep-alive off.",
                                );
                            if cb.changed() {
                                self.set_keep_gsr(on);
                            }
                        },
                    );
                });

                if self.gsr_running {
                    let stop = ui
                        .small_button("stop GSR")
                        .on_hover_text("Stop GPU Screen Recorder (the current file is finalised)");
                    if stop.clicked() {
                        self.stop_gsr();
                    }
                    ui.add_space(2.0);
                }

                if self.status_age < 6.0 && !self.status.is_empty() {
                    ui.add_space(4.0);
                    let ok = matches!(
                        self.status.as_str(),
                        "clip saved" | "gsr clip saved" | "gsr recording started" | "gsr stopped"
                    );
                    ui.label(
                        egui::RichText::new(&self.status)
                            .size(12.0)
                            .color(if ok { GREEN } else { RED }),
                    );
                }

                ui.add_space(8.0);
            });
        });
    }
}

fn main() -> eframe::Result {
    // 64x64 RGBA pixels generated from assets/voice-clipper.png (see install.sh).
    // eframe wants literal pixels rather than an encoded image, which keeps an
    // image decoder out of the dependency tree.
    let icon = egui::IconData {
        rgba: include_bytes!("../assets/icon.rgba").to_vec(),
        width: 64,
        height: 64,
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([300.0, 330.0])
        .with_resizable(false)
        .with_icon(icon)
        .with_title("Voice Clipper");
    if let Some((x, y)) = window_position() {
        viewport = viewport.with_position(egui::pos2(x, y));
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("Voice Clipper", options, Box::new(|_cc| Ok(Box::new(PowerApp::new()))))
}

/// Where to open the window, from `VOICE_CLIPPER_POS="x,y"`.
///
/// egui cannot see monitor geometry, so the launcher says where the second
/// screen starts. The autostart entry uses it so the window appears on the
/// monitor you are looking at rather than always the primary one.
fn window_position() -> Option<(f32, f32)> {
    let v = std::env::var("VOICE_CLIPPER_POS").ok()?;
    let (x, y) = v.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::arc_points;
    use eframe::egui::Pos2;

    #[test]
    fn arc_points_all_sit_on_the_circle() {
        let c = Pos2::new(100.0, 100.0);
        let r = 40.0;
        for p in arc_points(c, r, 0.0, std::f32::consts::TAU, 16) {
            let d = ((p.x - c.x).powi(2) + (p.y - c.y).powi(2)).sqrt();
            assert!((d - r).abs() < 0.001, "{p:?} is {d} away, expected {r}");
        }
    }
}
