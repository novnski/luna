//! A lit, text-rendered lunar sphere. Only its surface turns; the text grid and
//! surrounding layout stay fixed. The view parks after a single hover flourish.

use crate::{motion, theme::Theme};
use gpui::{Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};
use std::{
    f32::consts::{PI, TAU},
    sync::OnceLock,
    time::Instant,
};

const COLUMNS: usize = 72;
const ROWS: usize = 39;
const TEXTURE_WIDTH: usize = 144;
const TEXTURE_HEIGHT: usize = 72;
const TURN_SECONDS: f32 = 4.8;
const GLYPHS: [char; 7] = [' ', '·', ':', '░', '▒', '▓', '█'];

struct Sample {
    longitude: f32,
    latitude: f32,
    light: f32,
}

struct Surface {
    cells: Vec<Option<Sample>>,
    texture: Vec<f32>,
}

impl Surface {
    fn shared() -> &'static Self {
        static SURFACE: OnceLock<Surface> = OnceLock::new();
        SURFACE.get_or_init(Self::new)
    }

    fn new() -> Self {
        // Preserve the existing NASA-derived near-side artwork. The remaining
        // hemisphere is procedural illustration, not a scientific far-side map.
        let source: Vec<Vec<char>> = include_str!("../assets/illustrations/moon-full-ascii.txt")
            .lines()
            .map(|line| line.chars().collect())
            .collect();
        let source_width = source.iter().map(Vec::len).max().unwrap_or(1);
        let mut seed = 38_u32;
        let mut random = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed as f32 / u32::MAX as f32
        };
        let craters: Vec<([f32; 3], f32)> = (0..46)
            .map(|_| {
                let y = random() * 1.8 - 0.9;
                let longitude = random() * TAU;
                let radius = (1.0 - y * y).sqrt();
                (
                    [radius * longitude.sin(), y, radius * longitude.cos()],
                    0.035 + random() * 0.13,
                )
            })
            .collect();
        let mut texture = Vec::with_capacity(TEXTURE_WIDTH * TEXTURE_HEIGHT);
        for row in 0..TEXTURE_HEIGHT {
            let latitude = (row as f32 + 0.5) / TEXTURE_HEIGHT as f32 * PI;
            for col in 0..TEXTURE_WIDTH {
                let longitude = col as f32 / TEXTURE_WIDTH as f32 * TAU;
                let x = longitude.sin() * latitude.sin();
                let y = -latitude.cos();
                let z = longitude.cos() * latitude.sin();
                let mut albedo = 0.72
                    + 0.08 * (19.0 * x + 7.0 * z).sin() * (22.0 * y - 9.0 * z).sin()
                    + 0.05 * (41.0 * x + 33.0 * y + 23.0 * z).sin();
                for (center, radius) in &craters {
                    let dot = x * center[0] + y * center[1] + z * center[2];
                    let distance = 2.0 * (1.0 - dot) / (radius * radius);
                    if distance < 2.0 {
                        albedo -= 0.17 * (-distance * 4.0).exp();
                        albedo += 0.10 * (-(distance - 0.8).powi(2) * 28.0).exp();
                    }
                }
                if z > 0.0 {
                    let sx = (((x + 1.0) * 0.5 * (source_width - 1) as f32).round() as usize)
                        .min(source_width - 1);
                    let sy = (((y + 1.0) * 0.5 * (source.len() - 1) as f32).round() as usize)
                        .min(source.len() - 1);
                    let density = match source[sy].get(sx).copied().unwrap_or(' ') {
                        '█' => 1.0,
                        '▓' => 0.80,
                        '▒' => 0.56,
                        '░' => 0.30,
                        _ => 0.12,
                    };
                    let blend = (z / 0.3).clamp(0.0, 1.0);
                    let blend = blend * blend * (3.0 - 2.0 * blend);
                    albedo = albedo * (1.0 - blend) + (0.3 + density * 0.7) * blend;
                }
                texture.push(albedo.clamp(0.15, 1.0));
            }
        }
        let mut cells = Vec::with_capacity(COLUMNS * ROWS);
        for row in 0..ROWS {
            for col in 0..COLUMNS {
                let x = (col as f32 - (COLUMNS - 1) as f32 * 0.5) / (COLUMNS as f32 * 0.5 - 2.0);
                let y = (row as f32 - (ROWS - 1) as f32 * 0.5) / (ROWS as f32 * 0.5 - 1.0);
                let radius = x * x + y * y;
                cells.push((radius < 1.0).then(|| {
                    let z = (1.0 - radius).sqrt();
                    Sample {
                        longitude: x.atan2(z) / TAU,
                        latitude: (-y).acos() / PI,
                        light: 0.075 + ((-0.62 * x - 0.5 * y + z) / 1.279).max(0.0) * 0.94,
                    }
                }));
            }
        }
        Self { cells, texture }
    }

    fn rows(&self, turn: f32, light_appearance: bool) -> Vec<SharedString> {
        self.cells
            .chunks(COLUMNS)
            .map(|row| {
                let text: String = row
                    .iter()
                    .map(|cell| {
                        let Some(sample) = cell else {
                            return ' ';
                        };
                        let u = (sample.longitude + turn).rem_euclid(1.0) * TEXTURE_WIDTH as f32;
                        let x = u.floor() as usize % TEXTURE_WIDTH;
                        let y = ((sample.latitude * TEXTURE_HEIGHT as f32) as usize)
                            .min(TEXTURE_HEIGHT - 1);
                        let next = (x + 1) % TEXTURE_WIDTH;
                        let a = self.texture[y * TEXTURE_WIDTH + x];
                        let b = self.texture[y * TEXTURE_WIDTH + next];
                        let value = (a + (b - a) * u.fract()) * sample.light;
                        // On white paper, low ink coverage means brighter illumination.
                        let density = if light_appearance {
                            0.94 - value * 0.9
                        } else {
                            value
                        };
                        GLYPHS[(density * (GLYPHS.len() - 1) as f32)
                            .round()
                            .clamp(1.0, (GLYPHS.len() - 1) as f32)
                            as usize]
                    })
                    .collect();
                SharedString::from(text.trim_end().to_owned())
            })
            .collect()
    }
}

