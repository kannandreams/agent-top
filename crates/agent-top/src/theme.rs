//! Catppuccin Mocha and Latte, selected once before the TUI starts.
//!
//! Palette values: https://catppuccin.com/palette/ (checked 2026-09-17).
//! Query the terminal's actual foreground/background with OSC 10/11; fall
//! back to COLORFGBG, then Mocha. Detection never runs in non-interactive modes,
//! and `--theme dark|light` or `AGENT_TOP_THEME` skips it.

use ratatui::style::{Color, Style};
use std::io::{self, IsTerminal};
pub use terminal_colorsaurus::ThemeMode;

type Rgb = (u8, u8, u8);

pub struct Theme {
    pub background: Color,
    pub text: Color,
    pub dim: Color,
    pub border: Color,
    pub selection: Color,
    pub accent: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub mauve: Color,
    pub blue: Color,
    pub peach: Color,
    pub advice: Color,
    pub track: Color,
    pub ramp_ok: Ramp,
    pub ramp_subagent: Ramp,
    pub ramp_open: Ramp,
    pub ramp_error: Ramp,
    pub ramp_inference: Ramp,
    pub ramp_load: Ramp,
    truecolor: bool,
}

/// `--theme`. `Auto` asks the terminal; the other two skip the question.
#[derive(clap::ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    /// Catppuccin Mocha on a dark terminal, Latte on a light one.
    #[default]
    Auto,
    /// Catppuccin Mocha.
    Dark,
    /// Catppuccin Latte.
    Light,
}

impl ThemeChoice {
    pub fn flag(self) -> Option<&'static str> {
        match self {
            ThemeChoice::Auto => None,
            ThemeChoice::Dark => Some("dark"),
            ThemeChoice::Light => Some("light"),
        }
    }
}

/// The flag wins, then `AGENT_TOP_THEME`; `None` means detect. An unknown
/// value in the variable is ignored rather than refused, so a typo in a shell
/// profile cannot stop agent-top from starting.
fn forced_mode(choice: ThemeChoice, env: Option<&str>) -> Option<ThemeMode> {
    let named = |s: &str| match s.trim().to_ascii_lowercase().as_str() {
        "dark" | "mocha" => Some(ThemeMode::Dark),
        "light" | "latte" => Some(ThemeMode::Light),
        _ => None,
    };
    match choice {
        ThemeChoice::Dark => Some(ThemeMode::Dark),
        ThemeChoice::Light => Some(ThemeMode::Light),
        ThemeChoice::Auto => env.and_then(named),
    }
}

impl Theme {
    pub fn detect(choice: ThemeChoice) -> Self {
        let truecolor = std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
        if let Some(mode) = forced_mode(choice, std::env::var("AGENT_TOP_THEME").ok().as_deref()) {
            return Self::new(mode, truecolor);
        }
        let mode = if io::stdin().is_terminal() && io::stdout().is_terminal() {
            // The library bounds the wait to one second and restores raw mode
            // before returning. No other input reader has started yet.
            terminal_colorsaurus::theme_mode(Default::default()).ok()
        } else {
            None
        };
        let mode = select_mode(mode, std::env::var("COLORFGBG").ok().as_deref());
        Self::new(mode, truecolor)
    }

