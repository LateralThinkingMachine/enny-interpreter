"""Exercise the downloaded HTML in desktop Chrome using Python Playwright."""
import argparse
from pathlib import Path
import re

from playwright.sync_api import expect, sync_playwright


def check_download(chromium, html, *, with_models):
    browser = chromium.launch(
        channel="chrome",
        headless=True,
        ignore_default_args=[
            "--disable-component-update",
            "--disable-background-networking",
            "--disable-component-extensions-with-background-pages",
        ] if with_models else [],
    )
    try:
        page = browser.new_page()
        errors = []
        requests = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.on("request", lambda request: requests.append(request.url))
        page.goto(html.as_uri())
        expect(page).to_have_title('Enny · Meeting interpreter')
        expect(page.get_by_role('heading', level=1)).to_have_text('Enny · Meeting interpreter')
        expect(page.locator('main[data-ui="rust-ui-wasm"]')).to_be_visible()
        expect(page.locator('[data-name="Card"]')).to_have_count(2)
        expect(page.locator("#to")).to_have_value("es")
        page.get_by_role("button", name="Try a sample").click()
        expect(page.locator("#sample-caption-pairs")).to_be_visible()
        expect(page.locator("#sample-caption-pairs li")).to_have_count(2)
        expect(page.locator("#entries li")).to_have_count(0)
        expect(page.get_by_role("button", name="Clear transcript")).to_be_disabled()
        page.get_by_role("button", name="Close sample").click()
        expect(page.locator("#sample-caption-pairs")).to_be_hidden()
        if with_models:
            page.locator("#mode").select_option("text")
            page.get_by_role("button", name="Prepare translation").click()
            expect(page.locator("#status")).to_have_text("Ready to translate typed text.", timeout=180_000)
            phrase = "We can meet at the station on Thursday."
            page.locator("#message").fill(phrase)
            page.get_by_role("button", name="Translate text").click()
            translated = page.locator("#entries li .translation")
            expect(translated).to_be_visible(timeout=20_000)
            assert translated.inner_text().strip() and translated.inner_text() != phrase
            print("English → Spanish:", translated.inner_text())
        assert not errors, errors
        assert not [url for url in requests if url.startswith(("http:", "https:"))]
        print(f"Chrome {browser.version}: file-open sample and dismissal passed.")
    finally:
        browser.close()


def check_missing_track_support(chromium, html):
    browser = chromium.launch(channel="chrome", headless=True, args=["--disable-blink-features=MediaStreamTrackWebSpeech"])
    try:
        page = browser.new_page()
        page.goto(html.as_uri())
        page.locator("#mode").select_option("tab")
        expect(page.locator("#status")).to_contain_text("unavailable")
        expect(page.locator("#start")).to_be_disabled()
        page.get_by_role("button", name="Try a sample").click()
        expect(page.locator("#sample-caption-pairs")).to_be_visible()
        expect(page.locator("#entries li")).to_have_count(0)
    finally:
        browser.close()


def exercise_shipment_history(page, private_text=None):
    history = page.locator("#shipment-history")
    summary = history.locator("summary")
    expect(summary).to_have_text("Explore a fictional shipment correction")
    expect(history.locator("fieldset")).to_be_hidden()
    before = page.evaluate("window.modelCalls.slice()")
    summary.focus()
    page.keyboard.press("Enter")
    choices = history.get_by_role("group", name="Show the meeting record as of")
    decision = choices.get_by_role("radio", name=re.compile("09:05"))
    correction = choices.get_by_role("radio", name=re.compile("09:10"))
    report, source, basis = (history.locator("#history-" + key) for key in ("report", "source", "basis"))
    expect(decision).to_be_checked()
    expect(report).to_have_text("Friday · 15 boxes")
    expect(source).to_contain_text("09:00 shipment report")
    expect(basis).to_contain_text(re.compile(r"09:05.*09:00.*Friday · 15 boxes"))
    original_basis = basis.inner_text()
    if private_text:
        expect(history).not_to_contain_text(private_text)

    page.keyboard.press("Tab")
    expect(decision).to_be_focused()
    page.keyboard.press("ArrowRight")
    expect(correction).to_be_focused()
    expect(correction).to_be_checked()
    expect(report).to_have_text("Thursday · 50 boxes")
    expect(source).to_contain_text("09:10 correction")
    expect(basis).to_have_text(original_basis)
    if private_text:
        expect(history).not_to_contain_text(private_text)
    page.keyboard.press("ArrowLeft")
    expect(decision).to_be_focused()
    expect(decision).to_be_checked()
    expect(report).to_have_text("Friday · 15 boxes")
    expect(source).to_contain_text("09:00 shipment report")
    expect(basis).to_have_text(original_basis)
    summary.focus()
    page.keyboard.press("Space")
    expect(choices).to_be_hidden()
    assert page.evaluate("window.modelCalls.slice()") == before
    assert page.evaluate("async () => [localStorage.length, sessionStorage.length, (await indexedDB.databases()).length]") == [0, 0, 0]


