use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use smithay::utils::SERIAL_COUNTER;

use crate::state::DriftWm;
use driftwm::window_ext::WindowExt;

pub mod client;
pub mod protocol;

use self::protocol::{Reply, Request, Response, socket_path};

/// Reject a command line longer than this (without a newline) — bounds the
/// per-connection buffer against a client that never terminates a command.
const MAX_COMMAND_SIZE: usize = 4096;

/// Cap how long a reply write may block, so a stuck reader can't hang the loop.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

pub struct IpcServer {
    socket_path: PathBuf,
}

impl IpcServer {
    pub fn new(
        event_loop: &LoopHandle<'static, DriftWm>,
        wayland_display: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let socket_path = socket_path(wayland_display);

        std::fs::remove_file(&socket_path).ok();
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&socket_path)?;
        listener.set_nonblocking(true)?;
        std::fs::set_permissions(&socket_path, PermissionsExt::from_mode(0o600))?;

        tracing::info!("IPC socket started at {}", socket_path.display());

        let source = Generic::new(listener, Interest::READ, Mode::Level);
        event_loop.insert_source(source, |_, listener, state| {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => accept_client(state, stream),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    // A transient accept error (e.g. fd exhaustion) must never
                    // tear down the compositor — log and keep serving.
                    Err(e) => {
                        tracing::warn!("IPC accept error: {e}");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        })?;

        Ok(Self { socket_path })
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        std::fs::remove_file(&self.socket_path).ok();
        tracing::debug!("IPC socket cleaned up");
    }
}

/// Register an accepted connection as its own calloop source. The closure owns a
/// per-connection read buffer so partial commands survive across event-loop ticks.
fn accept_client(state: &mut DriftWm, stream: UnixStream) {
    if let Err(e) = stream.set_nonblocking(true) {
        tracing::warn!("IPC: failed to set client nonblocking: {e}");
        return;
    }
    let mut buffer: Vec<u8> = Vec::with_capacity(256);
    let source = Generic::new(stream, Interest::READ, Mode::Level);
    // The callback hands a `&mut NoIoDrop<UnixStream>` which derefs to
    // `&UnixStream`; its Read/Write impls take `&self`, so a shared ref suffices.
    let registered = state
        .loop_handle
        .insert_source(source, move |_, stream, state| {
            Ok(serve_connection(stream, &mut buffer, state))
        });
    if let Err(e) = registered {
        tracing::warn!("IPC: failed to register client connection: {e}");
    }
}

/// Drain everything readable, answering each complete `\n`-terminated command.
fn serve_connection(
    mut stream: &UnixStream,
    buffer: &mut Vec<u8>,
    state: &mut DriftWm,
) -> PostAction {
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return PostAction::Remove, // EOF
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.len() > MAX_COMMAND_SIZE {
                    tracing::warn!("IPC command too large, disconnecting");
                    return PostAction::Remove;
                }
                while let Some(nl) = buffer.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=nl).collect();
                    let reply = process_line(&line[..nl], state);
                    if write_reply(stream, &reply).is_err() {
                        return PostAction::Remove;
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => return PostAction::Continue,
            Err(e) => {
                tracing::warn!("IPC read error: {e}");
                return PostAction::Remove;
            }
        }
    }
}

fn process_line(line: &[u8], state: &mut DriftWm) -> Reply {
    let request: Request =
        serde_json::from_slice(line).map_err(|e| format!("invalid request: {e}"))?;
    dispatch(request, state)
}

fn dispatch(request: Request, state: &mut DriftWm) -> Reply {
    // The animated setters (Camera/Zoom) only stash a target — they don't
    // self-schedule a frame — so every state-changing command needs the kick.
    // Pure queries don't, and shouldn't force a redraw.
    if is_mutating(&request) {
        state.mark_all_dirty();
    }

    match request {
        Request::Camera(arg) => cmd_camera(arg, state),
        Request::Zoom(arg) => cmd_zoom(arg, state),
        Request::Layout => Ok(Response::Layout(state.active_layout.clone())),
        Request::State => Ok(cmd_state(state)),
        Request::Focus(arg) => cmd_focus(arg, state),
        Request::Close => cmd_close(state),
        Request::Move(arg) => cmd_move(arg, state),
        Request::Quit => {
            tracing::info!("IPC quit received, shutting down");
            state.loop_signal.stop();
            Ok(Response::Ok)
        }
    }
}

