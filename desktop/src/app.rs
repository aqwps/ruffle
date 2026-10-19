use crate::custom_event::{OpenType, RuffleEvent};
use crate::gui::{GuiController, MENU_HEIGHT};
use crate::player::{LaunchOptions, PlayerController};
use crate::preferences::GlobalPreferences;
use crate::util::{
    get_screen_size, gilrs_button_to_gamepad_button, winit_input_to_ruffle_key_descriptor,
    winit_to_ruffle_text_control,
};
use anyhow::Error;
use gilrs::{Event, EventType, Gilrs};
use ruffle_core::FloatDuration;
use ruffle_core::PlayerEvent;
use ruffle_core::events::{ImeEvent, ImeNotification, PlayerNotification};
use ruffle_core::swf::HeaderExt;
use ruffle_frontend_utils::content::ContentDescriptor;
use ruffle_render::backend::ViewportDimensions;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize, Size};
use winit::event::{ElementState, Ime, KeyEvent, Modifiers, StartCause, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Icon, WindowAttributes, WindowId};


/// AQW build: the time each `perf_counters::timers` timer added since `before`, in ms
/// (only the nonzero ones).
fn timer_parts(before: &[u64; ruffle_render_wgpu::perf_counters::timers::LEN]) -> String {
    use ruffle_render_wgpu::perf_counters::timers;
    let parts = timers::NAMES
        .iter()
        .zip(timers::snapshot().iter().zip(before))
        .filter(|(_, (a, b))| *a > *b)
        .map(|(name, (a, b))| format!("{name} {:.1}", (a - b) as f64 / 1000.0))
        .collect::<Vec<_>>()
        .join(", ");
    if parts.is_empty() { "none of the timed work".to_string() } else { parts }
}

struct MainWindow {
    preferences: GlobalPreferences,
    gui: GuiController,
    player: PlayerController,
    minimized: bool,
    /// Set from `WindowEvent::Occluded`, which winit only reports on macOS and X11
    /// (and on X11 only without a compositor). Always false elsewhere.
    occluded: bool,
    mouse_pos: PhysicalPosition<f64>,

    /// Whether `mouse_pos` changed since the last `MouseMove` sent to the player.
    /// Moves are coalesced, so that a movie with slow mouse handlers sees only
    /// the latest position instead of every move queued while it was busy.
    mouse_move_pending: bool,

    modifiers: Modifiers,
    min_window_size: LogicalSize<u32>,
    max_window_size: PhysicalSize<u32>,
    no_gui: bool,
    preferred_width: Option<f64>,
    preferred_height: Option<f64>,
    start_fullscreen: bool,
    loaded: LoadingState,
    time: Instant,
    next_frame_time: Option<Instant>,
    event_loop_proxy: EventLoopProxy<RuffleEvent>,
}