def exercise_sample_with_captions(page, first):
    entries = page.locator("#entries li .original")
    sample = page.locator("#sample-caption-pairs")
    button = page.locator("#sample")
    clear = page.get_by_role("button", name="Clear transcript")
    translate = page.get_by_role("button", name="Translate text")
    text_input = page.get_by_label("Text to translate")

    # Reach the transcript limit through the real text input and browser adapter.
    for number in range(1, 100):
        text_input.fill(f"line {number}")
        translate.click()
    expected = [first] + [f"line {number}" for number in range(1, 100)]
    expect(entries).to_have_text(expected)
    before = page.evaluate("window.modelCalls.slice()")
    for _ in range(2):
        button.click()
        expect(button).to_have_attribute("aria-expanded", "true")
        expect(sample).to_be_visible()
        expect(sample.locator(".translation")).to_have_text(["¿Me oyen todos?", "Sí, podemos empezar."])
        expect(entries.first).to_be_visible()
        expect(entries).to_have_text(expected)
        expect(text_input).to_have_value("line 99")
        button.click()
        expect(button).to_have_attribute("aria-expanded", "false")
        expect(sample).to_be_hidden()
    assert page.evaluate("window.modelCalls.slice()") == before

    # The next genuine row evicts precisely the oldest genuine row.
    button.click()
    text_input.fill("line 100")
    translate.click()
    expect(sample).to_be_hidden()
    expect(entries).to_have_text(expected[1:] + ["line 100"])
    button.click()
    clear.click()
    expect(entries).to_have_count(0)
    expect(sample).to_be_visible()
    expect(clear).to_be_disabled()

    text_input.fill(first)
    page.evaluate("window.holdTranslation = true")
    translate.click()
    expect(sample).to_be_hidden()
    button.click()
    page.wait_for_function("typeof window.releaseTranslation === 'function'")
    page.evaluate("window.releaseTranslation()")
    expect(entries).to_have_text([first])
    expect(sample).to_be_visible()
    page.evaluate('window.dispatchEvent(new PageTransitionEvent("pagehide", {persisted: true}))')
    expect(entries).to_have_count(0)
    expect(text_input).to_have_value("")
    expect(sample).to_be_hidden()
    page.evaluate('window.dispatchEvent(new PageTransitionEvent("pageshow", {persisted: true}))')
    expect(entries).to_have_count(0)
    expect(sample).to_be_hidden()


def check_offline_runtime(chromium, html):
    """Exercise the downloaded WASM UI with a deterministic, local browser-model double."""
    browser = chromium.launch(channel="chrome", headless=True)
    try:
        context = browser.new_context(offline=True)
        page = context.new_page()
        page.add_init_script("""window.modelCalls = [];
        Object.defineProperty(window, "Translator", {value: {
          availability: async () => { window.modelCalls.push("availability"); return "available"; },
          create: async () => {
            window.modelCalls.push("create");
            return {translate: async text => {
              window.modelCalls.push("translate");
              if (window.holdTranslation) await new Promise(resolve => { window.releaseTranslation = resolve; });
              return "local · " + text;
            }, destroy() {}};
          }
        }});
        for (const name of ["getUserMedia", "getDisplayMedia"]) {
          navigator.mediaDevices[name] = async () => { window.modelCalls.push(name); throw Error("Unexpected capture"); };
        }""")
        errors, traffic, transfers = [], [], []
        page.on("pageerror", lambda error: errors.append(str(error)))
        context.on("request", lambda request: traffic.append(request.url))
        page.on("websocket", lambda socket: traffic.append(socket.url))
        for event in ("download", "popup", "filechooser"):
            page.on(event, lambda _, kind=event: transfers.append(kind))
        page.goto(html.as_uri())
        exercise_shipment_history(page)
        expect(page.locator("#entries li")).to_have_count(0)
        page.get_by_role("button", name="Try a sample").click()
        page.get_by_role("button", name="Prepare translation").click()
        value = "<b>Fictional fixture</b>"
        page.get_by_label("Text to translate").fill(value)
        page.get_by_role("button", name="Translate text").click()
        expect(page.locator("#entries li")).to_have_count(1)
        expect(page.locator("#entries .translation")).to_have_text("local · " + value)
        expect(page.locator("#entries .translation")).to_have_attribute("lang", "es")
        expect(page.locator("#entries b")).to_have_count(0)
        exercise_shipment_history(page, value)
        expect(page.locator("#entries li")).to_have_count(1)
        expect(page.locator("#entries .translation")).to_have_text("local · " + value)
        expect(page.locator("#message")).to_have_value(value)
        exercise_sample_with_captions(page, value)
        page.get_by_role("button", name="Prepare translation").click()
        page.get_by_role("button", name="Stop", exact=True).click()
        expect(page.locator("#start")).to_be_disabled()
        page.locator("#to").select_option("en")
        expect(page.locator("#status")).to_have_text("Choose two different languages.")
        expect(page.locator("#entries li")).to_have_count(0)
        page.evaluate('window.dispatchEvent(new PageTransitionEvent("pagehide", {persisted: true}))')
        expect(page.locator("#message")).to_have_value("")
        assert not errors, errors
        assert not transfers, transfers
        assert not [url for url in traffic if url.startswith(("http:", "https:", "ws:", "wss:"))], traffic

        blocked = context.new_page()
        blocked.add_init_script('Object.defineProperty(window, "WebAssembly", {value: undefined})')
        blocked.goto(html.as_uri())
        expect(blocked.locator("#load-status")).to_contain_text("Enable WebAssembly")
        expect(blocked.get_by_role("button", name="Start captions")).to_have_count(0)
        print("Offline WASM history, keyboard navigation, caption preservation at capacity, clearing, page restore and runtime fallback passed.")
    finally:
        browser.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("html", type=Path, nargs="?", default=Path(__file__).with_name("meeting-interpreter.html"))
    parser.add_argument("--with-models", action="store_true", help="Download browser-managed English→Spanish models into the disposable Chrome profile and translate a sentence.")
    args = parser.parse_args()
    with sync_playwright() as playwright:
        check_download(playwright.chromium, args.html.resolve(), with_models=args.with_models)
        check_missing_track_support(playwright.chromium, args.html.resolve())
        check_offline_runtime(playwright.chromium, args.html.resolve())
