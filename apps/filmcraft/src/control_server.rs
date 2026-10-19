//! Localhost JSON-lines control server (one request per line, one reply per line). Loopback only.
//!
//! The server only ever reads requests: a line that is not a JSON object with a string `method`
//! (an HTTP request line from a browser's cross-origin `fetch`, a stray `nc`, binary junk) gets
//! one error reply and the connection is closed, so nothing after it is executed. Lines are
//! bounded ([`MAX_LINE`]) and so is the number of open connections ([`MAX_CONNECTIONS`]); junk
//! never reaches the UI thread. See `docs/control-protocol.md`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use filmcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};

/// Longest accepted request line (bytes, without the newline). Requests are small JSON objects;
/// anything longer is refused and the connection closed.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Connections served at once; further ones get an error line and are closed.
pub const MAX_CONNECTIONS: usize = 16;

pub fn start(port: u16, ctx: egui::Context) -> Receiver<ControlRequest> {
    let (tx, rx) = channel::<ControlRequest>();
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("filmcraft: control server failed to bind 127.0.0.1:{port}: {e}");
            return rx;
        }
    };
    eprintln!("filmcraft: control server listening on 127.0.0.1:{port}");
    std::thread::spawn(move || accept_loop(listener, tx, ctx));
    rx
}

fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>, ctx: egui::Context) {
    let open = Arc::new(AtomicUsize::new(0));
    for mut stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
            continue;
        }
        let tx = tx.clone();
        let ctx = ctx.clone();
        // The slot is given back when the guard drops: after `serve` returns, if it panics, or if
        // the thread can't be started (the closure, and with it the guard, is dropped).
        let slot = Slot(Arc::clone(&open));
        let spawned = std::thread::Builder::new().name("control-conn".into()).spawn(move || {
            let _slot = slot;
            serve(stream, &tx, &ctx);
        });
        if let Err(e) = spawned {
            eprintln!("filmcraft: control server could not start a connection thread: {e}");
        }
    }
}

/// One counted connection; decrements the open-connection count when dropped.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn serve(stream: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context) {
    let Ok(read) = stream.try_clone() else { return };
    let Ok(events_out) = stream.try_clone() else { return };
    let mut watch: Option<Value> = None;
    serve_lines(read, stream, |method, params| {
        if method == WATCH_METHOD {
            // the reply acknowledges; the connection then only streams events (see `watch_events`)
            let reply = json!({"ok": true, "result": {"watching": true, "events": EVENT_KINDS}, CLOSE_AFTER: true});
            watch = Some(params);
            return Some(reply);
        }
        ask(tx, ctx, &method, params, Duration::from_secs(60))
    });
    if let Some(params) = watch {
        watch_events(events_out, tx, ctx, &params);
    }
}

/// Send one request to the UI thread and wait for its reply (`None` when the app is gone).
fn ask(tx: &Sender<ControlRequest>, ctx: &egui::Context, method: &str, params: Value, timeout: Duration) -> Option<Value> {
    let (req, rrx) = ControlRequest::new(method.to_string(), params);
    tx.send(req).ok()?;
    ctx.request_repaint();
    Some(rrx.recv_timeout(timeout).unwrap_or_else(|_| json!({"ok": false, "error": "timeout"})))
}

/// `events.watch {intervalMs?, settleMs?, agent?}`: after its reply the connection streams one JSON line per
/// change of the editor state an agent cares about, until the client closes it:
/// `{"event":"selection","selection":[clip ids]}`, `{"event":"playback","playing":bool,"playhead":ticks}`,
/// `{"event":"playhead","playhead":ticks}` (a parked playhead: sent once it has not moved for `settleMs`,
/// not while playing or scrubbing), `{"event":"edit","undo":n,"redo":n,"last":label}` (an edit, undo or
/// redo), `{"event":"sequence","activeSequence":id}`, and the Assistant panel's user events as
/// `{"event":"agent","kind":…}` (unless `agent: false`; see `panels::agent`). Each line also has `"state"`: the current
/// `{playhead, selection, playing, activeSequence, undo, redo}`. The state is sampled every
/// `intervalMs` (default 250, 50-5000) on the UI thread, like a client polling `ui.inspect`.
pub const WATCH_METHOD: &str = "events.watch";
const EVENT_KINDS: [&str; 6] = ["selection", "playback", "playhead", "edit", "sequence", "agent"];
/// Reply key that tells `serve_lines` to stop reading requests after this reply.
const CLOSE_AFTER: &str = "__closeAfter";

