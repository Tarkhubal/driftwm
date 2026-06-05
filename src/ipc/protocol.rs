//! Shared IPC protocol types for the compositor and the `driftwm msg` client.
//!
//! The transport is line-delimited JSON over a Unix socket: one `Request` per
//! line, one `Reply` per line. Keeping it JSON means the socket is debuggable
//! with `socat` and usable from any scripting language, not just `driftwm msg`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A command from a client to the compositor. Variants carrying `Option<_>` read
/// when `None` and write when `Some`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Coordinates are the viewport center, Y-up.
    Camera(Option<(f64, f64)>),
    /// Set value is clamped to the supported range (out to fit-all, in to native
    /// — no magnification).
    Zoom(Option<f64>),
    Layout,
    State,
    /// Focus a window by `app_id` substring when `Some`.
    Focus(Option<String>),
    Close,
    /// Coordinates are window-center, Y-up (the window-rule convention).
    Move(Option<(i32, i32)>),
    Quit,
}

/// A successful reply payload. Pairs with [`Request`] variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Camera {
        x: f64,
        y: f64,
    },
    Zoom(f64),
    Layout(String),
    State {
        camera: (f64, f64),
        zoom: f64,
        windows: Vec<WindowInfo>,
    },
    Focused(Option<String>),
    /// Window-center, Y-up coordinates.
    Position {
        x: i32,
        y: i32,
    },
    Ok,
}

/// The result of a request: `Ok(Response)` or a human-readable error string.
pub type Reply = Result<Response, String>;

/// One window in the canvas inventory (`position` = window center, Y-up).
///
/// Shared by the IPC [`Response::State`] payload and the
/// `$XDG_RUNTIME_DIR/driftwm/state` file so the two representations can't drift.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub app_id: String,
    pub title: String,
    pub position: [i32; 2],
    pub size: [i32; 2],
    pub is_focused: bool,
    pub is_widget: bool,
}

/// Path to the IPC socket for a given `WAYLAND_DISPLAY` name:
/// `$XDG_RUNTIME_DIR/driftwm/ipc-<wayland_display>.sock` (falls back to `/tmp`).
///
/// Deriving the name from the wayland display lets each compositor instance own
/// a distinct socket and lets `driftwm msg` auto-target the session it runs in.
pub fn socket_path(wayland_display: &str) -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir)
        .join("driftwm")
        .join(format!("ipc-{wayland_display}.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(value, &back);
    }

    #[test]
    fn request_roundtrip() {
        for r in [
            Request::Camera(None),
            Request::Camera(Some((100.0, -200.0))),
            Request::Zoom(Some(2.0)),
            Request::Layout,
            Request::State,
            Request::Focus(Some("alacritty".into())),
            Request::Close,
            Request::Move(None),
            Request::Move(Some((0, 0))),
            Request::Quit,
        ] {
            roundtrip(&r);
        }
    }

    #[test]
    fn reply_roundtrip() {
        let windows = vec![WindowInfo {
            app_id: "foo".into(),
            title: "bar".into(),
            position: [10, -20],
            size: [640, 480],
            is_focused: true,
            is_widget: false,
        }];
        let replies: Vec<Reply> = vec![
            Ok(Response::Camera { x: 1.0, y: 2.0 }),
            Ok(Response::Zoom(1.5)),
            Ok(Response::State {
                camera: (0.0, 0.0),
                zoom: 1.0,
                windows,
            }),
            Ok(Response::Focused(None)),
            Ok(Response::Ok),
            Err("no focused window".into()),
        ];
        for reply in &replies {
            let json = serde_json::to_string(reply).unwrap();
            let back: Reply = serde_json::from_str(&json).unwrap();
            assert_eq!(reply, &back);
        }
    }

    #[test]
    fn socket_path_uses_wayland_display() {
        // SAFETY: single-threaded test; no other thread reads the env here.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
        assert_eq!(
            socket_path("wayland-1"),
            PathBuf::from("/run/user/1000/driftwm/ipc-wayland-1.sock")
        );
    }
}