    pub fn new(mode: ThemeMode, truecolor: bool) -> Self {
        // Base, text, subtext, overlay1 borders, surface0 selection and
        // surface1 meter tracks. Latte uses subtext1 so secondary text stays
        // legible on selected rows even after 256-colour quantization.
        let pick = |mocha, latte| rgb(if mode == ThemeMode::Dark { mocha } else { latte });
        let base = pick(0x1e1e2e, 0xeff1f5);
        let text = pick(0xcdd6f4, 0x4c4f69);
        let dim = pick(0xa6adc8, 0x5c5f77);
        let border = pick(0x7f849c, 0x8c8fa1);
        let selection = pick(0x313244, 0xccd0da);
        let track = pick(0x45475a, 0xbcc0cc);
        let green = pick(0xa6e3a1, 0x40a02b);
        let yellow = pick(0xf9e2af, 0xdf8e1d);
        let red = pick(0xf38ba8, 0xd20f39);
        let maroon = pick(0xeba0ac, 0xe64553);
        let peach = pick(0xfab387, 0xfe640b);
        let blue = pick(0x89b4fa, 0x1e66f5);
        let teal = pick(0x94e2d5, 0x179299);
        let mauve = pick(0xcba6f7, 0x8839ef);
        let lavender = pick(0xb4befe, 0x7287fd);
        let rosewater = pick(0xf5e0dc, 0xdc8a78);
        let flamingo = pick(0xf2cdcd, 0xdd7878);
        let c = |rgb| terminal_color(rgb, truecolor);
        let ramp = |stops| Ramp { stops, truecolor };
        Self {
            background: c(base),
            text: c(text),
            dim: c(dim),
            border: c(border),
            selection: c(selection),
            accent: c(blue),
            // Shade Latte's brighter accents for small status text on both
            // base and selected rows. Meter stops keep the original colours.
            green: c(pick(0xa6e3a1, 0x2f7620)),
            yellow: c(pick(0xf9e2af, 0xa16615)),
            red: c(red),
            mauve: c(pick(0xcba6f7, 0x7c2fdc)),
            blue: c(blue),
            peach: c(pick(0xfab387, 0xc14c08)),
            advice: c(mauve),
            track: c(track),
            ramp_ok: ramp([green, yellow, peach]),
            ramp_subagent: ramp([blue, teal, mauve]),
            ramp_open: ramp([peach, yellow, rosewater]),
            ramp_error: ramp([red, maroon, flamingo]),
            ramp_inference: ramp([border, dim, lavender]),
            ramp_load: ramp([green, yellow, red]),
            truecolor,
        }
    }

    pub fn style(&self) -> Style {
        Style::default().fg(self.text).bg(self.background)
    }

    pub fn badge_style(&self, background: Color) -> Style {
        // Latte's saturated blue/mauve need light lettering, but yellow/peach
        // need dark lettering. A single inverse text colour cannot serve both.
        let bg = match background {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(i) => xterm_rgb(i),
            _ => return self.style(),
        };
        let dark = rgb(0x11111b); // Mocha crust
        // White rather than Latte base keeps small text on Latte blue above
        // 4.5:1 contrast; the slightly tinted base falls just below that.
        let light = rgb(0xffffff);
        let foreground = if contrast(dark, bg) >= contrast(light, bg) { dark } else { light };
        Style::default().fg(terminal_color(foreground, self.truecolor)).bg(background)
    }
}

fn select_mode(queried: Option<ThemeMode>, colorfgbg: Option<&str>) -> ThemeMode {
    queried.or_else(|| colorfgbg.and_then(mode_from_colorfgbg)).unwrap_or(ThemeMode::Dark)
}

fn mode_from_colorfgbg(value: &str) -> Option<ThemeMode> {
    // Both "foreground;background" and rxvt's "foreground;default;background".
    let (_, background) = value.rsplit_once(';')?;
    let index = background.parse::<u8>().ok()?;
    Some(if luminance(xterm_rgb(index)) > 0.179 { ThemeMode::Light } else { ThemeMode::Dark })
}

