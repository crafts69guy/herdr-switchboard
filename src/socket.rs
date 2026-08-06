//! The one place this plugin talks to herdr over its unix socket instead of the
//! `herdr` CLI.
//!
//! Everything else routes through [`crate::runner::CommandRunner`], and should:
//! shelling out is testable and the CLI is the stable contract. This module
//! exists because `pane.graphics.set` / `pane.graphics.clear` have **no CLI
//! subcommand** — they are absent from `herdr pane --help` and reachable only
//! through the socket API. The zen gutters' scrim needs them, so the socket is
//! the only door.
//!
//! Two rules hold the blast radius down:
//!
//! 1. **Everything fails soft.** A scrim is decoration; a dead socket, a herdr
//!    without `[experimental] kitty_graphics`, or a pane that vanished mid-call
//!    must degrade to "no scrim", never to an error the user sees. The public
//!    helpers return `bool` for that reason — there is nothing a caller could
//!    usefully do with the error but ignore it.
//! 2. **The payload builder is pure.** [`scrim_params`] is a plain
//!    `Value`-returning function so the wire shape is unit-tested without a
//!    running herdr, the same way `git.rs` keeps `on_key` IO-free.

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// How long to wait on connect/read. The socket is local and herdr answers
/// immediately; a hang here would stall the zen toggle, so the timeout is short
/// and a miss simply means "no scrim".
const TIMEOUT: Duration = Duration::from_millis(1500);

/// `$HERDR_SOCKET_PATH`, falling back to herdr's default location. herdr injects
/// the variable into plugin panes, so the fallback only matters when the binary
/// is run by hand.
fn socket_path() -> Option<PathBuf> {
    if let Some(path) = env::var("HERDR_SOCKET_PATH")
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(path));
    }
    env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config/herdr/herdr.sock"))
}

/// One newline-delimited JSON round trip: `{id, method, params}` out, one line
/// back. Returns the `result` object, or an error for a transport failure *or*
/// an `{"error":{"code","message"}}` reply — callers treat both the same.
pub fn request(method: &str, params: Value) -> Result<Value> {
    let path = socket_path().context("no herdr socket path")?;
    let stream =
        UnixStream::connect(&path).with_context(|| format!("connect {}", path.display()))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;

    let payload =
        json!({ "id": format!("switchboard:{method}"), "method": method, "params": params });
    let mut writer = &stream;
    writeln!(writer, "{payload}").context("write herdr request")?;
    writer.flush().ok();

    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .context("read herdr response")?;
    parse_response(&line)
}

/// Split a reply into `result` or an error. Kept separate from the IO so the
/// error shape herdr actually returns is pinned by a test rather than by memory.
fn parse_response(line: &str) -> Result<Value> {
    let value: Value = serde_json::from_str(line.trim()).context("herdr sent invalid JSON")?;
    if let Some(error) = value.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("herdr rejected the request");
        bail!("{code}: {message}");
    }
    value
        .get("result")
        .cloned()
        .context("herdr reply had neither result nor error")
}

/// The `pane.graphics.set` payload for a flat scrim over `pane_id`.
///
/// The image is a **single pixel**. `placement.grid_cols`/`grid_rows` scale it
/// to the pane's cell grid, so a full-pane wash costs eight bytes of base64 no
/// matter how large the pane is — there is no reason to build a real bitmap.
///
/// Alpha is carried in the payload but herdr composites it opaque (measured
/// against 0.8.0: a 60%-alpha scrim renders identically to a 100% one). The
/// channel is kept because the wire format requires four bytes for `rgba`, not
/// because it does anything.
pub fn scrim_params(pane_id: &str, cols: u32, rows: u32, rgba: [u8; 4]) -> Value {
    json!({
        "pane_id": pane_id,
        "format": "rgba",
        "image_width": 1,
        "image_height": 1,
        "data_base64": base64(&rgba),
        "placement": {
            "viewport_col": 0,
            "viewport_row": 0,
            "grid_cols": cols,
            "grid_rows": rows,
        },
    })
}

/// Paint the scrim. `false` means it did not happen — which is a fine outcome,
/// the gutter is simply an undimmed blank pane.
pub fn set_scrim(pane_id: &str, cols: u32, rows: u32, rgba: [u8; 4]) -> bool {
    cols > 0
        && rows > 0
        && request("pane.graphics.set", scrim_params(pane_id, cols, rows, rgba)).is_ok()
}

