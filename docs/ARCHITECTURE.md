# Sonema architecture

## Dependency direction

```text
sonema-app ──────┬────> sonema-format ──> sonema-audio
                 └────> sonema-audio ───> sonema-dsp ──> sonema-core
                                      └───────────────> sonema-core
```

`sonema-core` contains no UI, operating-system, device, or file dependencies.
That boundary is the compatibility contract for future frontends and plugin
hosts.

## Realtime rules

- Audio device callbacks do not read files, allocate vectors, or acquire mutexes.
- Session edits are compiled on the UI thread and transferred as an owned graph
  through a bounded command channel.
- Recording and input monitoring use preallocated lock-free `ArrayQueue` buffers.
- Queue overflow drops samples and reports the exact count; it never blocks the
  device callback.
- Realtime playback and offline export both use `RealtimeSession` and the same
  `sonema-dsp` processors.

## Project compatibility

`.sonema` is a single binary container:

1. fixed magic and container version;
2. length-delimited JSON project manifest;
3. planar little-endian 32-bit float PCM media;
4. FNV-1a integrity checksum.

The model carries a separate format version and a named JSON extension map.
Unknown future data can be retained without changing realtime structures.

## Planned extension points

- `sonema-midi`: MIDI clips, devices, piano roll
- `sonema-plugin-host`: VST3/CLAP/AU process isolation
- `sonema-automation`: parameter lanes and tempo map
- `sonema-session`: take lanes, comping, media relinking
- alternate audio backend selected behind the current engine boundary
