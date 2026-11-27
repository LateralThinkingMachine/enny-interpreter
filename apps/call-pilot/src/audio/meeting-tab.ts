/** The browser chooser owns the selection. No meeting URL, title or account is required. */
export function requestMeetingTab(suppressLocalPlayback = false): Promise<MediaStream> {
  if (typeof navigator.mediaDevices?.getDisplayMedia !== 'function') throw new Error('capture_unavailable');
  const duckingAudio: MediaTrackConstraints & {suppressLocalAudioPlayback: boolean} = {suppressLocalAudioPlayback: true};
  const options = {
    video: {displaySurface: 'browser'},
    audio: suppressLocalPlayback ? duckingAudio : true,
    selfBrowserSurface: 'exclude', preferCurrentTab: false, monitorTypeSurfaces: 'exclude',
    systemAudio: 'exclude', surfaceSwitching: 'exclude',
  } as const;
  return navigator.mediaDevices.getDisplayMedia(options);
}

/** The chooser options are hints. Verify what was actually granted before using a stream. */
export function requireMeetingTab(stream: MediaStream): void {
  const audio = stream.getAudioTracks();
  if (audio.length !== 1 || audio[0].readyState !== 'live') throw new Error('tab_audio_missing');
  const video = stream.getVideoTracks();
  if (video.length !== 1 || video[0].readyState !== 'live' || video[0].getSettings().displaySurface !== 'browser' || stream.getTracks().length !== 2) {
    throw new Error('select_browser_tab');
  }
}
