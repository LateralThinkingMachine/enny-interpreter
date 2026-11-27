//! Offline file UI. Model installation and capture stay in the browser API adapter.
use crate::components::button::Button;
use crate::components::card::{Card, CardContent, CardHeader};
use leptos::prelude::*;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = connectPortableBrowser)]
    fn connect(state: JsValue, line: JsValue, hide: JsValue, restore: JsValue);
    #[wasm_bindgen(js_name = checkPortableLanguages)]
    fn check_languages(mode: &str, from: &str, to: &str);
    #[wasm_bindgen(js_name = preparePortableLanguages)]
    fn prepare();
    #[wasm_bindgen(js_name = startPortableTranslation)]
    fn start(text: &str, consent: bool);
    #[wasm_bindgen(js_name = stopPortableTranslation)]
    fn stop();
}

#[derive(Clone)]
struct Caption {
    id: u64,
    source: String,
    translated: String,
    from: String,
    to: String,
}

#[derive(Clone, Copy)]
struct Screen {
    mode: RwSignal<String>,
    from: RwSignal<String>,
    to: RwSignal<String>,
    phase: RwSignal<String>,
    message: RwSignal<String>,
    prepare_label: RwSignal<String>,
    text: RwSignal<String>,
    consent: RwSignal<bool>,
    sample: RwSignal<bool>,
    captions: RwSignal<Vec<Caption>>,
    next_id: RwSignal<u64>,
}

impl Screen {
    fn new() -> Self {
        Self {
            mode: RwSignal::new("text".into()),
            from: RwSignal::new("en".into()),
            to: RwSignal::new("es".into()),
            phase: RwSignal::new("checking".into()),
            message: RwSignal::new("Checking this browser…".into()),
            prepare_label: RwSignal::new("Prepare translation".into()),
            text: RwSignal::new(String::new()),
            consent: RwSignal::new(false),
            sample: RwSignal::new(false),
            captions: RwSignal::new(Vec::new()),
            next_id: RwSignal::new(0),
        }
    }

    fn clear(self) {
        self.captions.update(Vec::clear);
        self.sample.set(false);
    }

    fn check(self) {
        self.clear();
        self.consent.set(false);
        check_languages(
            &self.mode.get_untracked(),
            &self.from.get_untracked(),
            &self.to.get_untracked(),
        );
    }

    fn append(self, source: String, translated: String, from: String, to: String) {
        let id = self.next_id.get_untracked();
        self.next_id.set(id + 1);
        self.captions.update(|captions| {
            if captions.len() == 100 {
                captions.remove(0);
            }
            captions.push(Caption {
                id,
                source,
                translated,
                from,
                to,
            });
        });
    }

    fn show_sample(self) {
        self.clear();
        self.sample.set(true);
        for (source, translated) in [
            ("Can everyone hear me?", "¿Me oyen todos?"),
            ("Yes, we can begin.", "Sí, podemos empezar."),
        ] {
            self.append(source.into(), translated.into(), "en".into(), "es".into());
        }
    }

    fn connect(self) {
        let state =
            Closure::<dyn Fn(String, String, String)>::new(move |phase: String, message, label| {
                if matches!(phase.as_str(), "setup" | "checking" | "unavailable") {
                    self.consent.set(false);
                }
                self.phase.set(phase);
                self.message.set(message);
                self.prepare_label.set(label);
            });
        let line = Closure::<dyn Fn(String, String)>::new(move |source, translated| {
            if self.sample.get_untracked() {
                self.clear();
            }
            self.append(
                source,
                translated,
                self.from.get_untracked(),
                self.to.get_untracked(),
            );
        });
        let hide = Closure::<dyn Fn()>::new(move || {
            self.clear();
            self.text.set(String::new());
            self.consent.set(false);
        });
        let restore = Closure::<dyn Fn()>::new(move || self.check());
        // The browser keeps these callbacks for the lifetime of this document, including bfcache restores.
        connect(
            state.into_js_value(),
            line.into_js_value(),
            hide.into_js_value(),
            restore.into_js_value(),
        );
        self.check();
    }
}

const LANGUAGES: [(&str, &str); 5] = [
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("ja", "Japanese"),
];

