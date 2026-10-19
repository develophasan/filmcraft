//! Assistant panel: a docked panel that an external agent (an AI assistant, a script) drives over
//! the control channel, so an editing assistant lives inside FilmCraft without FilmCraft knowing
//! anything about models or prompts.
//!
//! The agent fills the panel with `agent.set` (title, status, context line, action buttons,
//! suggestions, log messages; see [`AgentPanel::set`]) and receives what the user does, in order,
//! with `agent.poll` or as `{"event":"agent", …}` lines of `events.watch`: `{"kind":"input","text"}`
//! (the input box), `{"kind":"action","id"}` (an action button), `{"kind":"suggestion","id",
//! "accept":bool}` (✓ / x on a suggestion). A suggestion's time label moves the playhead there itself.
//! The panel shows "not connected" when no agent has called in for [`STALE_SECS`].
//!
//! Automation ids: `agent.input`, `agent.send`, `agent.action.<id>`, `agent.suggestion.<id>.time`,
//! `agent.suggestion.<id>.accept`, `agent.suggestion.<id>.dismiss`.

use std::collections::VecDeque;

use egui::{Color32, Rect, RichText, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;

/// Log lines kept (oldest dropped first).
pub const MAX_LOG: usize = 400;
/// User events waiting for the agent (oldest dropped first when it stops polling).
pub const MAX_INBOX: usize = 64;
/// Actions / suggestions accepted per `agent.set`.
pub const MAX_ITEMS: usize = 64;
/// Longest text accepted for one field (characters).
const MAX_TEXT: usize = 4000;
/// Seconds without an `agent.set` / `agent.poll` after which the agent counts as gone.
pub const STALE_SECS: f64 = 15.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentAction {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub tip: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSuggestion {
    pub id: String,
    pub text: String,
    /// Sequence time the suggestion is about (seconds); its label jumps the playhead there.
    #[serde(default)]
    pub seconds: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentMessage {
    /// `user`, `assistant`, `tool`, `ok` or `error` (colours the line).
    pub role: String,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct AgentPanel {
    pub title: String,
    pub status: String,
    pub context: String,
    pub hint: String,
    pub busy: bool,
    pub actions: Vec<AgentAction>,
    pub suggestions: Vec<AgentSuggestion>,
    pub log: VecDeque<AgentMessage>,
    pub inbox: VecDeque<Value>,
    pub draft: String,
    /// UI time of the agent's last call (None: never connected).
    pub last_seen: Option<f64>,
}

fn clip(s: &str) -> String {
    s.chars().take(MAX_TEXT).collect()
}

impl AgentPanel {
    pub fn connected(&self, now: f64) -> bool {
        self.last_seen.is_some_and(|t| now - t <= STALE_SECS)
    }

    /// `agent.set {title?, status?, context?, hint?, busy?, actions?, suggestions?, append?, clearLog?}`:
    /// fields given replace the current ones (`actions` / `suggestions` replace the whole list);
    /// `append` adds `[{role, text}]` to the log. Returns the panel state.
    pub fn set(&mut self, p: &Value, now: f64) -> Result<Value, String> {
        let text = |k: &str| p.get(k).and_then(Value::as_str).map(clip);
        if let Some(v) = text("title") {
            self.title = v;
        }
        if let Some(v) = text("status") {
            self.status = v;
        }
        if let Some(v) = text("context") {
            self.context = v;
        }
        if let Some(v) = text("hint") {
            self.hint = v;
        }
        if let Some(v) = p.get("busy").and_then(Value::as_bool) {
            self.busy = v;
        }
        if let Some(a) = p.get("actions") {
            let list: Vec<AgentAction> = serde_json::from_value(a.clone()).map_err(|e| format!("`actions`: {e}"))?;
            self.actions = list.into_iter().take(MAX_ITEMS).map(|a| AgentAction { id: clip(&a.id), label: clip(&a.label), tip: clip(&a.tip) }).collect();
        }
        if let Some(a) = p.get("suggestions") {
            let list: Vec<AgentSuggestion> = serde_json::from_value(a.clone()).map_err(|e| format!("`suggestions`: {e}"))?;
            self.suggestions = list
                .into_iter()
                .take(MAX_ITEMS)
                .map(|s| AgentSuggestion { id: clip(&s.id), text: clip(&s.text), seconds: s.seconds.filter(|v| v.is_finite() && *v >= 0.0) })
                .collect();
        }
        if p.get("clearLog").and_then(Value::as_bool).unwrap_or(false) {
            self.log.clear();
        }
        if let Some(a) = p.get("append") {
            let list: Vec<AgentMessage> = serde_json::from_value(a.clone()).map_err(|e| format!("`append`: {e}"))?;
            for m in list.into_iter().take(MAX_ITEMS) {
                self.push_log(&m.role, &m.text);
            }
        }
        self.last_seen = Some(now);
        Ok(self.state(now))
    }

    /// `agent.poll`: the user's events since the last poll, oldest first.
    pub fn poll(&mut self, now: f64) -> Vec<Value> {
        self.last_seen = Some(now);
        self.inbox.drain(..).collect()
    }

    pub fn state(&self, now: f64) -> Value {
        json!({"connected": self.connected(now), "title": self.title, "status": self.status, "busy": self.busy,
               "actions": self.actions, "suggestions": self.suggestions, "log": self.log.len(), "pending": self.inbox.len()})
    }

    pub fn push_log(&mut self, role: &str, text: &str) {
        self.log.push_back(AgentMessage { role: clip(role), text: clip(text) });
        while self.log.len() > MAX_LOG {
            self.log.pop_front();
        }
    }

    pub fn push_event(&mut self, ev: Value) {
        self.inbox.push_back(ev);
        while self.inbox.len() > MAX_INBOX {
            self.inbox.pop_front();
        }
    }

    /// The user sends the input box: logged as theirs and queued for the agent.
    pub fn submit(&mut self) {
        let text = self.draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.push_log("user", &text);
        self.push_event(json!({"kind": "input", "text": text}));
        self.draft.clear();
    }
}

fn mmss(s: f64) -> String {
    let t = s.max(0.0) as u64;
    format!("{}:{:02}", t / 60, t % 60)
}

enum Click {
    Action(String),
    Suggestion(String, bool),
    Jump(f64),
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let now = ui.input(|i| i.time);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 6.0))));
    let ui = &mut child;
    let mut clicks: Vec<Click> = Vec::new();
    let mut boxes: Vec<(String, Rect, String)> = Vec::new();
    let connected = app.agent.connected(now);
    let a = &mut app.agent;

    ui.horizontal(|ui| {
        let title = if a.title.is_empty() { crate::i18n::t("Assistant").to_string() } else { a.title.clone() };
        ui.label(RichText::new(title).strong().color(t.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if a.busy {
                ui.add(egui::Spinner::new());
            }
            let (dot, color) = if connected { ("●", t.accent) } else { ("○", t.text_faint) };
            let status = if !connected {
                crate::i18n::t("not connected").to_string()
            } else if a.status.is_empty() {
                crate::i18n::t("connected").to_string()
            } else {
                a.status.clone()
            };
            ui.label(RichText::new(format!("{dot} {status}")).color(color).small());
        });
    });
    if !a.context.is_empty() {
        ui.add(egui::Label::new(RichText::new(&a.context).color(t.text_dim).small()).wrap());
    }
    if !a.actions.is_empty() {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for act in &a.actions {
                let mut b = ui.add_enabled(connected && !a.busy, egui::Button::new(&act.label));
                if !act.tip.is_empty() {
                    b = b.on_hover_text(&act.tip);
                }
                if b.clicked() {
                    clicks.push(Click::Action(act.id.clone()));
                }
                boxes.push((format!("agent.action.{}", act.id), b.rect, act.label.clone()));
            }
        });
    }
    if !a.suggestions.is_empty() {
        ui.add_space(4.0);
        ui.separator();
        egui::ScrollArea::vertical().id_salt("agent-suggestions").max_height(ui.available_height() * 0.4).show(ui, |ui| {
            for s in &a.suggestions {
                ui.horizontal(|ui| {
                    if let Some(sec) = s.seconds {
                        let b = ui.small_button(mmss(sec));
                        if b.clicked() {
                            clicks.push(Click::Jump(sec));
                        }
                        boxes.push((format!("agent.suggestion.{}.time", s.id), b.rect, mmss(sec)));
                    }
                    let ok = ui.add_enabled(connected && !a.busy, egui::Button::new("✓").small());
                    let no = ui.add(egui::Button::new("x").small());
                    if ok.clicked() {
                        clicks.push(Click::Suggestion(s.id.clone(), true));
                    }
                    if no.clicked() {
                        clicks.push(Click::Suggestion(s.id.clone(), false));
                    }
                    boxes.push((format!("agent.suggestion.{}.accept", s.id), ok.rect, s.text.clone()));
                    boxes.push((format!("agent.suggestion.{}.dismiss", s.id), no.rect, s.text.clone()));
                    ui.add(egui::Label::new(RichText::new(&s.text).color(t.text)).wrap());
                });
            }
        });
    }
    ui.separator();
    let input_h = 30.0;
    let log_h = (ui.available_height() - input_h - 8.0).max(40.0);
    egui::ScrollArea::vertical().id_salt("agent-log").max_height(log_h).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        for m in &a.log {
            let color = match m.role.as_str() {
                "user" => t.text_dim,
                "tool" => t.text_faint,
                "error" => Color32::from_rgb(0xff, 0x8a, 0x7a),
                "ok" => t.accent,
                _ => t.text,
            };
            let text = if m.role == "user" { format!("› {}", m.text) } else { m.text.clone() };
            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
        }
    });
    let mut send = false;
    ui.horizontal(|ui| {
        let hint = if a.hint.is_empty() { crate::i18n::t("Ask the assistant…").to_string() } else { a.hint.clone() };
        let w = (ui.available_width() - 64.0).max(60.0);
        let r = ui.add_enabled(connected, egui::TextEdit::singleline(&mut a.draft).id_salt("agent-input").hint_text(hint).desired_width(w));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            send = true;
            r.request_focus();
        }
        let b = ui.add_enabled(connected && !a.draft.trim().is_empty(), egui::Button::new(crate::i18n::t("Send")));
        send |= b.clicked();
        boxes.push(("agent.input".into(), r.rect, a.draft.clone()));
        boxes.push(("agent.send".into(), b.rect, crate::i18n::t("Send").to_string()));
    });
    if send && connected {
        a.submit();
    }
    for c in clicks {
        match c {
            Click::Action(id) => app.agent.push_event(json!({"kind": "action", "id": id})),
            Click::Suggestion(id, accept) => {
                if !accept {
                    app.agent.suggestions.retain(|s| s.id != id);
                }
                app.agent.push_event(json!({"kind": "suggestion", "id": id, "accept": accept}));
            }
            Click::Jump(sec) => {
                if let Err(e) = app.session.execute("playhead.set", json!({"seconds": sec})) {
                    app.ui.status = e.to_string();
                }
            }
        }
    }
    for (id, r, label) in boxes {
        app.auto.add(&id, r, &label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_replaces_lists_appends_log_and_marks_connected() {
        let mut a = AgentPanel::default();
        assert!(!a.connected(0.0));
        let st = a
            .set(&json!({"title": "Efsane", "actions": [{"id": "sub", "label": "Abone ol"}],
                          "suggestions": [{"id": "s1", "text": "Uyarı balonu", "seconds": 12.5}, {"id": "s2", "text": "x", "seconds": -3}],
                          "append": [{"role": "assistant", "text": "hazır"}]}), 10.0)
            .unwrap();
        assert_eq!(st["connected"], true);
        assert_eq!(a.actions.len(), 1);
        assert_eq!(a.suggestions[0].seconds, Some(12.5));
        assert_eq!(a.suggestions[1].seconds, None, "negative times are dropped");
        assert_eq!(a.log.len(), 1);
        assert!(a.connected(10.0 + STALE_SECS));
        assert!(!a.connected(10.0 + STALE_SECS + 1.0));
        assert!(a.set(&json!({"actions": "nope"}), 11.0).is_err());
    }

    #[test]
    fn input_and_events_reach_the_inbox_in_order_and_poll_drains_it() {
        let mut a = AgentPanel::default();
        a.draft = "  buraya balon koy ".into();
        a.submit();
        a.push_event(json!({"kind": "action", "id": "sub"}));
        a.draft = "   ".into();
        a.submit();
        let got = a.poll(1.0);
        assert_eq!(got, vec![json!({"kind": "input", "text": "buraya balon koy"}), json!({"kind": "action", "id": "sub"})]);
        assert!(a.poll(2.0).is_empty());
        assert_eq!(a.log.back().map(|m| m.role.as_str()), Some("user"));
    }

    #[test]
    fn log_and_inbox_are_bounded() {
        let mut a = AgentPanel::default();
        for i in 0..(MAX_LOG + 10) {
            a.push_log("tool", &i.to_string());
        }
        for i in 0..(MAX_INBOX + 10) {
            a.push_event(json!(i));
        }
        assert_eq!(a.log.len(), MAX_LOG);
        assert_eq!(a.inbox.len(), MAX_INBOX);
        assert_eq!(a.inbox.front(), Some(&json!(10)));
    }
}
