# Platform support and interpretation quality

## Provider and platform support

| Edition | Audio and provider | What to expect |
| --- | --- | --- |
| Full app on macOS | Apple Silicon, macOS 14 or later and desktop Chrome. OpenAI provides live captions and interpretation with your key. | The preview app has an ad-hoc signature. Gatekeeper may block it. For a local sound check, select **Start free rehearsal**. |
| Full app on Linux | x86-64 or ARM64, glibc 2.36 or later, system CA certificates, Python 3.10 or later, and a desktop browser offering tab audio. | The launcher offers rehearsal and OpenAI. Desktop audio capture on Linux still needs testing in real meetings. Use a Linux browser in the same distribution for WSL. |
| Gemini source preview | Use your own Google key and one spoken target per session, English, Japanese or Hindi. | The adapter has local protocol tests. Live Google availability, audible quality and charges still need independent verification. [Source activation](../README.md#gemini-fixed-target-source-preview) |
| Single HTML file | Open in desktop Chrome. The offline sample needs no key. Typed translation needs a browser language pack. | Browser-native live captions are experimental. Availability depends on browser, profile and languages. Use the full app for spoken interpretation. |

For a browser meeting, select the **meeting tab** and enable **Share tab audio**. The full app also offers a separate microphone input. Window capture and whole-screen capture are refused. Product-specific end-to-end capture, mobile browsers and other desktop browsers need separate verification.

## Interpretation quality benchmark

The synthetic English fixture addresses Enny. One voice says a shipment leaves Friday with fifteen boxes. A second voice corrects it to Thursday with fifty boxes. A quarter-second pause separates the turns. The expected result includes the name, the earlier plan, the correction and the final plan. [Fixture and expected terms](../tools/quality/fixture.py)

Listen to the playback as well as reading the captions. Ask the speaker to confirm important names, dates and quantities.

## Run a rehearsal

Build the native service and web client using the project README. Use Python with Playwright and desktop Chrome. macOS uses the installed Daniel and Samantha voices. Linux requires `espeak-ng` and `sox`. The rehearsal uses an isolated Chrome profile, synthetic microphone input and the normal operator page. It exercises the source picker, input meter and Stop with providers disabled.

```sh
python3 tools/test_interpretation_benchmark.py
python3 tools/test_interpretation_benchmark.py --chrome
run_dir=$(mktemp -d) || exit 1
printf 'Writing results to %s\n' "$run_dir"
python3 tools/benchmark_interpretation.py --receipt "$run_dir/rehearsal.json"
```

## Measure a live route

This command approves one synthetic session with OpenAI. Both source captions and live interpretation must become ready before the browser sends speech. The session lasts at most two minutes, admits up to two routes, and applies a US$10.00 local estimate limit. Each live invocation starts a separate allowance. The provider controls the actual bill.

```sh
run_dir=$(mktemp -d) || exit 1
printf 'Writing results to %s\n' "$run_dir"
python3 tools/benchmark_interpretation.py --live --key-file /absolute/path/to/private.env \
  --receipt "$run_dir/live-quality.json" --save-playback "$run_dir/interpreted.webm"
```

Use an owner-only regular key file (mode 0600) containing one `OPENAI_API_KEY` line. The benchmark uses the existing desktop configuration pipe. It never places the key in browser state, receipt or command arguments. Choose unused output paths. The runner reserves the receipt before loading a key and never automatically retries admission. The optional private WebM contains the provider's generated interpretation for this fictional fixture. A file with zero bytes has no usable recording.

## Read the receipt

- The receipt includes the executable and asset hashes, browser version, synthetic PCM hash and requested languages. Before Start, the benchmark matches the authenticated operator session and finite live-route quote against the requested provider, model and budget. It reads only interpretation output for the synthetic microphone and fixed Japanese target. Compare the same fixture and settings before drawing a latency conclusion. Record the operating system and build commit alongside the receipt.
- `first_playback_ms` runs from scheduling the trimmed first synthetic source sample to detecting a non-silent block at the browser's production playback graph. Chrome measures the graph on its monotonic clock. The analyser observes 256-sample windows on an 8 ms polling interval. This measures browser rendering, with scheduling and poll uncertainty. It does not measure acoustic arrival at a listener's ear. Absent energy produces a null value.
- The app exposes streaming target deltas. The runner reads the visible target only after the last source turn ends and a 14-second observation period has elapsed. It records this as the last visible target text. Provider-final transcript and invoice confirmation are unavailable on this interface.
- A lexical match requires the name, both earlier and corrected Japanese date/count expressions in order, and no recognized explicit negation of the correction. A mismatch is inconclusive because valid paraphrases can fall outside these patterns. Even a match requires an independent bilingual listener to check the generated playback for who, when, how many, and whether the second turn actually corrected the first. Record the listener's verdict separately. A text-only, silent or unsettled run cannot qualify audible interpretation.
- `synthetic_capture_tracks_ended` records the browser track state before the fixture performs cleanup. `server_stop_acknowledged` requires a Stop response for the expected session generation. The separate authenticated receipt supplies `provider_close_observation` and `stop_reserved_microusd`. The benchmark rechecks the read-only operator receipt for up to twelve seconds after Stop. A missing or uncertain transport receipt leaves a live benchmark incomplete. `native_shutdown` describes the process exit. Actual charges remain available only from the provider account.

Successful key-free checks establish mechanics only. Measure several fresh, separately authorized runs before publishing latency distributions or a language quality claim.

For the Google source preview, pass `--provider gemini` with a separate private key file containing `GEMINI_API_KEY`. The same two-minute synthetic fixture uses a US$0.45 local estimate limit. Each provider gets its own result and playback files. Compare complete receipts and bilingual listening judgments. No Google result has been inferred from an OpenAI or key-free run.
