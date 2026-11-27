# Security

## Report a vulnerability

Sign in to GitHub and use Enny’s private reporting form. It goes to the maintainers. Reports stay off the public issue list and project board. Include the release or commit, browser, operating system and impact. Use fictional meeting content. Remove credentials, meeting links and personal information. [Report a vulnerability privately](https://github.com/LateralThinkingMachine/enny-roadmap/security/advisories/new)

## Audio, credentials and exports

- **Browser edition.** The sample works locally. On-device speech and translation can trigger browser-managed model downloads. The application’s content security policy blocks application network connections. Closing the page clears its transcript.
- **Full app.** The local service binds to loopback. The operator page and the approved-viewer page have separate origins and credentials. Keep the operator tab, one-use sign-in links and credentials file private.
- **Live audio.** Obtain participant agreement before sending the selected audio to a provider. The desktop launcher accepts the key for one session. The source launcher reads it from a private local file. Keep keys out of recordings, shell history, Git and issue reports. Check the provider account for actual usage.
- **Local exports.** Captions and notes can contain sensitive information. Review the file before sharing it with another person, a database or a coding agent. In any self-hosted integration, enforce the current source permissions for each recipient.

## Verify a release download

1. Download your file and the matching checksum list. [Preview 5 files](docs/get-started-with-enny.md#1-get-the-app) · [Preview 5 SHA256SUMS](https://github.com/LateralThinkingMachine/enny-interpreter/releases/download/v0.1.0-preview.5/SHA256SUMS)
2. Open a terminal in the folder that contains the download.
3. On macOS, run `shasum -a 256 FILENAME`. On Linux, run `sha256sum FILENAME`. Replace `FILENAME` with the name of your file.
4. Open `SHA256SUMS`. Find the line with that filename. Use the download only if the two hashes match.