/// Remove the scrim. Safe on a pane that never had one and on a pane that is
/// already gone; both answer `false` and neither matters.
pub fn clear_scrim(pane_id: &str) -> bool {
    request("pane.graphics.clear", json!({ "pane_id": pane_id })).is_ok()
}

/// A tab's split tree, as `layout.export` reports it.
///
/// Read-only, and that is the entire reason it is used. Its sibling
/// `layout.apply` looks like the natural way to restore a layout and is
/// **destructive**: given a tree of existing `pane_id`s it does not re-parent
/// them, it builds a *new* tab full of *new* panes and discards the originals,
/// processes and all. Measured against herdr 0.8.0. Never call it here.
pub fn export_layout(tab_id: &str) -> Option<Value> {
    request("layout.export", json!({ "tab_id": tab_id }))
        .ok()?
        .get("layout")?
        .get("root")
        .cloned()
}

/// `#rrggbb` (or bare `rrggbb`) to an opaque RGBA quad. `None` on anything else,
/// so a typo'd config colour falls back to the default rather than painting a
/// surprise.
pub fn parse_hex(text: &str) -> Option<[u8; 4]> {
    let hex = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?, 0xff])
}

/// Standard base64 with padding. Four bytes in, eight characters out — small
/// enough that pulling in a crate for it would cost more than it saves.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 0x3f] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_reference_encoder() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        // The payload this module actually sends: one opaque catppuccin-crust pixel.
        assert_eq!(base64(&[0x11, 0x11, 0x1b, 0xff]), "EREb/w==");
    }

    #[test]
    fn a_scrim_is_one_pixel_scaled_over_the_cell_grid() {
        let params = scrim_params("w1:p2", 31, 50, [0x11, 0x11, 0x1b, 0xff]);
        // One pixel, however large the pane: the grid does the scaling.
        assert_eq!(params["image_width"], 1);
        assert_eq!(params["image_height"], 1);
        assert_eq!(params["format"], "rgba");
        assert_eq!(params["pane_id"], "w1:p2");
        assert_eq!(params["placement"]["grid_cols"], 31);
        assert_eq!(params["placement"]["grid_rows"], 50);
        assert_eq!(params["placement"]["viewport_col"], 0);
        assert_eq!(params["data_base64"], "EREb/w==");
    }

    #[test]
    fn a_zero_sized_scrim_is_never_sent() {
        // Guards the launch path: a pane whose rect has not settled yet would
        // otherwise reach the socket with a degenerate grid.
        assert!(!set_scrim("w1:p2", 0, 50, [0, 0, 0, 0xff]));
        assert!(!set_scrim("w1:p2", 31, 0, [0, 0, 0, 0xff]));
    }

    #[test]
    fn a_result_reply_yields_the_result() {
        let reply = r#"{"id":"switchboard:pane.graphics.set","result":{"type":"ok"}}"#;
        assert_eq!(parse_response(reply).unwrap()["type"], "ok");
    }

    #[test]
    fn an_error_reply_carries_herdrs_code_and_message() {
        // The exact shape herdr 0.8.0 returns for a stale pane id, which is the
        // failure the zen exit path hits most often.
        let reply =
            r#"{"error":{"code":"pane_not_found","message":"pane w1:p9 not found"},"id":"x"}"#;
        let error = parse_response(reply).unwrap_err().to_string();
        assert!(error.contains("pane_not_found"), "{error}");
        assert!(error.contains("pane w1:p9 not found"), "{error}");
    }

    #[test]
    fn a_junk_reply_is_an_error_not_a_panic() {
        assert!(parse_response("not json").is_err());
        assert!(parse_response(r#"{"id":"x"}"#).is_err());
    }

    #[test]
    fn hex_colours_parse_with_or_without_the_hash() {
        assert_eq!(parse_hex("#11111b"), Some([0x11, 0x11, 0x1b, 0xff]));
        assert_eq!(parse_hex("11111B"), Some([0x11, 0x11, 0x1b, 0xff]));
        assert_eq!(parse_hex("  #1e1e2e  "), Some([0x1e, 0x1e, 0x2e, 0xff]));
    }

    #[test]
    fn a_malformed_colour_is_rejected_rather_than_guessed() {
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("mauve"), None);
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(parse_hex(""), None);
    }
}