#[component]
fn LanguageField(
    id: &'static str,
    label: &'static str,
    value: RwSignal<String>,
    screen: Screen,
) -> impl IntoView {
    view! {
        <div class="field">
            <label for=id>{label}</label>
            <select id=id on:change=move |event| { value.set(event_target_value(&event)); screen.check(); }>
                {LANGUAGES.into_iter().map(|(code, name)| view! {
                    <option value=code selected=code == value.get_untracked()>{name}</option>
                }).collect_view()}
            </select>
        </div>
    }
}

#[component]
fn Setup(screen: Screen) -> impl IntoView {
    let is_text = move || screen.mode.get() == "text";
    view! {
        <Card>
            <section aria-labelledby="settings">
                <CardHeader><h2 id="settings">"Session setup"</h2></CardHeader>
                <CardContent>
                    <div class="field"><label for="mode">"Input"</label>
                        <select id="mode" on:change=move |event| { screen.mode.set(event_target_value(&event)); screen.check(); }>
                            <option value="text">"Typed text"</option>
                            <option value="tab">"Meeting tab · experimental"</option>
                            <option value="mic">"Microphone · experimental"</option>
                        </select>
                    </div>
                    <div class="pair">
                        <LanguageField id="from" label="From" value=screen.from screen/>
                        <LanguageField id="to" label="To" value=screen.to screen/>
                    </div>
                    <p class="note" id="inputHelp">{move || match screen.mode.get().as_str() {
                        "tab" => "Choose the meeting tab and enable Share tab audio in the browser picker. Only the selected tab is accepted.",
                        "mic" => "Start captions asks for microphone access. Select the audio input you intend to share.",
                        _ => "Translate up to 4,000 characters at a time using a local language model.",
                    }}</p>
                    <p class="status" id="status" role="status">{move || screen.message.get()}</p>
                    <div class="field" id="typed" hidden=move || !is_text()>
                        <label for="message">"Text to translate"</label>
                        <textarea id="message" rows="3" maxlength="4000" autocomplete="off" spellcheck="false"
                            prop:value=move || screen.text.get() on:input=move |event| screen.text.set(event_target_value(&event))/>
                    </div>
                    <p class="note">"Prepare each language model when prompted. The browser downloads any files it needs. It may take a few minutes. A download you approve may finish after you stop."</p>
                    <div class="field" id="consent" hidden=is_text><label>
                        <input type="checkbox" id="agreement" prop:checked=move || screen.consent.get()
                            disabled=move || matches!(screen.phase.get().as_str(), "selecting" | "live" | "finishing")
                            on:change=move |event| screen.consent.set(event_target_checked(&event))/>
                        " Everyone whose audio will be shared has agreed to live interpretation."
                    </label></div>
                    <div class="actions">
                        <Button attr:id="prepare" attr:disabled=move || screen.phase.get() != "setup" on:click=move |_| prepare()>
                            {move || screen.prepare_label.get()}
                        </Button>
                        <Button class="primary".to_owned() attr:id="start"
                            attr:disabled=move || screen.phase.get() != "ready" || (!is_text() && !screen.consent.get())
                            on:click=move |_| {
                                if screen.sample.get_untracked() { screen.clear(); }
                                start(&screen.text.get_untracked(), screen.consent.get_untracked());
                            }>
                            {move || if is_text() { "Translate text" } else { "Start captions" }}
                        </Button>
                        <Button attr:id="stop" attr:disabled=move || !matches!(screen.phase.get().as_str(), "preparing" | "ready" | "typing" | "selecting" | "live" | "finishing") on:click=move |_| stop()>"Stop"</Button>
                    </div>
                    <details><summary>"Privacy and browser support"</summary>
                        <p>"Get everyone’s consent before capturing audio. The app uses browser-managed local models. It sends no meeting audio or text to a service and stores no transcript. Stop ends capture. Clear transcript removes displayed captions."</p>
                        <p>"Live captions are experimental. A browser can detect speech without returning captions. Try desktop Google Chrome 139 or later. Your device, languages and browser settings must support on-device speech recognition and translation. If the downloaded file cannot use these features, serve it on localhost and check again. The sample works offline without models."</p>
                    </details>
                </CardContent>
            </section>
        </Card>
    }
}