fn rgb(hex: u32) -> Rgb {
    ((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn luminance((r, g, b): Rgb) -> f64 {
    let linear = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// A three-stop colour ramp, interpolated across a meter's length.
pub struct Ramp {
    stops: [Rgb; 3],
    truecolor: bool,
}

impl Ramp {
    pub fn rgb_at(&self, t: f64) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let (a, b, local) = if t < 0.5 { (self.stops[0], self.stops[1], t * 2.0) } else { (self.stops[1], self.stops[2], (t - 0.5) * 2.0) };
        let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * local).round() as u8;
        (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
    }

    pub fn at(&self, t: f64) -> Color {
        terminal_color(self.rgb_at(t), self.truecolor)
    }
}

fn terminal_color(rgb: Rgb, truecolor: bool) -> Color {
    if truecolor { Color::Rgb(rgb.0, rgb.1, rgb.2) } else { Color::Indexed(xterm256(rgb)) }
}

const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn xterm_rgb(index: u8) -> Rgb {
    match index {
        0..=15 => rgb([
            0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0x808080, 0xff0000, 0x00ff00, 0xffff00,
            0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
        ][index as usize]),
        16..=231 => {
            let i = (index - 16) as usize;
            (CUBE[i / 36], CUBE[(i / 6) % 6], CUBE[i % 6])
        }
        _ => {
            let grey = 8 + (index - 232) * 10;
            (grey, grey, grey)
        }
    }
}

/// Closest fixed xterm colour: compare the nearest cube entry with the nearest
/// grey. Never use ANSI indices 0..15, which the terminal theme can redefine.
fn xterm256(color: Rgb) -> u8 {
    let axis = |v: u8| (0..6).min_by_key(|&i| CUBE[i].abs_diff(v)).unwrap() as u8;
    let (r, g, b) = color;
    let cube = 16 + 36 * axis(r) + 6 * axis(g) + axis(b);
    let mean = (r as f64 + g as f64 + b as f64) / 3.0;
    let grey = 232 + ((mean - 8.0) / 10.0).round().clamp(0.0, 23.0) as u8;
    let distance = |index| {
        let (r2, g2, b2) = xterm_rgb(index);
        (r as i32 - r2 as i32).pow(2) + (g as i32 - g2 as i32).pow(2) + (b as i32 - b2 as i32).pow(2)
    };
    if distance(grey) < distance(cube) { grey } else { cube }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_wins_then_the_variable_and_a_typo_falls_back_to_detection() {
        assert_eq!(forced_mode(ThemeChoice::Light, Some("dark")), Some(ThemeMode::Light));
        assert_eq!(forced_mode(ThemeChoice::Dark, None), Some(ThemeMode::Dark));
        assert_eq!(forced_mode(ThemeChoice::Auto, Some("light")), Some(ThemeMode::Light));
        assert_eq!(forced_mode(ThemeChoice::Auto, Some(" Mocha ")), Some(ThemeMode::Dark));
        assert_eq!(forced_mode(ThemeChoice::Auto, Some("latte")), Some(ThemeMode::Light));
        assert_eq!(forced_mode(ThemeChoice::Auto, Some("solarized")), None);
        assert_eq!(forced_mode(ThemeChoice::Auto, None), None);
    }

    #[test]
    fn terminal_response_wins_over_environment_and_unknown_defaults_dark() {
        assert_eq!(select_mode(Some(ThemeMode::Light), Some("15;0")), ThemeMode::Light);
        assert_eq!(select_mode(Some(ThemeMode::Dark), Some("0;15")), ThemeMode::Dark);
        assert_eq!(select_mode(None, Some("0;15")), ThemeMode::Light);
        assert_eq!(select_mode(None, Some("15;0")), ThemeMode::Dark);
        assert_eq!(select_mode(None, None), ThemeMode::Dark);
        assert_eq!(select_mode(None, Some("invalid")), ThemeMode::Dark);
    }

    #[test]
    fn colorfgbg_uses_the_last_field_and_understands_256_colors() {
        for value in ["0;15", "0;7", "0;default;15", "0;231", "0;255"] {
            assert_eq!(mode_from_colorfgbg(value), Some(ThemeMode::Light), "{value}");
        }
        for value in ["15;0", "15;default;0", "15;16", "15;232", "15;235"] {
            assert_eq!(mode_from_colorfgbg(value), Some(ThemeMode::Dark), "{value}");
        }
        for value in ["", "15", "0;", "0;default", "0;256", "0;-1", "0;light"] {
            assert_eq!(mode_from_colorfgbg(value), None, "{value}");
        }
    }

    #[test]
    fn palettes_use_catppuccin_base_text_and_accent() {
        let mocha = Theme::new(ThemeMode::Dark, true);
        let latte = Theme::new(ThemeMode::Light, true);
        assert_eq!(mocha.background, Color::Rgb(30, 30, 46));
        assert_eq!(mocha.text, Color::Rgb(205, 214, 244));
        assert_eq!(mocha.accent, Color::Rgb(137, 180, 250));
        assert_eq!(latte.background, Color::Rgb(239, 241, 245));
        assert_eq!(latte.text, Color::Rgb(76, 79, 105));
        assert_eq!(latte.accent, Color::Rgb(30, 102, 245));
    }

    fn color_rgb(color: Color) -> Rgb {
        match color {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(i) => xterm_rgb(i),
            _ => panic!("theme leaked an ANSI/default colour: {color:?}"),
        }
    }

    #[test]
    fn text_selections_and_badges_contrast_in_both_color_depths() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            for truecolor in [true, false] {
                let theme = Theme::new(mode, truecolor);
                for bg in [theme.background, theme.selection] {
                    assert!(contrast(color_rgb(theme.text), color_rgb(bg)) >= 4.5);
                    assert!(contrast(color_rgb(theme.dim), color_rgb(bg)) >= 3.0, "{mode:?}: {:?} on {bg:?}", theme.dim);
                    for fg in [theme.accent, theme.green, theme.yellow, theme.peach, theme.red, theme.mauve] {
                        assert!(contrast(color_rgb(fg), color_rgb(bg)) >= 3.0, "{mode:?}: {fg:?} on {bg:?}");
                    }
                }
                for bg in [theme.accent, theme.yellow, theme.peach, theme.red, theme.mauve, theme.dim] {
                    let fg = theme.badge_style(bg).fg.unwrap();
                    assert!(contrast(color_rgb(fg), color_rgb(bg)) >= 4.5, "{mode:?}: {fg:?} on {bg:?}");
                }
            }
        }
    }

    #[test]
    fn ramps_interpolate_catppuccin_stops_and_clamp() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let theme = Theme::new(mode, true);
            let ramp = &theme.ramp_ok;
            let stops = match mode {
                ThemeMode::Dark => [0xa6e3a1, 0xf9e2af, 0xfab387],
                ThemeMode::Light => [0x40a02b, 0xdf8e1d, 0xfe640b],
            };
            for (t, stop) in [0.0, 0.5, 1.0].into_iter().zip(stops) {
                assert_eq!(ramp.rgb_at(t), rgb(stop));
            }
            assert_eq!(ramp.rgb_at(-1.0), ramp.rgb_at(0.0));
            assert_eq!(ramp.rgb_at(4.0), ramp.rgb_at(1.0));
            assert_ne!(ramp.at(0.1), ramp.at(0.9));
            for i in 0..=10 {
                let (r, g, b) = theme.ramp_error.rgb_at(i as f64 / 10.0);
                assert!(r > g && r > b, "error ramp left the red family");
            }
        }
    }

    #[test]
    fn fixed_256_palette_preserves_exact_entries_and_avoids_ansi() {
        for i in 16..=255 {
            assert_eq!(xterm_rgb(xterm256(xterm_rgb(i))), xterm_rgb(i));
        }
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let theme = Theme::new(mode, false);
            for ramp in [&theme.ramp_ok, &theme.ramp_subagent, &theme.ramp_open, &theme.ramp_error, &theme.ramp_inference, &theme.ramp_load]
            {
                for i in 0..=20 {
                    assert!(matches!(ramp.at(i as f64 / 20.0), Color::Indexed(16..=255)));
                }
            }
        }
    }
}
