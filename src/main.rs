//! locker — a fake lock screen.
//!
//! Shows a fullscreen image (or a built-in lock-screen lookalike) and exits
//! when the unlock code is typed. There is no input box and no feedback of
//! any kind while typing. Ctrl+Z suspends the process like a normal terminal
//! job; `fg` brings the lock screen back.
//!
//! This is a facade, not a security tool.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::Local;
use image::RgbImage;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowId};

const DEFAULT_CODE: &str = "unlock";

struct Config {
    code: String,
    image: Option<PathBuf>,
    /// Screen aliases from `screen.<alias> = <output>` lines, in file order.
    screens: Vec<(String, String)>,
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn load_config() -> Config {
    let mut config = Config {
        code: DEFAULT_CODE.to_string(),
        image: None,
        screens: Vec::new(),
    };
    let Some(home) = std::env::var_os("HOME") else {
        return config;
    };
    let Ok(text) = std::fs::read_to_string(PathBuf::from(home).join(".lockerrc")) else {
        return config;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "code" => config.code = value.to_string(),
            "image" => config.image = Some(expand_tilde(value)),
            k => {
                if let Some(alias) = k.strip_prefix("screen.") {
                    if !alias.is_empty() {
                        config.screens.push((alias.to_string(), value.to_string()));
                    }
                }
            }
        }
    }
    config
}

enum Mode {
    /// Lock the listed output names, or every monitor if None.
    Lock(Option<Vec<String>>),
    /// Print connected monitors and exit.
    List,
}

fn print_usage(config: &Config) {
    println!("usage: locker [--list] [screen ...]");
    println!();
    println!("With no screens given, every connected monitor is locked.");
    println!();
    if config.screens.is_empty() {
        println!("No screen aliases configured. Add lines to ~/.lockerrc like:");
        println!("  screen.laptop = eDP-1");
        println!("  screen.tv = HDMI-A-1");
        println!("then select screens with --laptop, -l, or bare: locker laptop");
    } else {
        println!("screens (from ~/.lockerrc):");
        for (alias, output) in &config.screens {
            let short = alias.chars().next().unwrap_or('?');
            println!("  --{alias}  (-{short})  ->  {output}");
        }
        println!("shorts combine: -lm. Raw output names also work: locker DP-1");
    }
    println!();
    println!("options:");
    println!("  --list      show connected monitor names and exit");
    println!("  --all       lock all monitors (same as no screens)");
    println!("  -h, --help  show this help");
}

fn usage_error(config: &Config, msg: &str) -> ! {
    eprintln!("locker: {msg}");
    eprintln!();
    print_usage(config);
    std::process::exit(2);
}

/// Resolve a screen selector: a configured alias maps to its output name,
/// anything else is taken as a raw output name (verified at window creation).
fn resolve_screen(config: &Config, name: &str) -> String {
    config
        .screens
        .iter()
        .find(|(alias, _)| alias.eq_ignore_ascii_case(name))
        .map(|(_, output)| output.clone())
        .unwrap_or_else(|| name.to_string())
}

fn parse_args(config: &Config) -> Mode {
    let mut selected: Vec<String> = Vec::new();
    let mut all = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--help" | "-h" => {
                print_usage(config);
                std::process::exit(0);
            }
            "--list" => return Mode::List,
            "--all" => all = true,
            s if s.starts_with("--") => selected.push(resolve_screen(config, &s[2..])),
            s if s.starts_with('-') && s.len() > 1 => {
                // Combined short flags: each letter is the first letter of a
                // configured alias.
                for ch in s[1..].chars() {
                    let matches: Vec<_> = config
                        .screens
                        .iter()
                        .filter(|(alias, _)| {
                            alias.chars().next().is_some_and(|c| c.eq_ignore_ascii_case(&ch))
                        })
                        .collect();
                    match matches.as_slice() {
                        [(_, output)] => selected.push(output.clone()),
                        [] => usage_error(config, &format!("no screen alias starts with '{ch}'")),
                        _ => usage_error(
                            config,
                            &format!("'-{ch}' is ambiguous; use the full --alias form"),
                        ),
                    }
                }
            }
            s => selected.push(resolve_screen(config, s)),
        }
    }
    selected.sort();
    selected.dedup();
    if all || selected.is_empty() {
        Mode::Lock(None)
    } else {
        Mode::Lock(Some(selected))
    }
}

/// Find a usable system font for the built-in lock screen. Best effort: if
/// nothing is found the default screen just renders without text.
fn load_font() -> Option<fontdue::Font> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(out) = Command::new("fc-match")
        .args(["-f", "%{file}", "sans-serif:bold"])
        .output()
    {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                candidates.push(PathBuf::from(path));
            }
        }
    }
    candidates.extend(
        [
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/gnu-free/FreeSansBold.otf",
        ]
        .iter()
        .map(PathBuf::from),
    );
    for path in candidates {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(font) = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                return Some(font);
            }
        }
    }
    None
}

