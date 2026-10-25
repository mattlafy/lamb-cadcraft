//! A generic chat palette (fork-only, LAMB T425): a server field, a model drop-down, a transcript
//! and a composer over one registered [`ChatBackend`]. A turn's steps run on the UI thread against
//! the session; only the model call ([`ChatBackend::send`]) and the model list run on a worker
//! thread, so the window keeps painting while the server thinks. It knows nothing of what the
//! backend is; the LAMB desktop app registers its assistant into it.

use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use serde_json::Value;

use cadcraft_engine::Session;

use crate::CadApp;
use crate::theme::Tokens;

/// What a turn step asks for next.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatStep {
    /// Send `messages` to the model, then resume `turn` with the reply.
    Ask { turn: Value, messages: Value },
    /// The turn is over; `text` is the assistant's line. `candidate` is an opaque payload the
    /// palette lists for the user to flip through and take, instead of the turn applying itself.
    Done { text: String, candidate: Option<Value> },
}

/// The functions a chat backend provides.
#[derive(Clone, Copy)]
pub struct ChatBackend {
    pub default_base_url: &'static str,
    /// Begin a turn for the user's text (UI thread).
    pub start: fn(&mut Session, &str) -> Result<ChatStep, String>,
    /// Continue a turn with the model's reply (UI thread).
    pub resume: fn(&mut Session, &Value, String) -> Result<ChatStep, String>,
    /// One model call: (base URL, model, messages) → reply text. Runs on a worker thread.
    pub send: fn(&str, &str, &Value) -> Result<String, String>,
    /// The models the server lists. Runs on a worker thread.
    pub models: fn(&str) -> Result<Vec<String>, String>,
    /// A candidate row's text (UI thread).
    pub label: fn(&Value) -> String,
    /// Show a candidate on the canvas, or clear it with `None` (UI thread).
    pub preview: fn(&mut Session, Option<&Value>) -> Result<(), String>,
    /// Use a candidate for real; Ok is the transcript line to show (UI thread).
    pub take: fn(&mut Session, &Value) -> Result<String, String>,
}

static BACKEND: OnceLock<ChatBackend> = OnceLock::new();

/// Register the backend. Once per process; a second call is an error.
pub fn register_backend(b: ChatBackend) -> Result<(), &'static str> {
    BACKEND.set(b).map_err(|_| "chat backend already registered")
}

/// The palette's state, kept on [`CadApp::chat`].
#[derive(Default)]
pub struct ChatState {
    /// (role, text): `user`, `assistant`, `error` or `note`.
    pub transcript: Vec<(String, String)>,
    /// Server base URL and model name the next call uses.
    pub base_url: String,
    pub model: String,
    /// The models the server listed.
    pub models: Vec<String>,
    /// The composer's text.
    pub draft: String,
    /// The turn waiting on the model's reply.
    turn: Option<Value>,
    reply_rx: Option<Receiver<Result<String, String>>>,
    models_rx: Option<Receiver<Result<Vec<String>, String>>>,
    /// Settings were read from egui memory and the model list requested.
    loaded: bool,
    /// The candidates the current query produced, and the highlighted one.
    candidates: Vec<Value>,
    highlight: usize,
    /// The last query sent, which Try again re-runs.
    query: Option<String>,
}

impl ChatState {
    /// A turn is waiting on the model.
    pub fn busy(&self) -> bool {
        self.reply_rx.is_some()
    }
    /// The model list is being fetched.
    pub fn listing(&self) -> bool {
        self.models_rx.is_some()
    }
    /// The candidate payloads, in the order they arrived.
    pub fn candidates(&self) -> &[Value] {
        &self.candidates
    }
    /// The highlighted candidate's index.
    pub fn highlight(&self) -> usize {
        self.highlight
    }
    fn push(&mut self, role: &str, text: impl Into<String>) {
        self.transcript.push((role.to_string(), text.into()));
    }
    fn fail(&mut self, msg: impl Into<String>) {
        self.turn = None;
        self.reply_rx = None;
        self.push("error", msg);
    }
}

