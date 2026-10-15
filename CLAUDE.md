# CLAUDE.md: rules for changing Scana

For anyone, human or AI agent, working on this code. Read `docs/DESIGN.md` first: how everything works. If a `CLAUDE.local.md` exists beside this file, read it too: it holds a contributor's own setup and is not part of the repo.

Scana has two stages built: the firmware, seven crates (`scana-proto`, `scana-sim`, `scana-link`, `scana-core`, `scana-db`, `scana-app`, `scana-cli`) and `tools/gate.sh`. Stage 3 (every module, read-only) is next.

## Hard rules

- **Bus manners** (DESIGN section 7) and the **safety model** (section 11) are not optional. A new service that changes a module, a new raw-frame path or a change to the safety levels needs agreement first.
- Never add programming, security access (0x27), data writes (0x2E) or communication control (0x28, 0x85).
- The bridge firmware never sends a frame on its own, and always starts closed.
- `scana-proto` has no I/O, threads or clock. Time is passed in. Keep it deterministic so the simulator tests mean something.
- No `unwrap`/`expect` outside tests. Typed errors in libraries (`thiserror`), `anyhow` in binaries.
- The app never blocks and never touches the serial port: it talks to the session worker.
- Every decoder gets tests with real values and with malformed input.

## Public repo hygiene

This repo is public. Keep it about the software:

- No personal paths, names, places, contact details or notes from anyone's own files.
- No vehicle identification numbers. Recorded logs can contain a VIN (service 09): mask it before a log becomes a test or an example.
- Write neutrally: "runs on an Uno R3", not "runs on my Uno".

## Tools

| Tool | Command |
|---|---|
| Rust 1.99 stable, edition 2024 | `cargo` |
| arduino-cli with the AVR core 1.8.8 | `arduino-cli compile --fqbn arduino:avr:uno firmware/scana-bridge` |
| The gate | `tools/gate.sh` (or `--no-firmware`): formatting, clippy with `-D warnings`, tests, firmware host tests, firmware build with RAM under 75% |
| Hardware checks | `cargo run -p scana-cli -- probe`, then `loopback`. With no car attached, `detect` fails after 2 frames |
| Simulator | `--sim` (simulated Hijet), `--sim-dash` (adds a dash display polling), `--sim-faults` (codes, freeze frame, failing test; engine off) |
| Look at the app | `cargo build -p scana-app && ./target/debug/scana-app --screenshots /tmp/scana-shots`, then look at the PNGs. `--shot-bridge DIR` uses a real bridge. Do this after any UI change |

Work is done when the gate passes and the screenshots look right.

## Lessons

- Firmware timing: re-read the clock after handling a command. Opening the bus takes about 5 ms, and an unsigned "now minus last command" wrapped and fired the host watchdog at once (fixed in 0.1.0; the host tests' fake clock now moves during an open).
- The Uno's USB chip can send a noise byte when the port opens: the bridge ignores non-printable bytes, and the host sends a lone CR and drains replies before its first command.
- With nothing on the bus, a normal-mode frame fails with `TXARB` (unterminated lines read dominant), not `TXERR`. Treat both as "not acknowledged".
- The MCP2515's oscillator may not run yet when the sketch starts at power-up: look for the chip for up to 100 ms.
- Detection must log frames it hears while listening (it goes through `Client::note_frame`).
- egui 0.37 changed its API: the app implements `fn ui(&mut self, ui, frame)`, panels are `egui::Panel::left/right/top/bottom("id").show(ui, ...)`, screenshots are `ViewportCommand::Screenshot(ScreenshotCallback)`. Read the egui source before guessing.
- egui_plot 0.37 is built for egui 0.36 (two eguis in the tree, type errors). Scana draws its own charts (`scana-app/src/chart.rs`). Check `cargo tree -i egui` after adding any egui add-on.
- Inside `horizontal_wrapped` a `Frame`'s contents lay out sideways: give each tile its own `allocate_ui_with_layout(..., Layout::top_down(...))`. Long text in `ui.horizontal` doesn't wrap and widens the page: put it on its own line.
- Commands that save sessions (`health`, `clear`, `record`) write to the real sessions folder: test them with `XDG_DATA_HOME` (or `record --dir`) pointing at a temporary folder. Screenshot mode keeps its sessions inside its folder and never saves settings.
- `scana record` stops on a line from stdin; with input closed (no terminal) it runs until `--minutes` or Ctrl-C.

## Style

- Plain English in docs and messages, short sentences, no em or en dashes.
- Error messages say what happened and what to try, in words a non-programmer understands.
- Commits: work on a branch; `main` holds work that passed the gate.