#[component]
fn Transcript(screen: Screen) -> impl IntoView {
    let empty = move || screen.captions.with(Vec::is_empty);
    view! {
        <Card>
            <section class="output" aria-labelledby="output">
                <CardHeader><div class="sectionhead">
                    <h2 id="output">"Transcript"</h2>
                    <Button attr:id="sample" attr:disabled=move || matches!(screen.phase.get().as_str(), "preparing" | "typing" | "selecting" | "live" | "finishing") on:click=move |_| screen.show_sample()>"Try a sample"</Button>
                </div></CardHeader>
                <CardContent>
                    <p class="note" id="kind">{move || if screen.sample.get() { "Prewritten sample · English → Spanish" } else { "Source text appears above each translation." }}</p>
                    <p id="empty" hidden=move || !empty()>"Your translated lines will appear here."</p>
                    <ol id="entries" aria-live="polite" aria-relevant="additions">
                        <For each=move || screen.captions.get() key=|line| line.id children=move |line| view! {
                            <li><p class="original" lang=line.from>{line.source}</p>
                                <p class="translation" lang=line.to>{line.translated}</p></li>
                        }/>
                    </ol>
                    <div class="actions"><Button attr:id="clear" attr:disabled=empty on:click=move |_| screen.clear()>"Clear transcript"</Button></div>
                </CardContent>
            </section>
        </Card>
    }
}

/// A self-contained reading example. Its only state is which recorded time
/// the reader selected. It has no access to the interpreter or browser ports.
#[component]
fn ShipmentHistory() -> impl IntoView {
    let (corrected, select_correction) = signal(false);
    view! {
        <section class="history-example" aria-labelledby="history-summary">
            <details id="shipment-history">
                <summary id="history-summary">"Explore a fictional shipment correction"</summary>
                <p>"A warehouse team schedules a dock slot using a shipment report. Which report did the decision note cite, and what did the records say after the correction?"</p>
                <fieldset>
                    <legend>"Show the meeting record as of"</legend>
                    <label><input type="radio" name="history-cutoff" id="history-decision"
                        prop:checked=move || !corrected.get() on:change=move |_| select_correction.set(false)/>
                        "09:05 · Decision recorded"</label>
                    <label><input type="radio" name="history-cutoff" id="history-correction"
                        prop:checked=move || corrected.get() on:change=move |_| select_correction.set(true)/>
                        "09:10 · Correction recorded"</label>
                </fieldset>
                <div class="history-record" aria-live="polite" aria-atomic="true">
                    <h3>{move || if corrected.get() {"Shipment report at 09:10"} else {"Shipment report at 09:05"}}</h3>
                    <p id="history-report">{move || if corrected.get() {"Thursday · 50 boxes"} else {"Friday · 15 boxes"}}</p>
                    <p id="history-source">{move || if corrected.get() {
                        "The 09:10 correction updates the delivery day and quantity for the same shipment."
                    } else {
                        "The 09:00 shipment report gives the expected delivery day and quantity."
                    }}</p>
                </div>
                <h3>"What the decision cited"</h3>
                <p id="history-basis">"The 09:05 decision note scheduled a dock slot using the 09:00 shipment report (Friday · 15 boxes)."</p>
                <p>"A temporal graph can link the decision to the report version it cited and the correction to the same shipment. The notes establish what was recorded. What each participant heard is unknown."</p>
                <p class="note">"In the full app, download your source captions and notes as a text file before you stop."</p>
            </details>
        </section>
    }
}

#[component]
fn PortableInterpreter() -> impl IntoView {
    let screen = Screen::new();
    screen.connect();
    view! {
        <main data-ui="rust-ui-wasm">
            <header><h1>"Enny · Meeting interpreter"</h1><p>"Translate typed text on your device. You can also try experimental live captions from a meeting tab or microphone in a supported browser."</p></header>
            <div class="panels"><Setup screen/><Transcript screen/></div>
            <ShipmentHistory/>
        </main>
    }
}

#[wasm_bindgen]
pub fn mount_portable() -> Result<(), JsValue> {
    let root = document()
        .get_element_by_id("portable-root")
        .ok_or_else(|| JsValue::from_str("Missing portable root"))?;
    leptos::mount::mount_to(root.dyn_into()?, PortableInterpreter).forget();
    Ok(())
}
