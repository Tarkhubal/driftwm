//! The `driftwm msg` client: connect to the running compositor's IPC socket,
//! send one request, print the reply. Runs in the same binary but never starts
//! a compositor.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use super::protocol::{Reply, Request, Response, socket_path};

/// `driftwm msg <...>` subcommands. Variants with optional positionals read when
/// omitted and write when given.
#[derive(clap::Subcommand, Debug)]
pub enum Msg {
    /// Get the camera position, or set it (animated) with `<x> <y>` (viewport center, Y-up).
    #[command(allow_negative_numbers = true)]
    Camera { x: Option<f64>, y: Option<f64> },
    /// Get the zoom level, or set it with `<level>` (clamped to the supported range).
    Zoom { level: Option<f64> },
    /// Print the active keyboard layout.
    Layout,
    /// Dump camera, zoom, and the window inventory.
    State,
    /// Print the focused window, or focus a window by app_id substring.
    Focus { app_id: Option<String> },
    /// Close the focused window.
    Close,
    /// Get the focused window position, or move it (center, Y-up) with `<x> <y>`.
    #[command(allow_negative_numbers = true)]
    Move { x: Option<i32>, y: Option<i32> },
    /// Shut the compositor down.
    Quit,
}

pub fn run(msg: &Msg, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let request = to_request(msg)?;

    // A client launched inside a driftwm session inherits its WAYLAND_DISPLAY, so
    // the derived path targets that instance. DRIFTWM_SOCKET is an explicit
    // override (the server never reads it, so there's no nested-bind footgun).
    let path = match std::env::var_os("DRIFTWM_SOCKET") {
        Some(p) => PathBuf::from(p),
        None => {
            let display = std::env::var("WAYLAND_DISPLAY")
                .map_err(|_| "WAYLAND_DISPLAY is not set — are you in a driftwm session?")?;
            socket_path(&display)
        }
    };

    let mut stream = UnixStream::connect(&path)
        .map_err(|e| format!("cannot connect to {}: {e}", path.display()))?;

    let mut payload = serde_json::to_vec(&request)?;
    payload.push(b'\n');
    stream.write_all(&payload)?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err("no response from compositor".into());
    }
    let reply: Reply = serde_json::from_str(line.trim_end())?;

    if json {
        println!("{}", serde_json::to_string_pretty(&reply)?);
        // Exit non-zero on a command error too, so scripts can branch on it.
        if reply.is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }

    match reply {
        Ok(response) => {
            print_response(response);
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn to_request(msg: &Msg) -> Result<Request, String> {
    Ok(match msg {
        Msg::Camera { x, y } => match (x, y) {
            (None, None) => Request::Camera(None),
            (Some(x), Some(y)) => Request::Camera(Some((*x, *y))),
            _ => return Err("camera needs both <x> and <y>".to_string()),
        },
        Msg::Zoom { level } => Request::Zoom(*level),
        Msg::Layout => Request::Layout,
        Msg::State => Request::State,
        Msg::Focus { app_id } => Request::Focus(app_id.clone()),
        Msg::Close => Request::Close,
        Msg::Move { x, y } => match (x, y) {
            (None, None) => Request::Move(None),
            (Some(x), Some(y)) => Request::Move(Some((*x, *y))),
            _ => return Err("move needs both <x> and <y>".to_string()),
        },
        Msg::Quit => Request::Quit,
    })
}

fn print_response(response: Response) {
    match response {
        Response::Camera { x, y } => println!("camera {x} {y}"),
        Response::Zoom(zoom) => println!("zoom {zoom}"),
        Response::Layout(layout) => println!("{layout}"),
        Response::Focused(Some(app_id)) => println!("{app_id}"),
        Response::Focused(None) => println!("(none)"),
        Response::Position { x, y } => println!("{x} {y}"),
        Response::Ok => println!("ok"),
        Response::State {
            camera,
            zoom,
            windows,
        } => {
            println!("camera {} {}", camera.0, camera.1);
            println!("zoom {zoom}");
            println!("windows {}", windows.len());
            for w in windows {
                let mark = if w.is_focused { "*" } else { " " };
                let title = if w.title.is_empty() {
                    String::new()
                } else {
                    format!("  \"{}\"", w.title)
                };
                println!(
                    "  {mark} {} [{}, {}] {}x{}{}",
                    w.app_id, w.position[0], w.position[1], w.size[0], w.size[1], title
                );
            }
        }
    }
}