/// Start a turn for `text`. Ignored while a turn is running, with no model, or empty text.
pub fn send(app: &mut CadApp, text: &str) {
    let text = text.trim();
    if app.chat.busy() || app.chat.model.is_empty() || text.is_empty() {
        return;
    }
    app.chat.push("user", text);
    let Some(b) = BACKEND.get().copied() else {
        app.chat.push("error", crate::i18n::t("No assistant backend"));
        return;
    };
    app.chat.query = Some(text.to_string());
    app.chat.candidates.clear();
    app.chat.highlight = 0;
    preview(app, b, None);
    let step = (b.start)(&mut app.session, text);
    advance(app, b, step);
}

/// Re-run the last query for another candidate, with no new user line. Ignored while busy, with
/// no model or no query.
pub fn try_again(app: &mut CadApp) {
    if app.chat.busy() || app.chat.model.is_empty() {
        return;
    }
    let (Some(text), Some(b)) = (app.chat.query.clone(), BACKEND.get().copied()) else { return };
    let step = (b.start)(&mut app.session, &text);
    advance(app, b, step);
}

/// Highlight candidate `i` and preview it. Out of range is ignored.
pub fn select(app: &mut CadApp, i: usize) {
    let Some(b) = BACKEND.get().copied() else { return };
    if i >= app.chat.candidates.len() {
        return;
    }
    app.chat.highlight = i;
    let c = app.chat.candidates[i].clone();
    preview(app, b, Some(&c));
}

/// Use the highlighted candidate: its line joins the transcript and the list clears; a failure is
/// an error line and the list stays.
pub fn take(app: &mut CadApp) {
    let Some(b) = BACKEND.get().copied() else { return };
    let Some(c) = app.chat.candidates.get(app.chat.highlight).cloned() else { return };
    match (b.take)(&mut app.session, &c) {
        Ok(line) => {
            app.chat.push("assistant", line);
            app.chat.candidates.clear();
            app.chat.highlight = 0;
            preview(app, b, None);
        }
        Err(e) => app.chat.push("error", e),
    }
}

/// Tab on the command line: move the highlight to the next candidate (wrapping). Consumed only
/// while candidates exist, nothing is typed and no command is running.
pub fn tab(app: &mut CadApp) -> bool {
    let n = app.chat.candidates.len();
    if n == 0 || !app.cmd.buffer.is_empty() || app.session.running.is_some() {
        return false;
    }
    select(app, (app.chat.highlight + 1) % n);
    true
}

fn preview(app: &mut CadApp, b: ChatBackend, c: Option<&Value>) {
    if let Err(e) = (b.preview)(&mut app.session, c) {
        app.chat.push("error", e);
    }
}

/// Act on a step: ask the model on a worker thread, or finish the turn.
fn advance(app: &mut CadApp, b: ChatBackend, step: Result<ChatStep, String>) {
    match step {
        Err(e) => app.chat.fail(e),
        Ok(ChatStep::Done { text, candidate }) => {
            app.chat.turn = None;
            app.chat.reply_rx = None;
            app.chat.push("assistant", text);
            if let Some(c) = candidate {
                app.chat.candidates.push(c);
                if app.chat.candidates.len() == 1 {
                    app.chat.highlight = 0;
                    let c = app.chat.candidates[0].clone();
                    preview(app, b, Some(&c));
                }
            }
        }
        Ok(ChatStep::Ask { turn, messages }) => {
            let (tx, rx) = channel();
            let (base, model, send) = (app.chat.base_url.clone(), app.chat.model.clone(), b.send);
            let spawned = std::thread::Builder::new().name("cadcraft-chat".into()).spawn(move || {
                let _ = tx.send(send(&base, &model, &messages));
            });
            match spawned {
                Ok(_) => {
                    app.chat.turn = Some(turn);
                    app.chat.reply_rx = Some(rx);
                }
                Err(e) => app.chat.fail(format!("could not start the model call: {e}")),
            }
        }
    }
}

