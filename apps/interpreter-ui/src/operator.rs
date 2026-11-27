use js_sys::Function;
use leptos::{mount::mount_to, prelude::*};
use serde_json::Value;
use wasm_bindgen::prelude::*;
use web_sys::HtmlElement;

#[path = "operator_fields.rs"]
mod fields;
#[path = "operator_meeting.rs"]
mod meeting;
#[path = "operator_reader.rs"]
mod reader;
#[path = "operator_records.rs"]
mod records;
#[path = "operator_speech.rs"]
mod speech_panel;

type State = ReadSignal<Value>;

// Browser callbacks run on the same thread as the Leptos owner. Dispatch stays
// synchronous for gestures such as tab selection, playback and downloads.
#[derive(Clone, Copy)]
struct Actions(StoredValue<Function, LocalStorage>);

impl Actions {
    fn emit(self, action: &str, value: &str) {
        self.0.with_value(|f| {
            if let Err(error) = f.call2(&JsValue::NULL, &action.into(), &value.into()) {
                wasm_bindgen::throw_val(error);
            }
        });
    }
}

fn flag(state: State, key: &str) -> bool {
    state.with(|s| s[key].as_bool().unwrap_or(false))
}

fn text(state: State, key: &str) -> String {
    state.with(|s| s[key].as_str().unwrap_or_default().to_owned())
}

#[wasm_bindgen]
pub struct OperatorUi {
    set: WriteSignal<Value>,
    unmount: Option<Box<dyn FnOnce()>>,
}

#[wasm_bindgen]
impl OperatorUi {
    pub fn update(&self, json: &str) -> Result<(), JsValue> {
        let data = parse_state(json)?;
        if self.unmount.is_some() {
            self.set.set(data);
        }
        Ok(())
    }

    pub fn dispose(&mut self) {
        if let Some(unmount) = self.unmount.take() {
            unmount();
        }
    }
}

fn parse_state(json: &str) -> Result<Value, JsValue> {
    serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid interpreter view state"))
}

#[wasm_bindgen]
pub fn mount_operator_ui(
    element: HtmlElement,
    kind: String,
    state_json: &str,
    dispatch: Function,
) -> Result<OperatorUi, JsValue> {
    let render = match kind.as_str() {
        "button" => meeting::button,
        "select" => fields::select,
        "input" => fields::input,
        "textarea" => fields::textarea,
        "setup" => meeting::setup,
        "transport" => meeting::transport,
        "audio-check" => meeting::audio_check,
        "sound" => meeting::sound,
        "session" => records::session,
        "stop-receipt" => records::stop_receipt,
        "notes" => records::notes,
        "fragments" => records::fragments,
        "reader" => reader::reader,
        "viewer" => records::viewer,
        "speech-hints" => speech_panel::speech_settings,
        _ => return Err(JsValue::from_str("Unknown interpreter view")),
    };
    let initial = parse_state(state_json)?;
    let owner = Owner::new();
    let (state, set) = owner.with(|| signal(initial));
    let handle = owner.with(|| mount_to(element, move || render(state, dispatch)));
    Ok(OperatorUi {
        set,
        unmount: Some(Box::new(move || {
            drop(handle);
            drop(owner);
        })),
    })
}