fn watch_events(mut out: TcpStream, tx: &Sender<ControlRequest>, ctx: &egui::Context, params: &Value) {
    let interval = Duration::from_millis(params.get("intervalMs").and_then(Value::as_u64).unwrap_or(250).clamp(50, 5000));
    let settle = Duration::from_millis(params.get("settleMs").and_then(Value::as_u64).unwrap_or(400).clamp(0, 10_000));
    // `agent: false`: this watcher leaves the Assistant panel's events to another client
    let drain_agent = params.get("agent").and_then(Value::as_bool).unwrap_or(true);
    let mut prev: Option<Value> = None;
    // a playhead that moved and has not yet been reported as parked: (value, when it last changed)
    let mut moving: Option<(Value, std::time::Instant)> = None;
    loop {
        let Some((state, inbox)) = sample(tx, ctx, drain_agent) else { return };
        let mut events = match &prev {
            Some(p) => diff_events(p, &state),
            None => Vec::new(),
        };
        let playing = state["playing"].as_bool().unwrap_or(false);
        if prev.as_ref().is_some_and(|p| p["playhead"] != state["playhead"]) {
            moving = Some((state["playhead"].clone(), std::time::Instant::now()));
        }
        if let Some((ph, since)) = &moving
            && !playing
            && *ph == state["playhead"]
            && since.elapsed() >= settle
        {
            events.push(json!({"event": "playhead", "playhead": ph}));
            moving = None;
        }
        events.extend(inbox.into_iter().map(|ev| {
            let mut e = json!({"event": "agent"});
            if let (Some(o), Some(src)) = (e.as_object_mut(), ev.as_object()) {
                o.extend(src.clone());
            }
            e
        }));
        for mut e in events {
            if let Some(o) = e.as_object_mut() {
                o.insert("state".into(), state.clone());
            }
            if writeln!(out, "{e}").and_then(|()| out.flush()).is_err() {
                return; // the client went away
            }
        }
        prev = Some(state);
        std::thread::sleep(interval);
    }
}

/// The watched part of the editor state, read on the UI thread, and the Assistant panel's user
/// events since the last sample (drained: they are delivered once, as `agent` events).
fn sample(tx: &Sender<ControlRequest>, ctx: &egui::Context, drain_agent: bool) -> Option<(Value, Vec<Value>)> {
    let ui = ask(tx, ctx, "ui.inspect", json!({}), Duration::from_secs(10))?;
    let hist = ask(tx, ctx, "engine.execute", json!({"command": "history.list", "params": {}}), Duration::from_secs(10))?;
    let ui = ui.get("result").cloned().unwrap_or(Value::Null);
    let hist = hist.get("result").cloned().unwrap_or(Value::Null);
    let undo = hist.get("undo").and_then(Value::as_array);
    let inbox = if drain_agent {
        ask(tx, ctx, "agent.poll", json!({}), Duration::from_secs(10)).and_then(|r| r.get("result").and_then(Value::as_array).cloned()).unwrap_or_default()
    } else {
        Vec::new()
    };
    let state = json!({
        "playhead": ui.get("playhead").cloned().unwrap_or(Value::Null),
        "selection": ui.get("selection").cloned().unwrap_or(json!([])),
        "playing": ui.get("playback").and_then(|p| p.get("playing")).cloned().unwrap_or(json!(false)),
        "activeSequence": ui.get("activeSequence").cloned().unwrap_or(Value::Null),
        "undo": undo.map_or(0, Vec::len),
        "redo": hist.get("redo").and_then(Value::as_array).map_or(0, Vec::len),
        "last": undo.and_then(|u| u.last()).cloned().unwrap_or(Value::Null),
    });
    Some((state, inbox))
}

/// Events between two samples (the parked-playhead event is timed by the caller).
fn diff_events(prev: &Value, cur: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    if prev["activeSequence"] != cur["activeSequence"] {
        out.push(json!({"event": "sequence", "activeSequence": cur["activeSequence"]}));
    }
    if prev["selection"] != cur["selection"] {
        out.push(json!({"event": "selection", "selection": cur["selection"]}));
    }
    if prev["playing"] != cur["playing"] {
        out.push(json!({"event": "playback", "playing": cur["playing"], "playhead": cur["playhead"]}));
    }
    if prev["undo"] != cur["undo"] || prev["redo"] != cur["redo"] || prev["last"] != cur["last"] {
        out.push(json!({"event": "edit", "undo": cur["undo"], "redo": cur["redo"], "last": cur["last"]}));
    }
    out
}

