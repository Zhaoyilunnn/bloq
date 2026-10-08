//! Run browser compilation in a module Worker so progress can paint between stages.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use bloq_compile::{CompileConfig, CompileContext, CompileStage};
use bloq_graph::BlockGraph;
use bloq_ir::Bloq;
use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use web_sys::{
    DedicatedWorkerGlobalScope, ErrorEvent, Event, MessageEvent, Worker, WorkerOptions, WorkerType,
};

use super::CompileRequest;
use crate::systems::ui::zx_viewer::{
    SimplifiedZxView, decode_zx_worker_source, decode_zx_worker_view, encode_zx_worker_source,
    encode_zx_worker_view, simplified_zx_graph,
};

pub(super) struct WorkerResult {
    pub(super) bloq: Bloq,
    pub(super) compile_duration: Duration,
}

thread_local! {
    // Browser UI work is single-threaded; Bevy's EditorJobs must remain Send + Sync.
    static CURRENT: RefCell<Option<WorkerTask>> = const { RefCell::new(None) };
    static CURRENT_ZX: RefCell<Option<WorkerTask>> = const { RefCell::new(None) };
}

pub(super) fn start(source: &BlockGraph, request: &CompileRequest) -> Result<(), String> {
    CURRENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err("Compilation is already running".into());
        }
        let message = js_sys::Object::new();
        set(
            &message,
            "source",
            &JsValue::from_str(&source.to_blog_text()),
        );
        set(
            &message,
            "codeDistance",
            &JsValue::from_f64(request.code_distance as f64),
        );
        set(
            &message,
            "prepareTWithMpps",
            &JsValue::from_bool(request.prepare_t_with_mpps),
        );
        *slot = Some(WorkerTask::start(message)?);
        Ok(())
    })
}

pub(super) fn poll() -> Option<Result<WorkerResult, String>> {
    CURRENT
        .with(|slot| WorkerTask::poll(&mut slot.borrow_mut()))
        .map(|result| result.and_then(decode_compilation))
}

pub(super) fn stage() -> Option<String> {
    CURRENT.with(|slot| slot.borrow().as_ref().and_then(WorkerTask::stage))
}

pub(super) fn cancel() {
    CURRENT.with(|slot| {
        slot.borrow_mut().take();
    });
}

pub(super) fn start_zx_simplification(graph: &BlockGraph, seed: u64) -> Result<(), String> {
    CURRENT_ZX.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err("ZX simplification is already running".into());
        }
        let message = js_sys::Object::new();
        set(&message, "operation", &JsValue::from_str("simplify-zx"));
        let source = encode_zx_worker_source(graph)
            .map_err(|error| format!("Cannot send ZX source: {error:#}"))?;
        set(&message, "source", &JsValue::from_str(&source));
        // wasm-bindgen's u64 parameter is a BigInt, not a lossy JS Number.
        set(&message, "seed", &JsValue::from_str(&seed.to_string()));
        *slot = Some(WorkerTask::start(message)?);
        Ok(())
    })
}

pub(super) fn poll_zx_simplification() -> Option<Result<SimplifiedZxView, String>> {
    CURRENT_ZX
        .with(|slot| WorkerTask::poll(&mut slot.borrow_mut()))
        .map(|result| {
            result.and_then(|message| {
                let json = field(&message, "graphJson")
                    .as_string()
                    .ok_or("ZX worker returned an invalid graph")?;
                decode_zx_worker_view(&json)
                    .map_err(|error| format!("Cannot read simplified ZX view: {error:#}"))
            })
        })
}

pub(super) fn cancel_zx_simplification() {
    CURRENT_ZX.with(|slot| {
        slot.borrow_mut().take();
    });
}

fn decode_compilation(message: JsValue) -> Result<WorkerResult, String> {
    let bytes: Uint8Array = field(&message, "bytes")
        .dyn_into()
        .map_err(|_| "Compiler returned invalid bytes".to_owned())?;
    let bloq = Bloq::from_binary(&bytes.to_vec())
        .map_err(|error| format!("Cannot read compiled program: {error}"))?;
    let seconds = field(&message, "durationSeconds")
        .as_f64()
        .ok_or("Compiler returned invalid duration")?;
    let compile_duration =
        Duration::try_from_secs_f64(seconds).map_err(|_| "Compiler returned invalid duration")?;
    Ok(WorkerResult {
        bloq,
        compile_duration,
    })
}

struct State {
    stage: Option<String>,
    result: Option<Result<JsValue, String>>,
}

/// One browser job. Dropping it terminates the Worker.
struct WorkerTask {
    worker: Worker,
    state: Rc<RefCell<State>>,
    // Keep callbacks alive until Drop detaches the listeners.
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(ErrorEvent)>,
    _on_message_error: Closure<dyn FnMut(Event)>,
}