/// Fetch the server's model list on a worker thread.
pub fn refresh_models(app: &mut CadApp) {
    let Some(b) = BACKEND.get().copied() else { return };
    let (tx, rx) = channel();
    let (base, models) = (app.chat.base_url.clone(), b.models);
    match std::thread::Builder::new().name("cadcraft-chat-models".into()).spawn(move || {
        let _ = tx.send(models(&base));
    }) {
        Ok(_) => app.chat.models_rx = Some(rx),
        Err(e) => app.chat.push("note", format!("Model list unavailable: could not start a thread: {e}")),
    }
}

/// Take finished worker results. Called every frame; true while a worker is still running.
pub fn poll(app: &mut CadApp) -> bool {
    if let Some(rx) = &app.chat.models_rx {
        match rx.try_recv() {
            Ok(r) => {
                app.chat.models_rx = None;
                match r {
                    Ok(list) => {
                        if !list.contains(&app.chat.model) {
                            app.chat.model = list.first().cloned().unwrap_or_default();
                        }
                        app.chat.models = list;
                    }
                    Err(e) => app.chat.push("note", format!("Model list unavailable: {e}")),
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                app.chat.models_rx = None;
                app.chat.push("note", "Model list unavailable: the request ended without an answer");
            }
        }
    }
    if let Some(rx) = &app.chat.reply_rx {
        let reply = match rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("the model call ended without an answer".to_string())),
        };
        if let Some(reply) = reply {
            app.chat.reply_rx = None;
            match (reply, app.chat.turn.take(), BACKEND.get().copied()) {
                (Err(e), _, _) => app.chat.fail(e),
                (Ok(text), Some(turn), Some(b)) => {
                    let step = (b.resume)(&mut app.session, &turn, text);
                    advance(app, b, step);
                }
                _ => app.chat.fail("the turn was lost"),
            }
        }
    }
    app.chat.busy() || app.chat.listing()
}

const BASE_KEY: &str = "cadcraft-chat-base-url";
const MODEL_KEY: &str = "cadcraft-chat-model";

fn persist(ctx: &egui::Context, key: &str, value: &str) {
    ctx.data_mut(|d| d.insert_persisted(egui::Id::unique(key), value.to_string()));
}