struct WindowState {
    // Declaration order matters: surface must drop before context/window.
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    _context: softbuffer::Context<Rc<Window>>,
    window: Rc<Window>,
    /// Cover-scaled background cached for the current window size.
    scaled_bg: Option<(u32, u32, Vec<u32>)>,
}

struct App {
    code: String,
    image: Option<RgbImage>,
    font: Option<fontdue::Font>,
    windows: Vec<WindowState>,
    typed: String,
    modifiers: ModifiersState,
    /// Windows have been destroyed; stop the process on the next timer tick,
    /// after the event loop has flushed the destroy requests to the compositor.
    pending_suspend: bool,
    /// Output names to lock; None locks every monitor.
    selected: Option<Vec<String>>,
    /// Print monitors and exit instead of locking.
    list_only: bool,
    /// Alias table, for annotating --list output.
    screens: Vec<(String, String)>,
    /// Set when locking failed (e.g. no matching monitors).
    failed: bool,
}

impl App {
    fn list_monitors(&self, event_loop: &ActiveEventLoop) {
        let monitors: Vec<_> = event_loop.available_monitors().collect();
        if monitors.is_empty() {
            println!("no monitors detected");
        }
        for monitor in monitors {
            let name = monitor.name().unwrap_or_else(|| "<unnamed>".to_string());
            let size = monitor.size();
            let alias = self
                .screens
                .iter()
                .find(|(_, output)| output.eq_ignore_ascii_case(&name))
                .map(|(alias, _)| format!("  (alias: {alias})"))
                .unwrap_or_default();
            println!("{name}  {}x{}{alias}", size.width, size.height);
        }
    }

    fn create_windows(&mut self, event_loop: &ActiveEventLoop) {
        let monitors: Vec<_> = event_loop.available_monitors().collect();
        let targets: Vec<Option<MonitorHandle>> = match &self.selected {
            Some(wanted) => monitors
                .into_iter()
                .filter(|m| {
                    m.name()
                        .is_some_and(|n| wanted.iter().any(|w| n.eq_ignore_ascii_case(w)))
                })
                .map(Some)
                .collect(),
            None if monitors.is_empty() => vec![None],
            None => monitors.into_iter().map(Some).collect(),
        };
        if targets.is_empty() {
            eprintln!("locker: no connected monitor matches the requested screens; available:");
            self.list_monitors(event_loop);
            self.failed = true;
            event_loop.exit();
            return;
        }
        for monitor in targets {
            let attrs = Window::default_attributes()
                .with_title("locker")
                .with_decorations(false)
                .with_fullscreen(Some(Fullscreen::Borderless(monitor)));
            let Ok(window) = event_loop.create_window(attrs) else {
                continue;
            };
            let window = Rc::new(window);
            window.set_cursor_visible(false);
            let Ok(context) = softbuffer::Context::new(window.clone()) else {
                continue;
            };
            let Ok(surface) = softbuffer::Surface::new(&context, window.clone()) else {
                continue;
            };
            window.request_redraw();
            self.windows.push(WindowState {
                surface,
                _context: context,
                window,
                scaled_bg: None,
            });
        }
    }

    fn begin_suspend(&mut self) {
        // Drop every window now so the compositor unmaps them, then suspend on
        // the next wakeup (see new_events) once the requests have been flushed.
        self.windows.clear();
        self.typed.clear();
        self.modifiers = ModifiersState::empty();
        self.pending_suspend = true;
    }

    fn handle_key(&mut self, event_loop: &ActiveEventLoop, event: &winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        if self.modifiers.control_key() {
            if let Key::Character(c) = event.logical_key.as_ref() {
                if c.eq_ignore_ascii_case("z") {
                    self.begin_suspend();
                }
            }
            return;
        }
        let Some(text) = event.text.as_ref() else {
            return;
        };
        for ch in text.chars().filter(|c| !c.is_control()) {
            self.typed.push(ch);
        }
        let max = self.code.chars().count();
        while self.typed.chars().count() > max {
            self.typed.remove(0);
        }
        if self.typed == self.code {
            event_loop.exit();
        }
    }

