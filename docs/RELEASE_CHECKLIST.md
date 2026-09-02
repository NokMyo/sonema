# Sonema v0.1 release checklist

## Automated

- `cargo fmt --all -- --check`
- `cargo clippy --locked --workspace --all-targets -- -D warnings`
- `cargo test --locked --workspace`
- Windows release build and portable ZIP artifact
- macOS, Windows, and Ubuntu workspace tests

## Physical audio-device pass

- Windows 11 WASAPI default output at 44.1/48 kHz
- Audient iD14 MKII mono microphone input and stereo loopback input
- Five-minute uninterrupted recording with zero reported dropped samples
- Input monitoring at matching rates; explicit refusal at mismatched rates
- Split, trim, move, duplicate, undo, save, reopen, and identical clip boundaries
- Korean, Japanese, spaces, and long paths for project and WAV files
- Forced exit followed by recovery of the last 30-second checkpoint
- 20 tracks / 10-minute session realtime playback on the minimum target PC
- 16-bit, 24-bit, and float WAV reopened in a second editor
- Realtime and offline-render null comparison within DSP tolerance

## v0.1 scope

- Audio tracks only; no MIDI or instrument tracks
- OS default output; selectable input
- WASAPI/CoreAudio/ALSA through CPAL; no bundled ASIO driver
- No VST3/AU/CLAP host yet
- Offline export holds the stereo mix in memory to support normalization