pub fn show(app: &mut CadApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let Some(b) = BACKEND.get().copied() else {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(egui::RichText::new(crate::i18n::t("No assistant backend")).color(t.text_dim));
        });
        return;
    };
    let ctx = ui.ctx().clone();
    if !app.chat.loaded {
        app.chat.loaded = true;
        let (base, model) =
            ctx.data_mut(|d| (d.get_persisted::<String>(egui::Id::unique(BASE_KEY)), d.get_persisted::<String>(egui::Id::unique(MODEL_KEY))));
        app.chat.base_url = base.filter(|s| !s.is_empty()).unwrap_or_else(|| b.default_base_url.to_string());
        if app.chat.model.is_empty() {
            app.chat.model = model.unwrap_or_default();
        }
        refresh_models(app);
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(crate::i18n::t("Server"));
        let r = ui.add(egui::TextEdit::singleline(&mut app.chat.base_url).desired_width(ui.available_width() - 10.0));
        if r.changed() {
            persist(&ctx, BASE_KEY, &app.chat.base_url);
        }
    });
    let mut refresh = false;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(crate::i18n::t("Model"));
        let before = app.chat.model.clone();
        let shown = if app.chat.model.is_empty() { crate::i18n::t("(none)").to_string() } else { app.chat.model.clone() };
        egui::ComboBox::from_id_salt("cadcraft-chat-model").selected_text(shown).width((ui.available_width() - 44.0).max(60.0)).show_ui(ui, |ui| {
            for m in app.chat.models.clone() {
                ui.selectable_value(&mut app.chat.model, m.clone(), m);
            }
        });
        if app.chat.model != before {
            persist(&ctx, MODEL_KEY, &app.chat.model);
        }
        refresh = ui.add_enabled(!app.chat.listing(), egui::Button::new("⟳")).on_hover_text(crate::i18n::t("Refresh the model list")).clicked();
    });
    if refresh {
        refresh_models(app);
    }
    ui.separator();
    let rows_h = if app.chat.candidates.is_empty() { 0.0 } else { 30.0 + 20.0 * app.chat.candidates.len().min(5) as f32 };
    let composer_h = 64.0 + rows_h;
    egui::ScrollArea::vertical()
        .id_salt("cadcraft-chat-transcript")
        .auto_shrink([false, false])
        .max_height((ui.available_height() - composer_h - 16.0).max(40.0))
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for (role, text) in &app.chat.transcript {
                let (label, color) = match role.as_str() {
                    "user" => ("You", t.text),
                    "assistant" => ("Assistant", t.text),
                    "error" => ("Error", t.warn),
                    _ => ("", t.text_dim),
                };
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(8.0);
                    if !label.is_empty() {
                        ui.label(egui::RichText::new(crate::i18n::t(label)).strong().color(color));
                    }
                    ui.label(egui::RichText::new(text).color(color));
                });
            }
            if app.chat.busy() {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("…").color(t.text_dim));
                });
            }
        });
    if !app.chat.candidates.is_empty() {
        ui.separator();
        let mut pick = None;
        egui::ScrollArea::vertical().id_salt("cadcraft-chat-candidates").max_height(100.0).show(ui, |ui| {
            for (i, c) in app.chat.candidates.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if ui.selectable_label(i == app.chat.highlight, format!("#{} — {}", i + 1, (b.label)(c))).clicked() {
                        pick = Some(i);
                    }
                });
            }
        });
        if let Some(i) = pick {
            select(app, i);
        }
        let (mut again, mut use_it) = (false, false);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let label = format!("{} {}", crate::i18n::t("Use candidate"), app.chat.highlight + 1);
            use_it = ui.add_enabled(!app.chat.busy(), egui::Button::new(label)).clicked();
            again = ui.add_enabled(!app.chat.busy() && !app.chat.model.is_empty(), egui::Button::new(crate::i18n::t("Try again"))).clicked();
        });
        if use_it {
            take(app);
        } else if again {
            try_again(app);
        }
    }
    ui.separator();
    let mut go = false;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let r = ui.add(egui::TextEdit::multiline(&mut app.chat.draft).desired_rows(3).desired_width(ui.available_width() - 70.0));
        if r.has_focus() && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
            go = true;
        }
        let can = !app.chat.busy() && !app.chat.model.is_empty() && !app.chat.draft.trim().is_empty();
        if ui.add_enabled(can, egui::Button::new(crate::i18n::t("Send"))).clicked() {
            go = true;
        }
    });
    if go && !app.chat.busy() && !app.chat.model.is_empty() && !app.chat.draft.trim().is_empty() {
        let text = std::mem::take(&mut app.chat.draft);
        send(app, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use serde_json::json;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Asks once ("twice": three times); the turn carries the user's text and the replies seen.
    /// Done draws a line. A user text "fail" makes the model call fail.
    fn start(_: &mut Session, text: &str) -> Result<ChatStep, String> {
        Ok(ChatStep::Ask { turn: json!({"text": text, "replies": 0}), messages: json!([{"role": "user", "content": text}]) })
    }
    fn resume(s: &mut Session, turn: &Value, reply: String) -> Result<ChatStep, String> {
        let replies = turn["replies"].as_u64().unwrap() + 1;
        let text = turn["text"].as_str().unwrap().to_string();
        if text == "twice" && replies < 3 {
            return Ok(ChatStep::Ask { turn: json!({"text": text, "replies": replies}), messages: json!([{"role": "user", "content": text}]) });
        }
        if text.starts_with("cand") {
            let n = CANDS.with(|c| {
                c.set(c.get() + 1);
                c.get()
            });
            return Ok(ChatStep::Done { text: format!("candidate {n}"), candidate: Some(json!({"n": n, "summary": format!("{n} new")})) });
        }
        s.execute("line", &json!({"points": [[0, 0], [100, 0]]})).map_err(|e| e.to_string())?;
        Ok(ChatStep::Done { text: format!("{reply}: drew a line after {replies} replies"), candidate: None })
    }
    /// Model calls for "cand draw" (only the candidate test sends it; the call runs on a worker).
    static CAND_SENDS: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        /// Every preview and take the backend saw, on this test's thread.
        static SEEN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// Candidates this test's thread has made (resume runs on the UI thread).
        static CANDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    fn seen() -> Vec<String> {
        SEEN.with(|v| v.borrow().clone())
    }
    fn label(c: &Value) -> String {
        c["summary"].as_str().unwrap_or_default().to_string()
    }
    fn preview_fn(_: &mut Session, c: Option<&Value>) -> Result<(), String> {
        SEEN.with(|v| v.borrow_mut().push(format!("preview {}", c.map_or("none".to_string(), |c| c["n"].to_string()))));
        Ok(())
    }
    fn take_fn(_: &mut Session, c: &Value) -> Result<String, String> {
        SEEN.with(|v| v.borrow_mut().push(format!("take {}", c["n"])));
        if c["fail"] == json!(true) { Err("take failed".into()) } else { Ok(format!("Applied {}", c["n"])) }
    }
    fn send_fn(_: &str, model: &str, messages: &Value) -> Result<String, String> {
        if messages[0]["content"] == "cand draw" {
            CAND_SENDS.fetch_add(1, Ordering::SeqCst);
        }
        std::thread::sleep(Duration::from_millis(20));
        if messages[0]["content"] == "fail" { Err("server down".into()) } else { Ok(format!("ok from {model}")) }
    }
    fn models(_: &str) -> Result<Vec<String>, String> {
        Ok(vec!["m1".into(), "m2".into()])
    }
    const STUB: ChatBackend =
        ChatBackend { default_base_url: "http://stub/v1", start, resume, send: send_fn, models, label, preview: preview_fn, take: take_fn };

    fn app() -> CadApp {
        let _ = register_backend(STUB);
        let mut app = CadApp::new(Session::default(), crate::Services::default());
        app.chat.model = "m1".into();
        app
    }

    fn run_until_idle(app: &mut CadApp) {
        let t0 = Instant::now();
        while poll(app) {
            assert!(t0.elapsed() < Duration::from_secs(2), "the turn did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn lines(app: &CadApp) -> usize {
        app.session.doc().unwrap().model.iter().count()
    }

    fn roles(app: &CadApp) -> Vec<&str> {
        app.chat.transcript.iter().map(|(r, _)| r.as_str()).collect()
    }

    #[test]
    fn a_turn_runs_the_model_off_thread_and_its_resume_draws() {
        let mut app = app();
        assert!(register_backend(STUB).is_err(), "one backend per process");
        let before = lines(&app);
        send(&mut app, "draw a line");
        assert!(app.chat.busy(), "the model call is on a worker");
        run_until_idle(&mut app);
        assert_eq!(roles(&app), ["user", "assistant"]);
        assert_eq!(app.chat.transcript[1].1, "ok from m1: drew a line after 1 replies");
        assert_eq!(lines(&app), before + 1, "the drawing has the line");
    }

    #[test]
    fn a_failed_model_call_is_an_error_line_and_ends_the_turn() {
        let mut app = app();
        send(&mut app, "fail");
        run_until_idle(&mut app);
        assert_eq!(roles(&app), ["user", "error"]);
        assert_eq!(app.chat.transcript[1].1, "server down");
        assert!(!app.chat.busy());
    }

    #[test]
    fn send_while_busy_with_no_model_or_empty_text_is_ignored() {
        let mut app = app();
        send(&mut app, "first");
        send(&mut app, "second");
        run_until_idle(&mut app);
        assert_eq!(roles(&app), ["user", "assistant"]);
        send(&mut app, "   ");
        app.chat.model.clear();
        send(&mut app, "no model");
        assert_eq!(app.chat.transcript.len(), 2);
    }

    #[test]
    fn a_resume_that_asks_again_makes_another_model_call() {
        let mut app = app();
        send(&mut app, "twice");
        run_until_idle(&mut app);
        assert_eq!(roles(&app), ["user", "assistant"]);
        assert_eq!(app.chat.transcript[1].1, "ok from m1: drew a line after 3 replies", "three worker calls before Done");
    }

    #[test]
    fn the_model_list_keeps_a_served_pick_else_takes_the_first() {
        let mut app = app();
        app.chat.model = "m2".into();
        refresh_models(&mut app);
        run_until_idle(&mut app);
        assert_eq!((app.chat.models.len(), app.chat.model.as_str()), (2, "m2"));
        app.chat.model = "gone".into();
        refresh_models(&mut app);
        run_until_idle(&mut app);
        assert_eq!(app.chat.model, "m1");
    }

    #[test]
    fn window_assistant_palette_opens_the_panel_on_demand() {
        use crate::docking::Panel;
        assert!(!crate::docking::default_workspace().layout.contains(&Panel::Assistant), "opened on request");
        let mut app = app();
        app.run("ui.panel.assistant", json!({})).unwrap();
        assert!(app.ui.docking.layout.contains(&Panel::Assistant));
        app.ui.docking.validate().unwrap();
        app.run("ui.dock", json!({"operation": "close", "panel": "assistant"})).unwrap();
        assert!(!app.ui.docking.layout.contains(&Panel::Assistant));
        app.run("ui.dock", json!({"operation": "open", "panel": "assistant"})).unwrap();
        assert!(app.ui.docking.layout.contains(&Panel::Assistant));
    }

    #[test]
    fn candidates_flip_through_and_take_the_highlighted_one() {
        let mut app = app();
        send(&mut app, "cand draw");
        run_until_idle(&mut app);
        assert_eq!(roles(&app), ["user", "assistant"]);
        assert_eq!(app.chat.candidates().len(), 1, "a dry-run Done is candidate #1");
        assert_eq!(label(&app.chat.candidates()[0]), "1 new");
        assert_eq!(seen(), ["preview none", "preview 1"], "send clears, the first candidate previews");
        let sends = CAND_SENDS.load(Ordering::SeqCst);
        try_again(&mut app);
        run_until_idle(&mut app);
        assert_eq!(CAND_SENDS.load(Ordering::SeqCst), sends + 1, "one more worker call");
        assert_eq!(roles(&app), ["user", "assistant", "assistant"], "no new user line");
        assert_eq!((app.chat.candidates().len(), app.chat.highlight()), (2, 0));
        select(&mut app, 1);
        assert_eq!(seen().last().unwrap(), "preview 2");
        // A failing take keeps the list.
        app.chat.candidates[1]["fail"] = json!(true);
        take(&mut app);
        assert_eq!(seen().last().unwrap(), "take 2");
        assert_eq!((roles(&app).last().copied(), app.chat.candidates().len()), (Some("error"), 2));
        app.chat.candidates[1]["fail"] = json!(false);
        take(&mut app);
        assert_eq!(seen()[seen().len() - 2..], ["take 2".to_string(), "preview none".to_string()]);
        assert_eq!(app.chat.transcript.last().unwrap(), &("assistant".to_string(), "Applied 2".to_string()));
        assert!(app.chat.candidates().is_empty());
        // A new send clears the list.
        send(&mut app, "cand again");
        run_until_idle(&mut app);
        assert_eq!(app.chat.candidates().len(), 1);
        send(&mut app, "draw a line");
        assert!(app.chat.candidates().is_empty(), "a new Send clears the list");
        run_until_idle(&mut app);
    }

    #[test]
    fn tab_cycles_candidates_only_with_an_empty_buffer_and_no_command() {
        let mut app = app();
        assert!(!tab(&mut app), "no candidates");
        send(&mut app, "cand tab");
        run_until_idle(&mut app);
        try_again(&mut app);
        run_until_idle(&mut app);
        assert_eq!(app.chat.highlight(), 0);
        assert!(tab(&mut app));
        assert_eq!(app.chat.highlight(), 1);
        assert!(tab(&mut app));
        assert_eq!(app.chat.highlight(), 0, "wraps");
        app.cmd.buffer = "L".into();
        assert!(!tab(&mut app), "typing wins");
        app.cmd.buffer.clear();
        app.session.start("line").unwrap();
        assert!(app.session.running.is_some());
        assert!(!tab(&mut app), "a running command wins");
    }
}