impl MainWindow {
    pub fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        if matches!(event, WindowEvent::RedrawRequested) {
            // Don't render when nobody can see the result: while minimized, the
            // surface also keeps the size it had before, as `GuiController::resize`
            // refuses to reconfigure it to a zero size.
            if !self.minimized && !self.occluded {
                let started = Instant::now();
                let timers_before = ruffle_render_wgpu::perf_counters::timers::snapshot();
                let walk_before = ruffle_core::render_walk_micros();
                let mut player = self.player.get();
                let frame_budget_ms = player
                    .as_ref()
                    .map_or(1000.0 / 24.0, |player| 1000.0 / player.frame_rate());
                let mut rendered_movie = false;
                if let Some(ref mut player) = player {
                    // AQW build: egui asks for a repaint on every mouse move, and each repaint
                    // used to re-render the whole movie too. The movie view keeps the last
                    // frame, so only re-render it when a new SWF frame ran (or it was resized).
                    // Even if the movie is paused, user interaction with debug tools can change the render output

                    // With frame interpolation on, a new in-between picture goes out on the
                    // first refresh after it's due, every n refreshes' time; the refreshes in
                    // between show the last one again (see `pace_interpolation`). Timed, not
                    // counted: a long draw (e.g. 11 ms in Yulgar) makes presents miss refreshes.
                    let interpolating = self.next_draw_time.is_some();
                    let new_picture_due = if interpolating {
                        let interval = self.refresh_interval * self.draw_divisor;
                        let due = self.picture_due + interval;
                        if started + self.refresh_interval / 2 >= due {
                            // Keep the rhythm, unless it fell more than a picture behind.
                            self.picture_due = if started < due + interval {
                                due
                            } else {
                                started
                            };
                            true
                        } else {
                            false
                        }
                    } else {
                        self.frame_since_render
                    };
                    if new_picture_due || !player.is_playing() || extra_renders_enabled() {
                        player.render();
                        rendered_movie = true;
                    }
                }
                let movie_ms = started.elapsed().as_secs_f64() * 1000.0;

                self.gui.render(player);
                self.perf.presents += 1;
                self.count_fps();
                self.frame_since_render = false;
                self.last_redraw = Instant::now();
                self.draw_started = started;
                if rendered_movie {
                    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
                    // Drawing the movie only (presenting can include waiting for the refresh);
                    // one-off slow draws, e.g. of new shapes, count at most 3x the average.
                    let cost = movie_ms.min(self.draw_cost_ms * 3.0);
                    self.draw_cost_ms = self.draw_cost_ms * 0.9 + cost * 0.1;
                    self.perf.render(total_ms);
                    // AQW build: say what a slow render (longer than 1.2 frames) spent its
                    // time on.
                    if total_ms > frame_budget_ms * 0.9 {
                        let parts = timer_parts(&timers_before);
                        let walk_ms =
                            (ruffle_core::render_walk_micros() - walk_before) as f64 / 1000.0;
                        tracing::info!(
                            target: "ruffle_aqw_perf",
                            "slow render {total_ms:.0} ms: movie {movie_ms:.0} ms, window/present {:.0} ms; build_commands {walk_ms:.1}, {parts} (ms)",
                            total_ms - movie_ms
                        );
                    }
                }
            }
            // Important that we return here, or we'll get a feedback loop with egui
            // (winit says redraw, egui hears redraw and says redraw, we hear redraw and tell winit to redraw...)
            return;
        }