impl WorkerTask {
    fn start(request: js_sys::Object) -> Result<Self, String> {
        let options = WorkerOptions::new();
        options.set_type(WorkerType::Module);
        let worker = Worker::new_with_options("./compile_worker.js", &options)
            .map_err(|error| format!("Cannot start background worker: {error:?}"))?;
        let state = Rc::new(RefCell::new(State {
            stage: None,
            result: None,
        }));
        let on_message = {
            let state = Rc::clone(&state);
            let worker = worker.clone();
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let data = event.data();
                match field(&data, "kind").as_string().as_deref() {
                    Some("ready") => {
                        if let Err(error) = worker.post_message(&request) {
                            state.borrow_mut().result =
                                Some(Err(format!("Cannot send worker request: {error:?}")));
                        }
                    }
                    Some("stage") => state.borrow_mut().stage = field(&data, "name").as_string(),
                    Some("complete") => {
                        state.borrow_mut().result = Some(Ok(data));
                    }
                    Some("error") => {
                        state.borrow_mut().result = Some(Err(field(&data, "message")
                            .as_string()
                            .unwrap_or_else(|| "Background job failed".into())));
                    }
                    _ => state.borrow_mut().result = Some(Err("Invalid worker message".into())),
                }
            })
        };
        let on_error = {
            let state = Rc::clone(&state);
            Closure::<dyn FnMut(ErrorEvent)>::new(move |event: ErrorEvent| {
                state.borrow_mut().result = Some(Err(format!(
                    "Background worker failed: {}",
                    event.message()
                )));
            })
        };
        let on_message_error = {
            let state = Rc::clone(&state);
            Closure::<dyn FnMut(Event)>::new(move |_: Event| {
                state.borrow_mut().result =
                    Some(Err("Background worker sent an unreadable message".into()));
            })
        };
        worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        worker.set_onmessageerror(Some(on_message_error.as_ref().unchecked_ref()));
        Ok(Self {
            worker,
            state,
            _on_message: on_message,
            _on_error: on_error,
            _on_message_error: on_message_error,
        })
    }

    fn poll(slot: &mut Option<Self>) -> Option<Result<JsValue, String>> {
        let result = slot.as_ref()?.state.borrow_mut().result.take()?;
        slot.take();
        Some(result)
    }

    fn stage(&self) -> Option<String> {
        self.state.borrow().stage.clone()
    }
}

impl Drop for WorkerTask {
    fn drop(&mut self) {
        self.worker.set_onmessage(None);
        self.worker.set_onerror(None);
        self.worker.set_onmessageerror(None);
        self.worker.terminate();
    }
}

fn field(object: &JsValue, name: &str) -> JsValue {
    Reflect::get(object, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

fn set(object: &js_sys::Object, name: &str, value: &JsValue) {
    Reflect::set(object, &JsValue::from_str(name), value).expect("set plain request property");
}

fn post_stage(name: &str) {
    let worker: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let message = js_sys::Object::new();
    set(&message, "kind", &JsValue::from_str("stage"));
    set(&message, "name", &JsValue::from_str(name));
    let _ = worker.post_message(&message);
}

/// Called by `compile_worker.js` after the shared WASM module initializes.
#[wasm_bindgen]
#[expect(
    unreachable_pub,
    reason = "wasm-bindgen exports require pub even from a private module"
)]
pub fn compile_in_worker(
    source: &str,
    code_distance: u32,
    prepare_t_with_mpps: bool,
) -> Result<Array, JsValue> {
    let config = CompileConfig::try_new(code_distance)
        .map_err(|error| JsValue::from_str(&error.to_string()))?
        .with_prepare_t_with_mpps(prepare_t_with_mpps);
    let context = CompileContext::new(config)
        .with_progress_observer(|stage: CompileStage| post_stage(&stage.to_string()));
    let graph =
        BlockGraph::from_text(source).map_err(|error| JsValue::from_str(&error.to_string()))?;
    let artifacts = context
        .compile(&graph)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    post_stage("Transferring compiled program");
    let result = Array::new();
    result.push(&Uint8Array::from(artifacts.bloq.to_binary().as_slice()));
    result.push(&JsValue::from_f64(artifacts.compile_duration.as_secs_f64()));
    Ok(result)
}

/// Called by the shared worker for the expensive logical ZX viewer path.
#[wasm_bindgen]
#[expect(
    unreachable_pub,
    reason = "wasm-bindgen exports require pub from a private module"
)]
pub fn simplify_zx_in_worker(source: &str, seed: u64) -> Result<String, JsValue> {
    let graph = decode_zx_worker_source(source)
        .map_err(|error| JsValue::from_str(&format!("{error:#}")))?;
    post_stage("Simplifying ZX graph");
    let graph = simplified_zx_graph(&graph, seed)
        .map_err(|error| JsValue::from_str(&format!("{error:#}")))?;
    post_stage("Preparing ZX layout");
    let view = SimplifiedZxView::new(graph);
    encode_zx_worker_view(&view).map_err(|error| JsValue::from_str(&format!("{error:#}")))
}
