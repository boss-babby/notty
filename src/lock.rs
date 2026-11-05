//! The animated password window shown at startup (and when the workspace is locked).

use egui::{
    pos2, vec2, Align2, Area, Color32, Context, CornerRadius, CursorIcon, Frame, Id, Key, Margin, RichText, Sense,
    Stroke,
};

use crate::auth::{self, Credential};
use crate::theme::{bold, body, lerp_color, Palette};
use crate::widgets::{appear, ease, field, primary_button, radial_glow, with_alpha};

pub enum LockEvent {
    None,
    Unlocked,
    Created(Credential),
    Skipped,
    Reset,
}

#[derive(Default)]
pub struct LockScreen {
    pw: String,
    pw2: String,
    error: Option<String>,
    error_t: f64,
    show: bool,
    confirm_reset: bool,
    focus_pending: bool,
    working: bool,
}

impl LockScreen {
    pub fn new() -> Self {
        Self { focus_pending: true, ..Default::default() }
    }

    pub fn clear(&mut self) {
        self.pw.clear();
        self.pw2.clear();
        self.error = None;
        self.confirm_reset = false;
        self.focus_pending = true;
        self.working = false;
    }

    fn fail(&mut self, ctx: &Context, msg: &str) {
        self.error = Some(msg.to_owned());
        self.error_t = ctx.input(|i| i.time);
    }

    pub fn show(&mut self, ctx: &Context, cred: Option<&Credential>) -> LockEvent {
        let pal = Palette::dark();
        let time = ctx.input(|i| i.time) as f32;
        let mut event = LockEvent::None;
        let setup = cred.is_none();

        egui::CentralPanel::default().frame(Frame::new().fill(Color32::from_rgb(12, 12, 15))).show(ctx, |ui| {
            // animated aurora background
            let r = ui.max_rect();
            let p = ui.painter();
            let blobs = [
                (0.25, 0.30, 0.9, 0.00, Color32::from_rgba_unmultiplied(70, 90, 255, 70)),
                (0.78, 0.25, 0.7, 1.70, Color32::from_rgba_unmultiplied(190, 70, 255, 55)),
                (0.60, 0.80, 0.8, 3.10, Color32::from_rgba_unmultiplied(40, 200, 190, 45)),
                (0.15, 0.85, 0.6, 4.60, Color32::from_rgba_unmultiplied(255, 100, 120, 40)),
            ];
            for (x, y, s, ph, col) in blobs {
                let cx = r.left() + r.width() * (x + 0.06 * (time * 0.35 + ph).sin());
                let cy = r.top() + r.height() * (y + 0.06 * (time * 0.28 + ph * 1.3).cos());
                radial_glow(p, pos2(cx, cy), r.width().max(r.height()) * 0.45 * s, col);
            }
            // ~30 fps is plenty for a slow aurora and halves the GPU cost of the lock screen
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        });

        let a = appear(ctx, Id::new("lock_card_in"), 0.7);
        let mut dx = 0.0;
        if self.error.is_some() {
            let dt = (ctx.input(|i| i.time) - self.error_t) as f32;
            if dt < 0.5 {
                dx = (dt * 48.0).sin() * 11.0 * (1.0 - dt / 0.5);
            }
        }

        // phones: the card never exceeds the screen (it used to be a fixed 330 pt + 2×34 pt margin) and scrolls when the keyboard leaves little room
        let screen = ctx.screen_rect();
        let small = screen.width() < 440.0;
        let pad: i8 = if small { 20 } else { 34 };
        let cw = (screen.width() - 2.0 * pad as f32 - 24.0).clamp(180.0, 330.0);
        // centred by hand (an Area only offers the space below its anchor): remembers the card height of the previous frame
        let card_h = ctx.data(|d| d.get_temp::<f32>(Id::new("lock_card_h"))).unwrap_or(430.0);
        let top = ((screen.height() - card_h) / 2.0).max(12.0);
        Area::new(Id::new("lock_card"))
            .anchor(Align2::CENTER_TOP, vec2(dx, top + (1.0 - a) * 28.0))
            .show(ctx, |ui| {
                ui.set_opacity(a);
                let mut card = |ui: &mut egui::Ui| {
                let fr = Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(24, 24, 29, 232))
                    .stroke(Stroke::new(1.0_f32, Color32::from_white_alpha(26)))
                    .corner_radius(CornerRadius::same(20))
                    .inner_margin(Margin::same(pad))
                    .shadow(egui::Shadow { offset: [0, 24], blur: 60, spread: 0, color: Color32::from_black_alpha(140) })
                    .show(ui, |ui| {
                        ui.set_width(cw);
                        ui.spacing_mut().item_spacing.y = 10.0;
                        ui.vertical_centered(|ui| {
                            // logo
                            let (r, _) = ui.allocate_exact_size(vec2(60.0, 60.0), Sense::hover());
                            let pulse = 1.0 + 0.04 * (time * 2.0).sin();
                            radial_glow(ui.painter(), r.center(), 54.0 * pulse, Color32::from_rgba_unmultiplied(80, 120, 255, 60));
                            ui.painter().rect_filled(r, 16.0, Color32::from_rgb(45, 130, 235));
                            ui.painter().rect_filled(r.shrink(2.0).with_max_y(r.center().y), CornerRadius { nw: 14, ne: 14, sw: 0, se: 0 }, Color32::from_white_alpha(10));
                            ui.painter().text(r.center(), Align2::CENTER_CENTER, "L", bold(30.0), Color32::WHITE);
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(if setup { "Secure your workspace" } else { "Welcome back" })
                                    .font(bold(24.0))
                                    .color(Color32::WHITE),
                            );
                            ui.label(
                                RichText::new(if setup {
                                    "Create a password to protect your notes."
                                } else {
                                    "Enter your password to unlock Lumen."
                                })
                                .font(body(14.0))
                                .color(Color32::from_gray(150)),
                            );
                            ui.add_space(10.0);
                        });

                        let fill = Color32::from_white_alpha(12);
                        let id1 = Id::new("lock_pw");
                        let id2 = Id::new("lock_pw2");
                        let r1 = field(ui, &pal, fill, &mut self.pw, if setup { "New password" } else { "Password" }, !self.show, cw, id1);
                        if self.focus_pending {
                            ctx.memory_mut(|m| m.request_focus(id1));
                            self.focus_pending = false;
                        }
                        let mut r2 = None;
                        if setup {
                            r2 = Some(field(ui, &pal, fill, &mut self.pw2, "Confirm password", !self.show, cw, id2));
                            // strength meter
                            let (s, label) = auth::strength(&self.pw);
                            let v = ctx.animate_value_with_time(Id::new("strength"), s, 0.25);
                            let (rect, _) = ui.allocate_exact_size(vec2(cw, 14.0), Sense::hover());
                            let bar = egui::Rect::from_min_size(rect.min + vec2(0.0, 2.0), vec2(rect.width() - 56.0, 4.0));
                            ui.painter().rect_filled(bar, 2.0, Color32::from_white_alpha(18));
                            let col = lerp_color(Color32::from_rgb(235, 80, 80), Color32::from_rgb(60, 210, 130), v);
                            ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * v, 4.0)), 2.0, col);
                            ui.painter().text(pos2(rect.right(), rect.top() + 4.0), Align2::RIGHT_CENTER, label, body(12.0), col);
                        }

                        ui.horizontal(|ui| {
                            let lbl = if self.show { "Hide password" } else { "Show password" };
                            let r = ui.add(egui::Label::new(RichText::new(lbl).font(body(12.5)).color(Color32::from_gray(140))).sense(Sense::click()));
                            if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                self.show = !self.show;
                            }
                        });

