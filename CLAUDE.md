# CLAUDE.md: rules for changing Scana

For anyone, human or AI agent, working on this code. Read `docs/DESIGN.md` first: how everything works. If a `CLAUDE.local.md` exists beside this file, read it too: it holds a contributor's own setup and is not part of the repo.

Scana has three stages built: the firmware, seven crates (`scana-proto`, `scana-sim`, `scana-link`, `scana-core`, `scana-db`, `scana-app`, `scana-cli`) and `tools/gate.sh`. Stage 3 (every module, read-only: discovery, module codes, the data explorer) works on simulated modules; it still needs a real car.

## Hard rules

- **Bus manners** (DESIGN section 7) and the **safety model** (section 11) are not optional. A new service that changes a module, a new raw-frame path or a change to the safety levels needs agreement first.
- Never add programming, security access (0x27), data writes (0x2E) or communication control (0x28, 0x85).
- Every service goes through `scana_proto::uds::level_of`. Module discovery follows DESIGN section 7a: car still, listen first, never send on an ID heard in use, 0x7DF and 0x7E8-0x7EF never used as probes.
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

## Writing diagnosis guides

- One guide per code family in `crates/scana-db/data/diag/`, added to `FILES` in `diag.rs`. DESIGN section 15 has the format.
- Every fact needs a source in `sources` (or a make note's `source`); write "unverified" where a source is weak. Never copy SAE wording or a maker's manual text: write it in Scana's own words.
- Simplified technical English: short sentences, one instruction per step, the same word for the same thing. No em or en dashes.
- Weights are rough shares from the sources. Effects stay between -4 and +4. A test needs at least two answers; tests needing no tools come first.
- Thresholds in `auto` are estimates unless a source gives them: say so in the step text ("about"). Values differ by car: point to the workshop manual.
- Safety first: warn about hot exhausts, running engines, fuel pressure, high voltage, airbags.
- Write for people who do the work themselves. Give the do-it-yourself way first, with the tool it needs; send them to a dealer, workshop or specialist only where a licence (refrigerant), the maker's software, special equipment or real danger requires it, and say which. "Ask a dealer" is never a test on its own: say how to look it up (bulletins on nhtsa.gov, a maker's paid repair site, the calibration ID Scana reads).
- Make notes (`[[note]]`) carry a `kind`: `bulletin`, `recall`, `maker` or `owner-reports`. The app shows the label and only the car's make; owner reports must say "unverified" in their source. Keep a bulletin's facts and owners' opinions in separate notes.
- Linked words: after adding text, run `cargo run -p scana-db --example link_audit -- WORD` for any everyday word that links (ground, knock, session). Fix a wrong sense with a specific alias, or a phrase in `data/learn/no-link.toml`.

## Tools

| Tool | Command |
|---|---|
| Rust 1.99 stable, edition 2024 | `cargo` |
| arduino-cli with the AVR core 1.8.8 | `arduino-cli compile --fqbn arduino:avr:uno firmware/scana-bridge` |
| The gate | `tools/gate.sh` (or `--no-firmware`): formatting, clippy with `-D warnings`, tests, firmware host tests, firmware build with RAM under 75% |
| Hardware checks | `cargo run -p scana-cli -- probe`, then `loopback`. With no car attached, `detect` fails after 2 frames |
| Simulator | `--sim` (simulated Hijet), `--sim-dash` (adds a dash display polling), `--sim-faults` (codes, freeze frame, failing test; engine off), `--sim-corolla` (a 2006 Corolla with P0420; the app's `--demo-corolla`) |
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
- Module tests run full discoveries (about 500 probes): give the test client a 1 ms gap and an 8 ms answer window, or they take minutes.
- An OBD-II ECU refuses `3E 00` (`7F 3E 11`) on many cars: that still counts as found. Its enhanced side may list no codes while OBD-II service 03 does: `read_codes` merges them.
- Another tester on the bus (a dash display): the Hijet's engine computer slips single-frame answers to it into Scana's multi-frame replies on the same ID, and sometimes drops Scana's reply. A multi-PID reply must hold every PID asked for, the ISO-TP receiver keeps a long reply going through a single frame, and while another tester is active the poller asks only for what fits a one-frame reply.
- Each link's clock starts at 0: the recorder rebases after a reconnect, or times in the logs overlap.
- A bridge restart mid-session shows first as a refused send (BEL: its bus is closed), before the `#R` line is read. The client looks for that line when a send fails, so the reset report gets logged.
- The Uno's stock boot loader clears MCUSR, and r2 holds leftovers, not reset flags (both checked on the Uno). Firmware 0.1.1 tells watchdog resets apart with the watchdog interrupt and warm from cold with `.noinit` bytes.
- The Library (`crates/scana-db/data/learn/topics/`): topics need a summary of at most 25 words, at least two sources read on a stated date, related links that resolve, and aliases no other topic claims; the tests check all of it. Keep aliases specific: a common word as an alias ("rpm", "converter") underlines everywhere or links the wrong thing ("DC-DC converter").
- The look: workshop dark, orange accent (`theme.rs`); main buttons use `theme::accent_button` (dark text on orange). Logo and icons are made in VectorCraft from `design/`; see `design/README.md`.
- Switching cars in one app session: everything read from the last car must be cleared, the health check too (it once stayed and showed under the new car's name). The same car back after a lost link keeps its results and graphs.
- Several measurements can wait at once in the Diagnose page (`diag_wait` is a list): one slot silently dropped the first when a second started.
- Older cars: no permanent codes (service 0A), no ECU name; say so rather than show nothing. Try a new kind of car on a simulated one first (`scana-sim`): the 2006 Corolla emulation found several of these problems.
- Toyota-family facts come from openpilot's opendbc (MIT) and the toyota-diagnostics README: addresses only, no data copied. Name sources in `addresses.toml`.

## Style

- Plain English in docs and messages, short sentences, no em or en dashes.
- Error messages say what happened and what to try, in words a non-programmer understands.
- Commits: work on a branch; `main` holds work that passed the gate.