/// One parsed line.
enum Line {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Blank,
    /// Not a request: reply with this error and close the connection.
    Reject(String),
}

fn parse_line(bytes: &[u8]) -> Line {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Line::Reject("not a control request (invalid UTF-8); closing the connection".into());
    };
    let text = text.trim();
    if text.is_empty() {
        return Line::Blank;
    }
    let msg = match serde_json::from_str::<Value>(text) {
        Ok(v) => v,
        Err(e) => return Line::Reject(format!("bad JSON ({e}); expected one JSON request object per line; closing the connection")),
    };
    let Some(obj) = msg.as_object() else {
        return Line::Reject("not a control request (expected a JSON object); closing the connection".into());
    };
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Line::Reject("not a control request (missing string `method`); closing the connection".into());
    };
    Line::Request { id: obj.get("id").cloned().unwrap_or(Value::Null), method: method.to_string(), params: obj.get("params").cloned().unwrap_or(json!({})) }
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (newline stripped).
/// `Ok(false)` at end of stream; `Err` when the line is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    buf.clear();
    // `take` bounds the bytes buffered for one line: a peer can't make us allocate more than `max`.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(true);
    }
    if buf.len() > max {
        return Err(format!("request line longer than {max} bytes; closing the connection"));
    }
    // Last line without a newline, then end of stream.
    Ok(true)
}