        if self.gui.handle_event(&event) {
            // Event consumed by GUI.
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                // How minimizing shows up on Windows; winit still has no
                // `WindowEvent::Minimize`. (`Window::is_minimized` does exist by
                // now, but it has to be polled, and it wouldn't tell us anything
                // new: macOS reports minimized windows as occluded instead, and
                // on Wayland it always returns `None`.)
                self.minimized = size.width == 0 && size.height == 0;

                if let Some(mut player) = self.player.get() {
                    let viewport_scale_factor = self.gui.window().scale_factor();
                    player.set_viewport_dimensions(ViewportDimensions {
                        width: size.width,
                        height: size.height.saturating_sub(self.gui.height_offset() as u32),
                        scale_factor: viewport_scale_factor,
                    });
                }
                self.gui.window().request_redraw();
                if matches!(self.loaded, LoadingState::WaitingForResize) {
                    self.loaded = LoadingState::Loaded;
                }
            }
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if !occluded {
                    // Nothing repaints us on its own if the movie is paused.
                    self.gui.window().request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.gui.is_context_menu_visible() {
                    return;
                }

                // This is needed because some platforms (like macOS)
                // will fire this event at the same time as WindowEvent::MouseInput,
                // which may cause inaccurate behavior in the movie
                if position == self.mouse_pos {
                    return;
                }

                self.mouse_pos = position;

                // We can't handle mouse move here, doing so would put Ruffle in
                // a busy loop processing every queued CursorMoved event. Flash
                // Player does not queue mouse moves and instead skips them
                // if they happen when the movie is busy.
                self.mouse_move_pending = true;
            }
            WindowEvent::DroppedFile(file) => {
                if let Some(content_descriptor) = ContentDescriptor::new_local(&file, None) {
                    self.gui.create_movie(
                        &mut self.player,
                        LaunchOptions::from(&self.preferences),
                        content_descriptor,
                    );
                }
            }
            WindowEvent::Focused(true) => {
                self.handle_player_event(PlayerEvent::FocusGained);
            }
            WindowEvent::Focused(false) => {
                self.handle_player_event(PlayerEvent::FocusLost);
            }
            WindowEvent::MouseInput { button, state, .. } => {
                if self.gui.is_context_menu_visible() {
                    return;
                }

                use ruffle_core::events::MouseButton as RuffleMouseButton;
                use winit::event::MouseButton;
                let (x, y) = self.gui.window_to_movie_position(self.mouse_pos);
                let button = match button {
                    MouseButton::Left => RuffleMouseButton::Left,
                    MouseButton::Right => RuffleMouseButton::Right,
                    MouseButton::Middle => RuffleMouseButton::Middle,
                    _ => RuffleMouseButton::Unknown,
                };
                let event = match state {
                    // TODO We should get information about click index from the OS,
                    //   but winit does not support that yet.
                    ElementState::Pressed => PlayerEvent::MouseDown {
                        x,
                        y,
                        button,
                        index: None,
                    },
                    ElementState::Released => PlayerEvent::MouseUp { x, y, button },
                };
                let handled = self.handle_player_event(event);
                if !handled && state == ElementState::Pressed && button == RuffleMouseButton::Right
                {
                    // Show context menu.
                    if let Some(mut player) = self.player.get() {
                        let context_menu = player.prepare_context_menu();

                        // MouseUp event will be ignored when the context menu is shown,
                        // but it has to be dispatched when the menu closes.
                        let close_event = PlayerEvent::MouseUp {
                            x,
                            y,
                            button: RuffleMouseButton::Right,
                        };
                        self.gui.show_context_menu(context_menu, close_event);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.gui.is_context_menu_visible() {
                    return;
                }

                use ruffle_core::events::MouseWheelDelta;
                use winit::event::MouseScrollDelta;
                let delta = match delta {
                    MouseScrollDelta::LineDelta(_, dy) => MouseWheelDelta::Lines(dy.into()),
                    MouseScrollDelta::PixelDelta(pos) => MouseWheelDelta::Pixels(pos.y),
                };
                let event = PlayerEvent::MouseWheel { delta };
                self.handle_player_event(event);
            }
            WindowEvent::CursorEntered { .. } => {
                if let Some(mut player) = self.player.get() {
                    player.set_mouse_in_stage(true);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                // The movie has to see the latest position while the mouse
                // is still in the stage.
                self.flush_mouse_move();
                if let Some(mut player) = self.player.get() {
                    player.set_mouse_in_stage(false);
                }
                self.handle_player_event(PlayerEvent::MouseLeave);
            }
            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.modifiers = new_modifiers;
            }
            WindowEvent::Touch(winit::event::Touch {
                phase, location, ..
            }) => {
                self.mouse_pos = location;
                let (x, y) = self.gui.window_to_movie_position(location);
                use ruffle_core::events::MouseButton as RuffleMouseButton;
                if phase == TouchPhase::Started {
                    let event = PlayerEvent::MouseDown {
                        x,
                        y,
                        button: RuffleMouseButton::Left,
                        index: None,
                    };
                    self.handle_player_event(event);
                } else if phase == TouchPhase::Moved {
                    let event = PlayerEvent::MouseMove { x, y };
                    self.handle_player_event(event);
                } else if phase == TouchPhase::Ended {
                    let event = PlayerEvent::MouseUp {
                        x,
                        y,
                        button: RuffleMouseButton::Left,
                    };
                    self.handle_player_event(event);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if self.gui.is_context_menu_visible() {
                    return;
                }

                // Handle escaping from fullscreen.
                if let KeyEvent {
                    state: ElementState::Pressed,
                    logical_key: Key::Named(NamedKey::Escape),
                    ..
                } = event
                {
                    let _ = self
                        .event_loop_proxy
                        .send_event(RuffleEvent::ExitFullScreen);
                }

                let key = winit_input_to_ruffle_key_descriptor(&event);
                match event.state {
                    ElementState::Pressed => {
                        self.handle_player_event(PlayerEvent::KeyDown { key });
                        if let Some(control_code) =
                            winit_to_ruffle_text_control(&event, self.modifiers)
                        {
                            self.handle_player_event(PlayerEvent::TextControl {
                                code: control_code,
                            });
                        } else if let Some(text) = event.text {
                            for codepoint in text.chars() {
                                self.handle_player_event(PlayerEvent::TextInput { codepoint });
                            }
                        }
                    }
                    ElementState::Released => {
                        self.handle_player_event(PlayerEvent::KeyUp { key });
                    }
                };
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => {}
                Ime::Preedit(text, cursor) => {
                    self.handle_player_event(PlayerEvent::Ime(ImeEvent::Preedit(text, cursor)));
                }
                Ime::Commit(text) => {
                    self.handle_player_event(PlayerEvent::Ime(ImeEvent::Commit(text)));
                }
                Ime::Disabled => {}
            },
            _ => (),
        }
    }

    fn on_metadata(&mut self, swf_header: HeaderExt) {
        let height_offset = if self.gui.window().fullscreen().is_some() || self.no_gui {
            0.0
        } else {
            MENU_HEIGHT as f64
        };

        // To prevent issues like waiting on resize indefinitely (#11364) or desyncing the window state on Windows,
        // do not resize while window is maximized.
        let should_resize = !self.gui.window().is_maximized();

        let (viewport_size, state) = if should_resize {
            let movie_width = swf_header.stage_size().width().to_pixels();
            let movie_height = swf_header.stage_size().height().to_pixels();

            let window_size: Size = match (self.preferred_width, self.preferred_height) {
                (None, None) => LogicalSize::new(movie_width, movie_height + height_offset).into(),
                (Some(width), None) => {
                    let scale = width / movie_width;
                    let height = movie_height * scale;
                    PhysicalSize::new(
                        width.max(1.0),
                        height.max(1.0) + height_offset * self.gui.window().scale_factor(),
                    )
                    .into()
                }
                (None, Some(height)) => {
                    let scale = height / movie_height;
                    let width = movie_width * scale;
                    PhysicalSize::new(
                        width.max(1.0),
                        height.max(1.0) + height_offset * self.gui.window().scale_factor(),
                    )
                    .into()
                }
                (Some(width), Some(height)) => PhysicalSize::new(
                    width.max(1.0),
                    height.max(1.0) + height_offset * self.gui.window().scale_factor(),
                )
                .into(),
            };

            let window_size = Size::clamp(
                window_size,
                self.min_window_size.into(),
                self.max_window_size.into(),
                self.gui.window().scale_factor(),
            );

            let viewport_size = self.gui.window().inner_size();
            let mut window_resize_denied = false;

            if let Some(new_viewport_size) = self.gui.window().request_inner_size(window_size) {
                if new_viewport_size != viewport_size {
                    self.gui.resize(new_viewport_size);
                } else {
                    tracing::warn!("Unable to resize window");
                    window_resize_denied = true;
                }
            }

            let viewport_size = self.gui.window().inner_size();

            // On X11 (and possibly other platforms), the window size is not updated immediately.
            // On a successful resize request, wait for the window to be resized to the requested size
            // before we start running the SWF (which can observe the viewport size in "noScale" mode)
            let state = if !window_resize_denied && window_size != viewport_size.into() {
                LoadingState::WaitingForResize
            } else {
                LoadingState::Loaded
            };

            (viewport_size, state)
        } else {
            (self.gui.window().inner_size(), LoadingState::Loaded)
        };

        self.loaded = state;

        self.gui.window().set_fullscreen(if self.start_fullscreen {
            Some(Fullscreen::Borderless(None))
        } else {
            None
        });
        self.gui.window().set_visible(true);

        let viewport_scale_factor = self.gui.window().scale_factor();
        if let Some(mut player) = self.player.get() {
            player.set_viewport_dimensions(ViewportDimensions {
                width: viewport_size.width,
                height: viewport_size.height - (height_offset * viewport_scale_factor) as u32,
                scale_factor: viewport_scale_factor,
            });
        }
    }

    fn about_to_wait(&mut self, gilrs: Option<&mut Gilrs>) {
        // When we don't have any more pending events, flush any pending mouse
        // moves, so that they can get coalesced properly.
        self.flush_mouse_move();

        if let Some(Event { event, .. }) = gilrs.and_then(|gilrs| gilrs.next_event()) {
            match event {
                EventType::ButtonPressed(button, _) => {
                    if let Some(button) = gilrs_button_to_gamepad_button(button) {
                        self.handle_player_event(PlayerEvent::GamepadButtonDown { button });
                    }
                }
                EventType::ButtonReleased(button, _) => {
                    if let Some(button) = gilrs_button_to_gamepad_button(button) {
                        self.handle_player_event(PlayerEvent::GamepadButtonUp { button });
                    }
                }
                _ => {}
            }
        }

        // Core loop
        // [NA] This used to be called `MainEventsCleared`, but I think the behaviour is different now.
        // We should look at changing our tick to happen somewhere else if we see any behavioural problems.
        if matches!(self.loaded, LoadingState::Loaded) {
            let new_time = Instant::now();
            // This runs after every batch of OS events (so ~1000 times a second while a
            // gaming mouse moves); only hit-test when a movie frame is actually due.
            if self.next_frame_time.is_none_or(|t| new_time >= t) {
                self.flush_mouse_move();
            }
            let dt = FloatDuration::from_std(new_time.duration_since(self.time));
            if dt.as_millis() > 0.0 {
                self.time = new_time;
                let perf = &mut self.perf;
                let frame_since_render = &mut self.frame_since_render;
                let frame_cost_ms = &mut self.frame_cost_ms;
                self.next_frame_time = self.player.get().map(|mut player| {
                    let frames_before = player.frames_run();
                    let timers_before = ruffle_render_wgpu::perf_counters::timers::snapshot();
                    player.tick(dt);
                    if player.frames_run() != frames_before {
                        *frame_since_render = true;
                        let frame_ms = new_time.elapsed().as_secs_f64() * 1000.0;
                        *frame_cost_ms = *frame_cost_ms * 0.9 + frame_ms * 0.1;
                        // AQW build: say what a slow frame (scripts etc., longer than 1.2
                        // frames) spent its time on, e.g. when a crowded room loads.
                        if frame_ms > 1.2 * 1000.0 / player.frame_rate() {
                            tracing::info!(
                                target: "ruffle_aqw_perf",
                                "slow frame {frame_ms:.0} ms ({} frames run); of that: {} (ms)",
                                player.frames_run() - frames_before,
                                timer_parts(&timers_before)
                            );
                        }
                        perf.frame(
                            new_time,
                            frame_ms,
                            1000.0 / player.frame_rate(),
                            player.frames_run() - frames_before,
                        );
                    }
                    if perf.maybe_log(new_time) {
                        memory_watch(&mut player);
                        perf.log_object_counts(&mut player);
                        crate::mugetsu::low_quality_tick(&mut player, perf.last_drawn_fps);
                    }
                    new_time + player.time_til_next_frame()
                });
                self.render_movie_if_needed();
            }
            self.pace_interpolation();
        }

        self.render_movie_if_needed();
    }

    fn handle_player_event(&mut self, event: PlayerEvent) -> bool {
        // Apply any pending mouse moves, so that the movie sees the latest
        // position before e.g. a click.
        self.flush_mouse_move();
        let handled = self.player.handle_event(event);

        // Handling an event may have made the movie need rendering (on demand).
        self.render_movie_if_needed();
        handled
    }

    /// AQW build: with frame interpolation on (title-bar menu), sends a picture on every refresh
    /// of the monitor: vsync makes each one wait for its refresh, which paces this exactly (a
    /// timer is ~1-2 ms late, a lot at 360 Hz). A new in-between picture is drawn on every n-th
    /// of them, the most the main thread can afford, so each stays up the same time; the others
    /// show the last one again, which costs little. Needs at least 1.5 new pictures per SWF
    /// frame, else it draws once per frame as without interpolation.
    fn pace_interpolation(&mut self) {
        self.next_draw_time = None;
        if !ruffle_core::frame_interpolation::enabled() || self.minimized || self.occluded {
            return;
        }
        let now = Instant::now();
        if self
            .refresh_checked
            .is_none_or(|checked| now.duration_since(checked) > Duration::from_secs(2))
        {
            let first = self.refresh_checked.is_none();
            self.refresh_checked = Some(now);
            let millihertz = self
                .gui
                .window()
                .current_monitor()
                .and_then(|monitor| monitor.refresh_rate_millihertz())
                .unwrap_or(60_000)
                .clamp(24_000, 360_000);
            let interval = Duration::from_secs_f64(1000.0 / millihertz as f64);
            if interval != self.refresh_interval || first {
                tracing::info!(
                    target: "ruffle_aqw_perf",
                    "frame interpolation: monitor refreshes at {:.1} Hz",
                    millihertz as f64 / 1000.0
                );
            }
            self.refresh_interval = interval;
        }
        let frame_rate = match self.player.get() {
            Some(player) if player.is_playing() => player.frame_rate(),
            _ => return,
        };
        // Draw on every refresh if the main thread can afford it (keeping a third of its time
        // spare), else on every 2nd, 3rd, ... refresh: an even rate looks smoother than an
        // uneven higher one. Each change of rate is a visible change of rhythm, so it only steps
        // to fewer pictures after needing to for half a second, and back to more after 3 seconds
        // of clearly affording it.
        let refresh_hz = 1.0 / self.refresh_interval.as_secs_f64();
        let spare_ms = (650.0 - self.frame_cost_ms * frame_rate).max(100.0);
        let divisor_for = |cost_ms: f64| {
            (refresh_hz * cost_ms.max(0.5) / spare_ms).ceil().clamp(1.0, 16.0) as u32
        };
        let needed = divisor_for(self.draw_cost_ms);
        let lasted = |since: &mut Option<Instant>, wanted: bool, secs: f64| {
            if !wanted {
                *since = None;
                return false;
            }
            now.duration_since(*since.get_or_insert(now)).as_secs_f64() >= secs
        };
        if lasted(&mut self.divisor_up_since, needed > self.draw_divisor, 0.5)
            || lasted(
                &mut self.divisor_down_since,
                divisor_for(self.draw_cost_ms * 1.3) < self.draw_divisor,
                3.0,
            )
        {
            self.draw_divisor = needed;
            self.divisor_up_since = None;
            self.divisor_down_since = None;
        }
        let interval = self.refresh_interval * self.draw_divisor;
        self.perf.interpolation_rate = refresh_hz / self.draw_divisor as f64;
        if interval.as_secs_f64() * 1.5 > 1.0 / frame_rate {
            // Not even 1.5 draws per SWF frame: draw once per frame, as without interpolation.
            return;
        }
        // Requested right away, keeping the screen's queue of pictures full so a busy moment on
        // the main thread doesn't miss a refresh; presenting waits for vsync. The timer only
        // limits this if presenting doesn't wait.
        let early = self.refresh_interval / 2;
        let mut due = self.draw_started + early;
        if now >= due {
            self.gui.window().request_redraw();
            due = now + early;
        }
        self.next_draw_time = Some(due);
    }

    fn flush_mouse_move(&mut self) {
        if !std::mem::take(&mut self.mouse_move_pending) {
            return;
        }

        let (x, y) = self.gui.window_to_movie_position(self.mouse_pos);
        self.player.handle_event(PlayerEvent::MouseMove { x, y });
        self.render_movie_if_needed();
    }

    // self.check_redraw() -> self.render_movie_if_needed()
    fn render_movie_if_needed(&self) {
        let mut player = self.player.get();
        // AQW build: like Flash Player's normal pacing, only draw once a new SWF frame has run.
        // AQW calls `updateAfterEvent()` from timers and hovers change between frames, which
        // made Ruffle redraw the same frame 2-9x per frame interval; in combat those extra
        // draws cost the main thread the time the next frame needed. A paused movie still
        // redraws on demand.
        let player_needs_render = player.as_ref().is_some_and(|p| {
            p.needs_render()
                && (self.frame_since_render || !p.is_playing() || extra_renders_enabled())
        });
        if player_needs_render
            && !self.minimized
            && !self.occluded
            && let Some(player) = player.as_deref_mut()
        {
            player.render();
        }

        let player_needs_render = player
            .map(|p| {
                p.needs_render()
                    && (self.frame_since_render || !p.is_playing() || extra_renders_enabled())
            })
            .unwrap_or_default();
        // AQW build: egui asks for a repaint on every mouse move. Each repaint presents, and
        // presenting faster than the monitor refreshes blocks the main thread, so moving the
        // mouse made frames late. With the menu bar hidden there is usually nothing to update,
        // so GUI-only repaints are capped at ~60/s (popups and dialogs still work).
        let gui_needs_render = self.gui.needs_render()
            && self.gui.wants_redraws()
            && (!self.no_gui || self.last_redraw.elapsed() >= Duration::from_millis(16));
        if player_needs_render || gui_needs_render {
            self.gui.window().request_redraw();
        }
    }
}

pub struct App {
    main_window: Option<MainWindow>,
    runtime: Option<tokio::runtime::Runtime>,
    gilrs: Option<Gilrs>,
    event_loop_proxy: EventLoopProxy<RuffleEvent>,
    preferences: GlobalPreferences,
    font_database: fontdb::Database,
}

/// Enters the tokio runtime context.
/// This cannot be a method, as the borrow-checker would complain.
macro_rules! enter_runtime {
    ($this:expr) => {
        let _guard = $this.runtime.as_ref().map(|runtime| runtime.enter());
    };
}

impl App {
    pub fn new(preferences: GlobalPreferences) -> Result<(Self, EventLoop<RuffleEvent>), Error> {
        let event_loop = EventLoop::with_user_event().build()?;

        let mut font_database = fontdb::Database::default();
        font_database.load_system_fonts();

        let gilrs = Gilrs::new()
            .inspect_err(|err| {
                tracing::warn!("Gamepad support could not be initialized: {err}");
            })
            .ok();
        let event_loop_proxy = event_loop.create_proxy();
        let runtime = tokio::runtime::Runtime::new()?;

        Ok((
            Self {
                main_window: None,
                runtime: Some(runtime),
                gilrs,
                event_loop_proxy,
                font_database,
                preferences,
            },
            event_loop,
        ))
    }
}

impl ApplicationHandler<RuffleEvent> for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        enter_runtime!(self);

