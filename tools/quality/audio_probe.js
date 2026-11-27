// Installed only by the isolated benchmark browser. No microphone or tab is opened.
(() => {
  let input, source, playbackMeter, recorder, playbackTarget, onset, firstOutput, ended, timer;
  let lastOutput, audioBlob;
  let chunks = [];
  const nativeConnect = AudioNode.prototype.connect;
  Object.defineProperty(navigator.mediaDevices, "getUserMedia", {value: async constraints => {
    if (source || !constraints.audio || constraints.video) throw new Error("Unexpected capture request");
    input = new AudioContext({sampleRate: 16000});
    source = input.createMediaStreamDestination();
    await input.resume();
    return source.stream;
  }});
  Object.defineProperty(navigator.mediaDevices, "getDisplayMedia", {value: () =>
    Promise.reject(new Error("Display capture is unavailable in the benchmark"))});

  // Observe the production player's audio after its existing unmute and scheduling checks.
  // The normal destination stays connected. The second branch captures the samples for review.
  AudioNode.prototype.connect = function(destination, ...args) {
    const result = nativeConnect.call(this, destination, ...args);
    if (this instanceof AudioBufferSourceNode && destination === this.context.destination) {
      if (!playbackMeter) {
        playbackMeter = this.context.createAnalyser();
        playbackMeter.fftSize = 256;
        playbackTarget = this.context.createMediaStreamDestination();
        nativeConnect.call(playbackMeter, playbackTarget);
        recorder = new MediaRecorder(playbackTarget.stream, {mimeType: "audio/webm;codecs=opus"});
        recorder.ondataavailable = event => {if (event.data.size) chunks.push(event.data);};
        recorder.start();
        timer = setInterval(() => {
          const samples = new Float32Array(playbackMeter.fftSize);
          playbackMeter.getFloatTimeDomainData(samples);
          if (onset !== undefined && samples.some(x => Math.abs(x) > 0.01)) {
            firstOutput ??= performance.now();
            lastOutput = performance.now();
          }
        }, 8);
      }
      nativeConnect.call(this, playbackMeter);
    }
    return result;
  };
  window.qualityFixture = {
    async play(encoded) {
      if (!source || onset !== undefined) throw new Error("Fixture source is unavailable");
      const bytes = Uint8Array.from(atob(encoded), c => c.charCodeAt(0));
      const pcm = new DataView(bytes.buffer);
      const data = input.createBuffer(1, bytes.length / 2, 16000);
      const dest = data.getChannelData(0);
      for (let i = 0; i < dest.length; i++) dest[i] = pcm.getInt16(i * 2, true) / 32768;
      const fixture = input.createBufferSource();
      fixture.buffer = data;
      fixture.connect(source);
      fixture.onended = () => {ended = performance.now();};
      // Leading silence was trimmed to 1% amplitude. The next source quantum is the reference.
      onset = performance.now(); fixture.start();
    },
    tracksEnded() {
      const tracks = source?.stream.getAudioTracks();
      return tracks?.length ? tracks.every(track => track.readyState === "ended") : null;
    },
    timing() {
      return {input_started: onset !== undefined, input_ended: ended !== undefined,
        first_playback_ms: firstOutput === undefined ? null : Math.round(firstOutput - onset),
        last_output_age_ms: lastOutput === undefined ? null : Math.round(performance.now() - lastOutput)};
    },
    async finish() {
      clearInterval(timer);
      if (recorder?.state === "recording") {
        await new Promise(resolve => {recorder.onstop = resolve; recorder.stop();});
        audioBlob = new Blob(chunks, {type: "audio/webm;codecs=opus"}); chunks = [];
      }
      if (source) source.stream.getTracks().forEach(track => track.stop());
      if (input) await input.close();
      if (!audioBlob || audioBlob.size > 4 * 1024 * 1024) return null;
      const reader = new FileReader();
      return await new Promise(resolve => {
        reader.onload = () => resolve(String(reader.result).split(",")[1]);
        reader.onerror = () => resolve(null);
        reader.readAsDataURL(audioBlob);
      });
    },
  };
})();