/// Serve requests from `read`, answering on `out`, until the peer closes, sends something that is
/// not a request, or `handle` returns `None` (app gone). `handle` gets `(method, params)` and
/// returns the reply object (the request's `id` is added here).
fn serve_lines<R: Read, W: Write>(read: R, mut out: W, mut handle: impl FnMut(String, Value) -> Option<Value>) {
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
    loop {
        let reject = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(false) => return,
            Err(e) => e,
            Ok(true) => match parse_line(&buf) {
                Line::Blank => continue,
                Line::Reject(e) => e,
                Line::Request { id, method, params } => {
                    let Some(mut r) = handle(method, params) else { return };
                    let mut close = false;
                    if let Some(o) = r.as_object_mut() {
                        close = o.remove(CLOSE_AFTER).is_some();
                        o.insert("id".into(), id);
                    }
                    if writeln!(out, "{r}").and_then(|()| out.flush()).is_err() || close {
                        return;
                    }
                    continue;
                }
            },
        };
        let _ = writeln!(out, "{}", json!({"ok": false, "error": reject})).and_then(|()| out.flush());
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(playhead: i64, sel: &[u64], playing: bool, undo: usize) -> Value {
        json!({"playhead": playhead, "selection": sel, "playing": playing, "activeSequence": 1, "undo": undo, "redo": 0,
               "last": if undo > 0 { json!("Edit") } else { Value::Null }})
    }

    #[test]
    fn watch_reports_selection_playback_and_edits_only_when_they_change() {
        let a = st(0, &[], false, 0);
        assert!(diff_events(&a, &a).is_empty());
        let kinds = |v: Vec<Value>| v.iter().map(|e| e["event"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>();
        assert_eq!(kinds(diff_events(&a, &st(0, &[7], false, 0))), ["selection"]);
        assert_eq!(kinds(diff_events(&a, &st(10, &[], true, 0))), ["playback"]);
        assert_eq!(kinds(diff_events(&a, &st(0, &[], false, 1))), ["edit"]);
        // a moving playhead alone is not an event here: the watcher reports it once parked
        assert!(diff_events(&a, &st(99, &[], false, 0)).is_empty());
    }

    #[test]
    fn watch_request_is_acknowledged_and_ends_request_reading() {
        let input = b"{\"id\":1,\"method\":\"events.watch\"}\n{\"id\":2,\"method\":\"ui.inspect\"}\n";
        let mut out = Vec::new();
        let mut seen = Vec::new();
        serve_lines(&input[..], &mut out, |method, _| {
            seen.push(method.clone());
            Some(if method == WATCH_METHOD { json!({"ok": true, CLOSE_AFTER: true}) } else { json!({"ok": true}) })
        });
        assert_eq!(seen, [WATCH_METHOD]);
        let text = String::from_utf8(out).unwrap_or_default();
        assert_eq!(text.lines().count(), 1);
        assert!(!text.contains(CLOSE_AFTER), "{text}");
    }

    fn run(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(input, &mut out, |m, _p| {
            calls.push(m);
            Some(json!({"ok": true, "result": null}))
        });
        let replies = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
        (replies, calls)
    }

    #[test]
    fn requests_are_answered_in_order_with_ids() {
        let (r, calls) = run(b"{\"id\":1,\"method\":\"a\"}\n\n{\"id\":2,\"method\":\"b\",\"params\":{}}\r\n{\"method\":\"c\"}");
        assert_eq!(calls, ["a", "b", "c"]);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0]["id"], 1);
        assert_eq!(r[1]["id"], 2);
        assert_eq!(r[2]["id"], Value::Null);
    }

    #[test]
    fn http_request_closes_before_the_body_runs() {
        // A browser's cross-origin `fetch` POST (issue #119): the request line is rejected and the
        // connection closed, so the JSON body is never executed.
        let http = b"POST / HTTP/1.1\r\nHost: 127.0.0.1:9876\r\nOrigin: https://attacker.example\r\nContent-Type: text/plain\r\n\r\n\
            {\"id\":7,\"method\":\"engine.execute\",\"params\":{\"command\":\"file.openDemoProject\",\"params\":{}}}\n";
        let (r, calls) = run(http);
        assert!(calls.is_empty(), "nothing may run: {calls:?}");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["ok"], false);
        assert!(r[0]["error"].as_str().unwrap().contains("closing the connection"));
    }

    #[test]
    fn non_request_json_closes_the_connection() {
        for junk in [
            &b"[1,2]\n{\"method\":\"a\"}\n"[..],
            b"{\"id\":1}\n{\"method\":\"a\"}\n",
            b"{\"method\":5}\n{\"method\":\"a\"}\n",
            b"\xff\xfe\n{\"method\":\"a\"}\n",
        ] {
            let (r, calls) = run(junk);
            assert!(calls.is_empty(), "{calls:?}");
            assert_eq!(r.len(), 1);
            assert_eq!(r[0]["ok"], false);
        }
    }

    #[test]
    fn a_request_before_junk_still_runs() {
        let (r, calls) = run(b"{\"method\":\"a\"}\nGET / HTTP/1.1\n{\"method\":\"b\"}\n");
        assert_eq!(calls, ["a"]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1]["ok"], false);
    }

    #[test]
    fn overlong_line_is_refused() {
        // One byte over the limit, then a valid request: neither runs.
        let mut input = vec![b'x'; MAX_LINE + 1];
        input.extend_from_slice(b"\n{\"method\":\"a\"}\n");
        let (r, calls) = run(&input);
        assert!(calls.is_empty(), "{calls:?}");
        assert_eq!(r.len(), 1);
        assert!(r[0]["error"].as_str().unwrap().contains("longer than"));
    }

    #[test]
    fn line_at_the_limit_is_accepted() {
        let mut buf = Vec::new();
        let line = vec![b'a'; 10];
        let mut input = line.clone();
        input.push(b'\n');
        assert_eq!(read_bounded_line(&mut &input[..], &mut buf, 10), Ok(true));
        assert_eq!(buf, line);
        assert!(read_bounded_line(&mut &[b'a'; 11][..], &mut buf, 10).is_err());
    }

    #[test]
    fn tcp_connection_is_closed_after_junk() {
        // End to end over a real loopback socket (the transport the app uses).
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let read = s.try_clone().unwrap();
            serve_lines(read, s, |_, _| Some(json!({"ok": true})));
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(b"{\"id\":7,\"method\":\"x\"}\nhello\n{\"method\":\"y\"}\n").unwrap();
        let mut text = String::new();
        c.read_to_string(&mut text).unwrap(); // returns at EOF: the server closed the connection
        server.join().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].contains("\"id\":7"));
        assert!(lines[1].contains("closing the connection"));
    }

    #[test]
    fn extra_connections_are_refused() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, _rx) = channel::<ControlRequest>();
        std::thread::spawn(move || accept_loop(listener, tx, egui::Context::default()));
        // Idle connections are accepted in order and held open.
        let held: Vec<TcpStream> = (0..16).map(|_| TcpStream::connect(addr).unwrap()).collect();
        let mut extra = TcpStream::connect(addr).unwrap();
        extra.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        BufReader::new(&mut extra).read_line(&mut line).unwrap();
        assert!(line.contains("too many control connections"), "{line}");
        drop(held);
    }
}