pub(crate) struct AsciiMoon {
    diameter: f32,
    started: Option<Instant>,
    cached_turn: f32,
    cached_light: bool,
    rows: Vec<SharedString>,
    #[cfg(feature = "appshots-fixture")]
    frame_requests: u64,
    #[cfg(feature = "appshots-fixture")]
    bounds: Option<gpui::Bounds<gpui::Pixels>>,
}

#[cfg(feature = "appshots-fixture")]
pub struct MoonFixtureState {
    pub started: Option<Instant>,
    pub frame_requests: u64,
    pub rows: Vec<SharedString>,
    pub bounds: Option<gpui::Bounds<gpui::Pixels>>,
}

impl AsciiMoon {
    pub(crate) fn new(diameter: f32) -> Self {
        Self {
            diameter,
            started: None,
            cached_turn: 0.0,
            cached_light: false,
            rows: Surface::shared().rows(0.0, false),
            #[cfg(feature = "appshots-fixture")]
            frame_requests: 0,
            #[cfg(feature = "appshots-fixture")]
            bounds: None,
        }
    }

    pub(crate) fn set_diameter(&mut self, diameter: f32, cx: &mut Context<Self>) {
        if (self.diameter - diameter).abs() > 0.1 {
            self.diameter = diameter;
            cx.notify();
        }
    }

    #[cfg(feature = "appshots-fixture")]
    pub(crate) fn fixture_state(&self) -> MoonFixtureState {
        MoonFixtureState {
            started: self.started,
            frame_requests: self.frame_requests,
            rows: self.rows.clone(),
            bounds: self.bounds,
        }
    }
}

impl Render for AsciiMoon {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let duration = TURN_SECONDS * motion::speed_scale().max(0.01);
        let progress = self
            .started
            .map(|start| start.elapsed().as_secs_f32() / duration);
        let turn = if !motion::reduced_motion(cx) && progress.is_some_and(|p| p < 1.0) {
            window.request_animation_frame();
            #[cfg(feature = "appshots-fixture")]
            {
                self.frame_requests += 1;
            }
            let p = progress.unwrap();
            p * p * (3.0 - 2.0 * p)
        } else {
            self.started = None;
            0.0
        };
        let theme = Theme::of(cx);
        let light = theme.appearance.is_light();
        if self.cached_turn != turn || self.cached_light != light {
            self.rows = Surface::shared().rows(turn, light);
            self.cached_turn = turn;
            self.cached_light = light;
        }
        let font = gpui::Font {
            family: theme.font_mono.clone(),
            ..Default::default()
        };
        let font_id = window.text_system().resolve_font(&font);
        let advance = window
            .text_system()
            .advance(font_id, px(1.0), '0')
            .map(|size| f32::from(size.width))
            .unwrap_or(0.6)
            .max(0.1);
        let line_height = self.diameter / ROWS as f32;
        let content = div()
            .id("empty-project-moon")
            .role(gpui::accesskit::Role::Image)
            .aria_label("Moon")
            .size(px(self.diameter))
            .overflow_hidden()
            .font_family(theme.font_mono.clone())
            .text_size(px(self.diameter / (COLUMNS as f32 * advance)))
            .line_height(px(line_height))
            .text_color(theme.text.opacity(if light {
                0.64
            } else {
                0.72 + 0.12 * (PI * turn).sin()
            }))
            .children(
                self.rows
                    .iter()
                    .cloned()
                    .map(|line| div().h(px(line_height)).whitespace_nowrap().child(line)),
            );
        #[cfg(feature = "appshots-fixture")]
        let content = {
            let view = cx.entity().downgrade();
            content.relative().child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        let _ = view.update(cx, |view, _| view.bounds = Some(bounds));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
        };
        content.on_hover(cx.listener(|this, hovered, _, cx| {
            if *hovered && this.started.is_none() && !motion::reduced_motion(cx) {
                this.started = Some(Instant::now());
                cx.notify();
            }
        }))
    }
}