fn is_mutating(request: &Request) -> bool {
    matches!(
        request,
        Request::Camera(Some(_))
            | Request::Zoom(Some(_))
            | Request::Focus(Some(_))
            | Request::Close
            | Request::Move(Some(_))
    )
}

fn cmd_camera(arg: Option<(f64, f64)>, state: &mut DriftWm) -> Reply {
    match arg {
        None => {
            let (x, y) = camera_center(state);
            Ok(Response::Camera { x, y })
        }
        Some((x, y)) => {
            // (x, y) is the viewport center, Y-up; map it to the internal camera
            // target so the viewport ends up centered there.
            let target =
                driftwm::canvas::camera_for_center(x, y, state.zoom(), state.get_viewport_size());
            state.set_camera_target(Some(target));
            Ok(Response::Camera { x, y })
        }
    }
}

fn cmd_zoom(arg: Option<f64>, state: &mut DriftWm) -> Reply {
    match arg {
        None => Ok(Response::Zoom(state.zoom())),
        Some(zoom) => {
            if !zoom.is_finite() || zoom <= 0.0 {
                return Err("zoom must be a positive number".to_string());
            }
            // Same bounds as keyboard/gesture zoom; reply reports what was applied.
            let clamped = zoom.clamp(state.min_zoom(), driftwm::canvas::MAX_ZOOM);
            // Anchor on the viewport center (like keyboard zoom) so the center
            // stays put — otherwise zoom drifts the camera off the `camera` point.
            state.zoom_to_anchored(clamped);
            Ok(Response::Zoom(clamped))
        }
    }
}

fn cmd_state(state: &mut DriftWm) -> Response {
    let windows = state.window_inventory();
    Response::State {
        camera: camera_center(state),
        zoom: state.zoom(),
        windows,
    }
}

fn cmd_focus(arg: Option<String>, state: &mut DriftWm) -> Reply {
    match arg {
        None => Ok(Response::Focused(
            state.focused_window().and_then(|w| w.app_id_or_class()),
        )),
        Some(target) => {
            let target = target.to_lowercase();
            let found = state
                .space
                .elements()
                .find(|w| {
                    !w.is_widget()
                        && w.app_id_or_class()
                            .is_some_and(|a| a.to_lowercase().contains(&target))
                })
                .cloned();

            match found {
                Some(window) => {
                    let app_id = window.app_id_or_class();
                    // Already on screen: just raise + focus, don't move the camera.
                    if state.window_fully_in_viewport(&window) {
                        state.raise_and_focus(&window, SERIAL_COUNTER.next_serial());
                    } else {
                        state.navigate_to_window(&window, state.config.zoom_reset_on_activation);
                    }
                    Ok(Response::Focused(app_id))
                }
                None => Err(format!("no window matching '{target}'")),
            }
        }
    }
}

fn cmd_close(state: &mut DriftWm) -> Reply {
    match state.focused_window().filter(|w| !w.is_widget()) {
        Some(window) => {
            window.send_close();
            Ok(Response::Ok)
        }
        None => Err("no focused window to close".to_string()),
    }
}

fn cmd_move(arg: Option<(i32, i32)>, state: &mut DriftWm) -> Reply {
    let Some(window) = state.focused_window() else {
        return Err("no focused window".to_string());
    };
    let size = window.geometry().size;
    match arg {
        None => {
            let loc = state.space.element_location(&window).unwrap_or_default();
            let (x, y) = driftwm::canvas::internal_to_rule(loc, size);
            Ok(Response::Position { x, y })
        }
        Some((x, y)) => {
            let loc = driftwm::canvas::rule_to_internal(x, y, size);
            state.space.map_element(window, loc, true);
            Ok(Response::Position { x, y })
        }
    }
}

/// Serialize and send a reply. Switches to a bounded blocking write so a large
/// reply isn't truncated on `WouldBlock` and a stuck reader can't hang the loop.
fn write_reply(mut stream: &UnixStream, reply: &Reply) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(reply)?;
    bytes.push(b'\n');
    stream.set_nonblocking(false).ok();
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok();
    let res = stream.write_all(&bytes);
    stream.set_nonblocking(true).ok();
    res
}

/// The viewport center, Y-up — same representation as the state file, so `camera`,
/// `state`, and the state file all agree.
fn camera_center(state: &DriftWm) -> (f64, f64) {
    driftwm::canvas::viewport_center(state.camera(), state.zoom(), state.get_viewport_size())
}