        if cause == StartCause::Init {
            let movie_url = self.preferences.cli.movie_url.clone();
            let icon_bytes = include_bytes!("../assets/favicon-32.rgba");
            let icon =
                Icon::from_rgba(icon_bytes.to_vec(), 32, 32).expect("App icon should be correct");

            let no_gui = self.preferences.cli.no_gui;
            let min_window_size = if no_gui {
                (16, 16)
            } else {
                (350, MENU_HEIGHT + 16)
            }
            .into();
            let preferred_width = self.preferences.cli.width;
            let preferred_height = self.preferences.cli.height;
            let start_fullscreen = self.preferences.cli.fullscreen;

            #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
            let mut window_attributes = WindowAttributes::default()
                .with_visible(false)
                .with_title("Ruffle")
                .with_window_icon(Some(icon))
                .with_min_inner_size(min_window_size);

            #[cfg(target_os = "linux")]
            {
                use winit::platform::startup_notify::{
                    self, EventLoopExtStartupNotify, WindowAttributesExtStartupNotify,
                };
                use winit::platform::wayland::WindowAttributesExtWayland;
                window_attributes = window_attributes.with_name("rs.ruffle.Ruffle", "main");
                if let Some(token) = event_loop.read_token_from_env() {
                    startup_notify::reset_activation_token_env();
                    window_attributes = window_attributes.with_activation_token(token);
                }
            }

            let event_loop_proxy = self.event_loop_proxy.clone();
            let preferences = self.preferences.clone();
            let window = event_loop
                .create_window(window_attributes)
                .expect("Window should be created");
            let max_window_size = get_screen_size(&window);
            window.set_max_inner_size(Some(max_window_size));
            let window = Arc::new(window);
            let font_database = self.font_database.clone();

            let mut gui = GuiController::new(
                window.clone(),
                event_loop_proxy.clone(),
                preferences.clone(),
                &font_database,
                movie_url.clone(),
                no_gui,
            )
            .expect("GUI controller should be created");

            let mut player = PlayerController::new(
                event_loop_proxy.clone(),
                window.clone(),
                gui.descriptors().clone(),
                font_database,
                preferences.clone(),
                gui.file_picker(),
            );

            if let Some(movie_url) = &movie_url {
                gui.create_movie(
                    &mut player,
                    LaunchOptions::from(&preferences),
                    ContentDescriptor {
                        url: movie_url.clone(),
                        root_content_path: None,
                    },
                );
            } else {
                gui.show_open_dialog();
            }

            let mut loaded = LoadingState::Loading;

            if movie_url.is_none() {
                // No SWF provided on command line; show window with dummy movie immediately.
                window.set_visible(true);
                loaded = LoadingState::Loaded;
            }

            self.main_window = Some(MainWindow {
                preferences,
                gui,
                player,
                min_window_size,
                max_window_size,
                no_gui,
                preferred_width,
                preferred_height,
                start_fullscreen,
                loaded,
                minimized: false,
                occluded: false,
                mouse_pos: PhysicalPosition::new(0.0, 0.0),
                mouse_move_pending: false,
                modifiers: Modifiers::default(),
                time: Instant::now(),
                next_frame_time: None,
                event_loop_proxy,
            });
        }
    }

    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: RuffleEvent) {
        enter_runtime!(self);

        match (&mut self.main_window, event) {
            (Some(main_window), RuffleEvent::TaskPoll(task)) => main_window.player.poll(task),

            (Some(main_window), RuffleEvent::OnMetadata(swf_header)) => {
                main_window.on_metadata(swf_header)
            }

            (Some(main_window), RuffleEvent::ContextMenuItemClicked(index)) => {
                if let Some(mut player) = main_window.player.get() {
                    player.run_context_menu_callback(index);
                }
            }

            (Some(main_window), RuffleEvent::BrowseAndOpen(options, open_type)) => {
                let event_loop = main_window.event_loop_proxy.clone();
                let picker = main_window.gui.file_picker();
                tokio::spawn(async move {
                    let picked = match open_type {
                        OpenType::File => {
                            picker.pick_ruffle_file(None).await.map(|file| (file, None))
                        }
                        OpenType::Directory => picker
                            .pick_ruffle_directory_and_content(None)
                            .await
                            .map(|(dir, file)| (file, Some(dir))),
                    };

                    if let Some(desc) =
                        picked.and_then(|(file, dir)| ContentDescriptor::new_local(&file, dir))
                    {
                        let _ = event_loop.send_event(RuffleEvent::Open(desc, options));
                    }
                });
            }

            (Some(main_window), RuffleEvent::Open(desc, options)) => {
                main_window
                    .gui
                    .create_movie(&mut main_window.player, *options, desc);
            }

            (Some(main_window), RuffleEvent::Resize(width, height)) => {
                if let Some(mut player) = main_window.player.get() {
                    let width = width.max(1) as f64 * main_window.gui.window().scale_factor();
                    let height = height.max(1) as f64 * main_window.gui.window().scale_factor();
                    player.set_viewport_dimensions(ViewportDimensions {
                        width: width as u32,
                        height: height as u32,
                        scale_factor: main_window.gui.window().scale_factor(),
                    });
                    let inner_size = PhysicalSize::new(width as f64, height as f64);
                    main_window
                        .gui
                        .window()
                        .set_min_inner_size(Some(inner_size));
                    main_window
                        .gui
                        .window()
                        .set_max_inner_size(Some(inner_size));
                    let _ = main_window.gui.window().request_inner_size(inner_size);
                }
            }

            (Some(main_window), RuffleEvent::OpenDialog(descriptor)) => {
                main_window.gui.open_dialog(descriptor);
            }

            (Some(main_window), RuffleEvent::CloseFile) => {
                main_window.gui.window().set_title("Ruffle"); // Reset title since file has been closed.
                main_window.gui.close_movie(&mut main_window.player);
            }

            (Some(main_window), RuffleEvent::ExportBundle) => {
                main_window.gui.export_bundle();
            }

            (Some(main_window), RuffleEvent::EnterFullScreen) => {
                if let Some(mut player) = main_window.player.get()
                    && player.is_playing()
                {
                    player.set_fullscreen(true);
                }
            }

            (Some(main_window), RuffleEvent::ExitFullScreen) => {
                if let Some(mut player) = main_window.player.get()
                    && player.is_playing()
                {
                    player.set_fullscreen(false);
                }
            }

            (Some(main_window), RuffleEvent::PlayerNotification(notification)) => {
                match notification {
                    PlayerNotification::ImeNotification(ImeNotification::ImeReady {
                        purpose,
                        cursor_area,
                    }) => {
                        let ime_enabled = main_window.preferences.ime_enabled().unwrap_or(false);
                        main_window.gui.set_ime_allowed(ime_enabled);
                        main_window.gui.set_ime_purpose(purpose);
                        main_window.gui.set_ime_cursor_area(cursor_area);
                    }
                    PlayerNotification::ImeNotification(ImeNotification::ImePurposeUpdated(
                        purpose,
                    )) => {
                        main_window.gui.set_ime_purpose(purpose);
                    }
                    PlayerNotification::ImeNotification(ImeNotification::ImeCursorAreaUpdated(
                        cursor_area,
                    )) => {
                        main_window.gui.set_ime_cursor_area(cursor_area);
                    }
                    PlayerNotification::ImeNotification(ImeNotification::ImeNotReady) => {
                        main_window.gui.set_ime_allowed(false);
                    }
                }
            }

            (_, RuffleEvent::ExitRequested) => {
                event_loop.exit();
            }

            _ => {}
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        enter_runtime!(self);

        if let Some(main_window) = &mut self.main_window {
            main_window.window_event(event_loop, event);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        enter_runtime!(self);

        if let Some(main_window) = &mut self.main_window {
            main_window.about_to_wait(self.gilrs.as_mut());

            // The event loop is finished; let's find out how long we need to wait for.
            // We don't need to worry about earlier update requests, as it's the
            // only place where we're setting control flow, and events cancel wait.
            // Note: the control flow might be set to `ControlFlow::WaitUntil` with a
            // timestamp in the past! Take that into consideration when changing this code.
            if let Some(next_frame_time) = main_window.next_frame_time {
                let wake = match (main_window.next_frame_time, main_window.next_draw_time) {
                    (Some(frame), Some(draw)) => Some(frame.min(draw)),
                    (frame, draw) => frame.or(draw),
                };
                if let Some(wake) = wake {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
                }
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // `MainWindow` needs a tokio context to properly drop.
        {
            enter_runtime!(self);
            let _ = self.main_window.take();
        }

        // Manually stop the tokio runtime: this makes sure that any pending Player-bound futures
        // are properly cancelled and put back on the winit event loop before it closes, preventing
        // them from being dropped on the wrong thread and causing a panic.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(std::time::Duration::from_secs(1));
        }
    }
}

enum LoadingState {
    Loading,
    WaitingForResize,
    Loaded,
}