    fn draw(&mut self, id: WindowId) {
        let Some(state) = self.windows.iter_mut().find(|w| w.window.id() == id) else {
            return;
        };
        let size = state.window.inner_size();
        let (w, h) = (size.width, size.height);
        let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return;
        };
        if state.surface.resize(nw, nh).is_err() {
            return;
        }
        let Ok(mut buffer) = state.surface.buffer_mut() else {
            return;
        };
        if let Some(img) = &self.image {
            if state.scaled_bg.as_ref().map(|(sw, sh, _)| (*sw, *sh)) != Some((w, h)) {
                state.scaled_bg = Some((w, h, scale_cover(img, w, h)));
            }
            buffer.copy_from_slice(&state.scaled_bg.as_ref().unwrap().2);
        } else {
            render_default(&mut buffer, w, h, self.font.as_ref());
        }
        let _ = buffer.present();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.list_only {
            self.list_monitors(event_loop);
            event_loop.exit();
            return;
        }
        if self.windows.is_empty() && !self.pending_suspend {
            self.create_windows(event_loop);
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            if self.pending_suspend {
                self.pending_suspend = false;
                // Stop our whole process group, exactly like Ctrl+Z in a
                // terminal app. Execution resumes here after `fg`.
                unsafe {
                    libc::kill(0, libc::SIGTSTP);
                }
                self.create_windows(event_loop);
            } else {
                for state in &self.windows {
                    state.window.request_redraw();
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            // Refuse to close: the screen is "locked".
            WindowEvent::CloseRequested => {}
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => self.handle_key(event_loop, &event),
            WindowEvent::Resized(_) => {
                if let Some(state) = self.windows.iter_mut().find(|w| w.window.id() == window_id) {
                    state.scaled_bg = None;
                    state.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.draw(window_id),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.pending_suspend {
            // Give the event loop one flush-and-sleep cycle so the window
            // destroy requests actually reach the compositor before we stop.
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(80),
            ));
        } else if self.image.is_none() && !self.windows.is_empty() {
            // The built-in screen shows a clock: wake up at the next second.
            let ms = 1000u64.saturating_sub(u64::from(Local::now().timestamp_subsec_millis()));
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(ms.clamp(20, 1000)),
            ));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

fn pack(r: u8, g: u8, b: u8) -> u32 {
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

/// Scale the image to cover w x h (preserving aspect, cropping overflow) and
/// return it as softbuffer 0RGB pixels.
fn scale_cover(img: &RgbImage, w: u32, h: u32) -> Vec<u32> {
    let (iw, ih) = img.dimensions();
    let scale = (f64::from(w) / f64::from(iw)).max(f64::from(h) / f64::from(ih));
    let nw = ((f64::from(iw) * scale).ceil() as u32).max(w);
    let nh = ((f64::from(ih) * scale).ceil() as u32).max(h);
    let resized = image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle);
    let (x0, y0) = ((nw - w) / 2, (nh - h) / 2);
    let mut out = Vec::with_capacity((w as usize) * (h as usize));
    for y in 0..h {
        for x in 0..w {
            let p = resized.get_pixel(x0 + x, y0 + y);
            out.push(pack(p[0], p[1], p[2]));
        }
    }
    out
}

fn blend_px(buf: &mut [u32], w: u32, h: u32, x: i64, y: i64, color: (u8, u8, u8), alpha: u8) {
    if x < 0 || y < 0 || x >= i64::from(w) || y >= i64::from(h) || alpha == 0 {
        return;
    }
    let idx = (y as usize) * (w as usize) + x as usize;
    let dst = buf[idx];
    let (dr, dg, db) = ((dst >> 16) as u8, (dst >> 8) as u8, dst as u8);
    let a = u32::from(alpha);
    let mix = |d: u8, s: u8| -> u8 {
        ((u32::from(d) * (255 - a) + u32::from(s) * a) / 255) as u8
    };
    buf[idx] = pack(mix(dr, color.0), mix(dg, color.1), mix(db, color.2));
}

fn draw_text(
    buf: &mut [u32],
    w: u32,
    h: u32,
    font: &fontdue::Font,
    text: &str,
    px: f32,
    center_x: f32,
    top_y: f32,
    color: (u8, u8, u8),
) {
    use fontdue::layout::{CoordinateSystem, Layout, TextStyle};
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.append(std::slice::from_ref(font), &TextStyle::new(text, px, 0));
    let glyphs = layout.glyphs();
    if glyphs.is_empty() {
        return;
    }
    let min_x = glyphs.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
    let max_x = glyphs
        .iter()
        .map(|g| g.x + g.width as f32)
        .fold(f32::NEG_INFINITY, f32::max);
    let offset_x = center_x - (min_x + max_x) / 2.0;
    for g in glyphs {
        let (metrics, bitmap) = font.rasterize_config(g.key);
        for by in 0..metrics.height {
            for bx in 0..metrics.width {
                let cov = bitmap[by * metrics.width + bx];
                blend_px(
                    buf,
                    w,
                    h,
                    (g.x + offset_x) as i64 + bx as i64,
                    (g.y + top_y) as i64 + by as i64,
                    color,
                    cov,
                );
            }
        }
    }
}

/// Draw a padlock icon centered at (cx, cy) with overall height s.
fn draw_lock_icon(buf: &mut [u32], w: u32, h: u32, cx: f32, cy: f32, s: f32, color: (u8, u8, u8)) {
    let body_w = s * 1.05;
    let body_h = s * 0.62;
    let body_top = cy + s * 0.5 - body_h;
    let body_cy = body_top + body_h / 2.0;
    let corner = s * 0.10;
    let ring_r = s * 0.30;
    let ring_t = s * 0.115;
    let key_r = s * 0.085;

    let inside = |px: f32, py: f32| -> bool {
        // Rounded-rect body.
        let qx = (px - cx).abs() - (body_w / 2.0 - corner);
        let qy = (py - body_cy).abs() - (body_h / 2.0 - corner);
        let body = qx.max(qy) <= 0.0
            || (qx > 0.0 && qy > 0.0 && qx * qx + qy * qy <= corner * corner);
        // Shackle: upper half-ring anchored at the body top.
        let dx = px - cx;
        let dy = py - body_top;
        let dist = (dx * dx + dy * dy).sqrt();
        let shackle = dy <= 0.0 && dist >= ring_r - ring_t / 2.0 && dist <= ring_r + ring_t / 2.0;
        // Keyhole cut out of the body.
        let kx = px - cx;
        let ky = py - (body_cy - body_h * 0.12);
        let keyhole = kx * kx + ky * ky <= key_r * key_r
            || (kx.abs() <= key_r * 0.45 && ky >= 0.0 && ky <= body_h * 0.30);
        (body && !keyhole) || shackle
    };

    let x_min = (cx - body_w).floor() as i64;
    let x_max = (cx + body_w).ceil() as i64;
    let y_min = (body_top - ring_r - ring_t).floor() as i64;
    let y_max = (body_top + body_h + 2.0).ceil() as i64;
    for y in y_min..=y_max {
        for x in x_min..=x_max {
            // 2x2 supersampling for soft edges.
            let mut hits = 0u32;
            for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                if inside(x as f32 + ox, y as f32 + oy) {
                    hits += 1;
                }
            }
            blend_px(buf, w, h, x, y, color, (hits * 255 / 4) as u8);
        }
    }
}

/// The built-in lock screen: dark gradient, padlock, live clock and date.
fn render_default(buf: &mut [u32], w: u32, h: u32, font: Option<&fontdue::Font>) {
    for y in 0..h {
        let t = f64::from(y) / f64::from(h.max(1));
        let r = (26.0 - 12.0 * t) as u8;
        let g = (27.0 - 13.0 * t) as u8;
        let b = (38.0 - 16.0 * t) as u8;
        let px = pack(r, g, b);
        let row = (y as usize) * (w as usize);
        buf[row..row + w as usize].fill(px);
    }

    let (cx, hf) = (w as f32 / 2.0, h as f32);
    let fg = (205u8, 211u8, 235u8);
    draw_lock_icon(buf, w, h, cx, hf * 0.26, hf * 0.085, fg);

    if let Some(font) = font {
        let now = Local::now();
        let time = now.format("%H:%M").to_string();
        let date = now.format("%A, %B %-d").to_string();
        draw_text(buf, w, h, font, &time, hf * 0.135, cx, hf * 0.36, fg);
        draw_text(buf, w, h, font, &date, hf * 0.032, cx, hf * 0.53, (150, 156, 180));
    }
}

fn main() {
    let config = load_config();
    let mode = parse_args(&config);
    let (list_only, selected) = match mode {
        Mode::List => (true, None),
        Mode::Lock(selected) => (false, selected),
    };
    let image = if list_only {
        None
    } else {
        config.image.as_ref().and_then(|path| match image::open(path) {
            Ok(img) => Some(img.to_rgb8()),
            Err(err) => {
                eprintln!(
                    "locker: cannot load image {}: {err}; using built-in lock screen",
                    path.display()
                );
                None
            }
        })
    };
    let mut app = App {
        code: config.code,
        image,
        font: if list_only { None } else { load_font() },
        windows: Vec::new(),
        typed: String::new(),
        modifiers: ModifiersState::empty(),
        pending_suspend: false,
        selected,
        list_only,
        screens: config.screens,
        failed: false,
    };
    let event_loop = EventLoop::new().expect("locker: failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut app).expect("locker: event loop error");
    if app.failed {
        std::process::exit(1);
    }
}