                        if let Some(e) = &self.error {
                            ui.label(RichText::new(e).font(body(13.0)).color(pal.danger));
                        }

                        let enter = ui.input(|i| i.key_pressed(Key::Enter))
                            && (r1.has_focus() || r1.lost_focus() || r2.as_ref().map_or(false, |r| r.has_focus() || r.lost_focus()));
                        ui.add_space(4.0);
                        let label = if setup { "Create password" } else { "Unlock" };
                        let clicked = primary_button(ui, &pal, label, cw).clicked();
                        if clicked || enter {
                            if setup {
                                if self.pw.chars().count() < 4 {
                                    self.fail(ctx, "Use at least 4 characters.");
                                } else if self.pw != self.pw2 {
                                    self.fail(ctx, "Passwords don't match.");
                                } else {
                                    let c = auth::create(&self.pw);
                                    self.clear();
                                    event = LockEvent::Created(c);
                                }
                            } else if let Some(c) = cred {
                                if auth::verify(c, &self.pw) {
                                    self.clear();
                                    event = LockEvent::Unlocked;
                                } else {
                                    self.pw.clear();
                                    self.fail(ctx, "Incorrect password. Try again.");
                                    ctx.memory_mut(|m| m.request_focus(id1));
                                }
                            }
                        }

                        ui.add_space(4.0);
                        ui.vertical_centered(|ui| {
                            if setup {
                                let r = ui.add(egui::Label::new(RichText::new("Skip for now").font(body(13.0)).color(Color32::from_gray(150))).sense(Sense::click()));
                                if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                    self.clear();
                                    event = LockEvent::Skipped;
                                }
                            } else if !self.confirm_reset {
                                let r = ui.add(egui::Label::new(RichText::new("Forgot password?").font(body(13.0)).color(Color32::from_gray(150))).sense(Sense::click()));
                                if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                    self.confirm_reset = true;
                                }
                            } else {
                                ui.label(RichText::new("This erases the workspace on this device.").font(body(12.5)).color(Color32::from_gray(150)));
                                ui.horizontal(|ui| {
                                    ui.add_space(70.0);
                                    let r = ui.add(egui::Label::new(RichText::new("Erase & start over").font(body(13.0)).color(pal.danger)).sense(Sense::click()));
                                    if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                        self.clear();
                                        event = LockEvent::Reset;
                                    }
                                    let r = ui.add(egui::Label::new(RichText::new("Cancel").font(body(13.0)).color(Color32::from_gray(170))).sense(Sense::click()));
                                    if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                        self.confirm_reset = false;
                                    }
                                });
                            }
                        });
                    });
                ctx.data_mut(|d| d.insert_temp(Id::new("lock_card_h"), fr.response.rect.height()));
                };
                // a card taller than the screen (landscape phone, keyboard open) scrolls; otherwise no scroll area at all
                if card_h > screen.height() - 16.0 {
                    egui::ScrollArea::vertical().max_height((screen.height() - top - 12.0).max(160.0)).show(ui, |ui| card(ui));
                } else {
                    card(ui);
                }


            });

        // footer
        let _ = (ease(1.0), with_alpha(Color32::WHITE, 1.0));
        // (hidden when the screen is too short: it used to be painted over the card's buttons)
        if screen.height() > 640.0 {
        Area::new(Id::new("lock_footer")).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -22.0)).show(ctx, |ui| {
            ui.set_max_width((screen.width() - 32.0).max(120.0));
            ui.label(
                RichText::new("Lumen · built with Rust & egui · your notes never leave this device")
                    .font(body(12.5))
                    .color(Color32::from_gray(105)),
            );
        });
        }
        event
    }
}
