//! Flush the last completed frame on page hide, even when rendering stops.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::{JsCast, JsValue, closure::Closure};

use super::*;
use crate::components::EditorCamera;
use crate::resources::Notifications;

// The key and payload version stay at v1 during rapid development.
const STORAGE_KEY: &str = "bloq.editor.session.v1";

pub(crate) struct SessionPlugin;

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreStartup, restore_session)
            .add_systems(Last, capture_session);
    }
}

struct PendingSave {
    storage: web_sys::Storage,
    expected: Option<String>,
    session: Option<Session>,
    dirty: bool,
    paused: bool,
    error: Option<String>,
}

impl PendingSave {
    fn flush(&mut self) {
        if !self.dirty || self.paused {
            return;
        }
        let result = (|| {
            if self.storage.get_item(STORAGE_KEY).map_err(js_error)? != self.expected {
                self.paused = true;
                return Err(
                    "Another tab changed this session. Export your work before reloading.".into(),
                );
            }
            let json = serde_json::to_string(self.session.as_ref().expect("dirty session exists"))
                .map_err(|error| error.to_string())?;
            if self.expected.as_ref() != Some(&json) {
                // A failed setItem leaves the previous save intact.
                self.storage
                    .set_item(STORAGE_KEY, &json)
                    .map_err(js_error)?;
                self.expected = Some(json);
            }
            self.dirty = false;
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            self.error = Some(error);
        }
    }
}

struct BrowserSession {
    pending: Rc<RefCell<PendingSave>>,
    window: web_sys::Window,
    document: web_sys::Document,
    listener: Closure<dyn FnMut(web_sys::Event)>,
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        let callback = self.listener.as_ref().unchecked_ref();
        let _ = self
            .document
            .remove_event_listener_with_callback("visibilitychange", callback);
        let _ = self
            .window
            .remove_event_listener_with_callback("pagehide", callback);
    }
}

fn js_error(error: JsValue) -> String {
    format!("{error:?}")
}

fn restore_session(world: &mut World) {
    match initialize(world) {
        Ok(browser) => world.insert_non_send(browser),
        Err(error) => {
            world.resource_mut::<Notifications>().push_error(format!(
                "Browser autosave paused: {error}. Saved session kept unchanged; export any new work before reloading."
            ));
        }
    }
}

fn initialize(world: &mut World) -> Result<BrowserSession, String> {
    let window = web_sys::window().ok_or("Missing browser window")?;
    let document = window.document().ok_or("Missing browser document")?;
    let storage = window
        .local_storage()
        .map_err(js_error)?
        .ok_or("Storage is disabled")?;
    let expected = storage.get_item(STORAGE_KEY).map_err(js_error)?;
    let mut session = None;
    if let Some(json) = &expected {
        let tabs = Session::from_json(json)
            .and_then(Session::restore)
            .map_err(|error| format!("Cannot restore session: {error:#}"))?;
        let snapshot = tabs.active_tab().snapshot.clone();
        // Seed the cache: simply reopening must not rewrite another tab's save.
        session = Some(
            Session::capture(&tabs, (&snapshot).into(), None).map_err(|error| error.to_string())?,
        );
        world.insert_resource(snapshot.graph_state);
        world.insert_resource(snapshot.editor_state);
        world.insert_resource(snapshot.import_export);
        world.insert_resource(snapshot.compile_ui);
        world.insert_resource(tabs);
        world
            .resource_mut::<Notifications>()
            .push_info("Restored browser session");
    }
    let pending = Rc::new(RefCell::new(PendingSave {
        storage,
        expected,
        session,
        dirty: false,
        paused: false,
        error: None,
    }));
    let listener_state = Rc::clone(&pending);
    let listener_document = document.clone();
    let listener = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        if event.type_() == "pagehide" || listener_document.hidden() {
            listener_state.borrow_mut().flush();
        }
    });
    let browser = BrowserSession {
        pending,
        window,
        document,
        listener,
    };
    let callback = browser.listener.as_ref().unchecked_ref();
    browser
        .document
        .add_event_listener_with_callback("visibilitychange", callback)
        .map_err(js_error)?;
    browser
        .window
        .add_event_listener_with_callback("pagehide", callback)
        .map_err(js_error)?;
    Ok(browser)
}

fn capture_session(
    browser: Option<NonSend<BrowserSession>>,
    tabs: Res<EditorTabs>,
    graph: Res<GraphState>,
    editor: Res<EditorState>,
    import: Res<ImportExportState>,
    compile: Res<CompileUiState>,
    camera: Single<&CameraSettings, With<EditorCamera>>,
    time: Res<Time<Real>>,
    mut last_save: Local<f64>,
    mut notifications: ResMut<Notifications>,
) {
    let Some(browser) = browser else { return };
    let mut pending = browser.pending.borrow_mut();
    if !pending.paused {
        let live = TabSource {
            graph: &graph,
            editor: &editor,
            import: &import,
            compile: &compile,
            camera: &camera,
        };
        match Session::capture(&tabs, live, pending.session.as_ref()) {
            Ok(session) if pending.session.as_ref() != Some(&session) => {
                pending.session = Some(session);
                pending.dirty = true;
            }
            Ok(_) => {}
            Err(error) => pending.error = Some(error.to_string()),
        }
        if time.elapsed_secs_f64() - *last_save >= 0.5 {
            pending.flush();
            *last_save = time.elapsed_secs_f64();
        }
    }
    if let Some(error) = pending.error.take() {
        let message = format!("Browser autosave failed: {error}. Export BLOG to keep your work.");
        if !notifications.contains_message(&message) {
            notifications.push_error(message);
        }
    }
}
