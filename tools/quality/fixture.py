"""Synthetic English correction with a Japanese target and a bounded lexical screen."""
import array
import hashlib
from pathlib import Path
import re
import shutil
import subprocess
import sys
import unicodedata
import wave

ID = "en-ja-shipment-correction-v2"
TURNS = (
    ("Daniel", "Enny, the shipment leaves Friday with fifteen boxes."),
    ("Samantha", "Sorry to interrupt. Correction: the shipment leaves Thursday with fifty boxes."),
)
RATE = 16000


def fields_last_in_order(text, earlier, corrected):
    before = list(re.finditer(earlier, text, re.IGNORECASE))
    after = list(re.finditer(corrected, text, re.IGNORECASE))
    return bool(before and after and before[-1].end() <= after[-1].start())


def screen(observed):
    text = unicodedata.normalize("NFKC", observed)
    # The fixture addresses Enny directly. Accept a name before punctuation or a Japanese honorific/particle.
    name = r"(?<![\wー])(?:Enny|エニー?|えにー?)(?=$|[^\wー]|さん|様|は|が|の|に|へ|を)"
    count = r"(?<![0-9〇一二三四五六七八九十百千])"
    boxes = r"\s*(?:個|箱|ケース|ボックス)"
    negation = r"\s*(?:では(?:なく|ない|ありません)|じゃ(?:なく|ない)|で(?:は)?なく|ではなかった)"
    fifty = count + r"(?:五十|50)" + boxes
    fifteen = count + r"(?:十五|15)" + boxes
    return {
        "name_seen": bool(re.search(name, text, re.IGNORECASE)),
        "friday_then_thursday": fields_last_in_order(text, "金曜", "木曜"),
        "fifteen_then_fifty_boxes": fields_last_in_order(text, fifteen, fifty),
        "no_explicit_negation_of_correction_seen": not bool(re.search(
            r"(?:木曜(?:日)?|" + fifty + ")" + negation, text)),
    }


def synthesize(directory):
    """Generate both voices locally. Refuse unavailable/non-audio TTS before key loading."""
    turns = []
    directory = Path(directory)
    for index, (voice, words) in enumerate(TURNS):
        wav = directory / f"fixture-{index}.wav"
        if sys.platform == "darwin":
            aiff = directory / f"fixture-{index}.aiff"
            subprocess.run(["/usr/bin/say", "-v", voice, "-r", "155", "-o", aiff, words], check=True)
            subprocess.run(["/usr/bin/afconvert", "-f", "WAVE", "-d", f"LEI16@{RATE}", "-c", "1", aiff, wav], check=True)
        elif shutil.which("espeak-ng") and shutil.which("sox"):
            original = directory / f"fixture-{index}-original.wav"
            subprocess.run(["espeak-ng", "-v", ("en-gb", "en-us")[index], "-s", "155", "-w", original, words], check=True)
            subprocess.run(["sox", original, "-r", str(RATE), "-c", "1", "-b", "16", wav], check=True)
        else:
            raise ValueError("Install local espeak-ng and sox for Linux synthesis.")
        with wave.open(str(wav)) as stream:
            if (stream.getframerate(), stream.getnchannels(), stream.getsampwidth()) != (RATE, 1, 2):
                raise ValueError("Unexpected local speech format.")
            samples = array.array("h", stream.readframes(RATE * 12 + 1))
        if sys.byteorder != "little":
            samples.byteswap()
        active = [i for i, sample in enumerate(samples) if abs(sample) > 328]
        if not active or len(samples) > RATE * 12:
            raise ValueError("Local synthesis returned silent or oversized audio.")
        samples = samples[active[0]:active[-1] + 1]
        if sys.byteorder != "little":
            samples.byteswap()
        turns.append(samples.tobytes())
    pcm = (bytes(RATE // 4 * 2)).join(turns)
    return pcm, hashlib.sha256(pcm).hexdigest()
