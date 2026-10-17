# Scana design

How Scana works, as built in version 0.3.3 (stages 1, 2 and 3, the diagnosis guides and the Library; bridge firmware 0.1.2). Parts marked (draft) are still plans.

| Section | |
|---|---|
| 1 | Goals and limits |
| 2 | System overview |
| 3 | Bridge firmware |
| 4 | Serial protocol |
| 5 | Laptop app: crates and threads |
| 6 | Protocol stack |
| 7 | Connecting to a car |
| 7a | Finding and reading modules |
| 7b | The second bus (pins 3 and 11) |
| 8 | Polling |
| 9 | Data: definitions and vehicle profiles |
| 10 | Sessions, logs and reports |
| 11 | Safety model |
| 12 | Reliability rules |
| 13 | Testing |
| 14 | Building and installing |
| 15 | Diagnosis guides |

## 1. Goals and limits

- A scan tool that matches a closed handheld tool on generic OBD-II, then goes further: every module, logging, graphs, and plain-English help on every screen.
- First car: 2024 Daihatsu Hijet Jumbo (S510P, KF engine). OBD-II on CAN: ISO 15765-4, 11-bit IDs, 500 kbit/s, JOBD. The engine computer answers requests on 0x7DF from 0x7E8. It reports 30 mode 01 PIDs (a reference scan with a handheld scan tool, 2026-10-10).
- Then any car with OBD-II on CAN, 11-bit or 29-bit, 250 or 500 kbit/s.
- Out of reach of this hardware: K-line (ISO 9141-2, ISO 14230), J1850 PWM and VPW, CAN FD. The shield only has a CAN transceiver.
- Runs on a Linux laptop. Laid out for a 1366 x 768 screen and an older laptop (an 8th-generation Intel Core i5 with integrated graphics), and scales up.

## 2. System overview

```
 LAPTOP (Linux)                         USB              ARDUINO UNO R3 + SPARKFUN CAN-BUS SHIELD         CAR
 ┌──────────────────────────────┐   serial 1 Mbaud     ┌──────────────────────────────────────┐   DB9 + cable  ┌───────────────────┐
 │ scana-app   egui screens     │   SLCAN text lines   │ ATmega16U2   USB to serial            │   CAN-H  3-6   │ OBD port          │
 │ scana-core  sessions, polls, │◄────────────────────►│ ATmega328P   scana-bridge firmware    │   CAN-L  5-14  │  (directly, or    │
 │             logs, safety     │                      │ MCP2515      CAN controller, SPI,     │◄──────────────►│   through an OBD  │
 │ scana-proto ISO-TP, OBD, UDS │                      │              16 MHz crystal           │   GND    2-4   │   splitter)       │
 │ scana-link  serial | sim |   │                      │ MCP2551      CAN transceiver          │   +12 V  9-16  │ Engine ECU 0x7E8  │
 │             replay           │                      └──────────────────────────────────────┘                │ other modules     │
 └──────────────────────────────┘                                                                              └───────────────────┘
                                                                                     (pins: DB9 - OBD)
```

The bridge is deliberately simple. The ATmega328P has 2 KB of RAM: enough to buffer frames, not enough for diagnostics. All protocol logic runs on the laptop, where it can be tested, logged and updated without reflashing.

## 3. Bridge firmware

Folder `firmware/scana-bridge/`, version 0.1.2. Arduino sketch, built with arduino-cli for `arduino:avr:uno`: 11,164 bytes of flash (34%), 1,165 bytes of RAM (56%).

| File | Holds |
|---|---|
| `scana-bridge.ino` | Pins, LEDs, the Uno glue, the AVR watchdog |
| `bridge.h` | All the logic: commands, modes, queues, host watchdog, events. Plain C++, templated on the hardware layer |
| `slcan.h`, `bittiming.h`, `frame.h` | Parsing and formatting, bit timing. Plain C++ |
| `mcp2515.h`, `mcp2515.cpp` | The MCP2515 driver (own code) |
| `test/test_bridge.cpp` | Host tests: 190 checks with a fake hardware layer, built with g++ |

Hardware it drives (SparkFun CAN-BUS Shield DEV-13262, checked against SparkFun's hookup guide, its schematic and a real board):

| Signal | Uno pin | Note |
|---|---|---|
| MCP2515 chip select | D10 | SPI on D11 (MOSI), D12 (MISO), D13 (SCK) |
| MCP2515 interrupt | D2 | Active low. Use it: polling SPI wastes time |
| microSD chip select | D9 | Keep D9 high so the card stays off the bus |
| LEDs | D7 (CAN traffic), D8 (GPS in the demo) | Scana: D7 blinks on frames, D8 shows the bus state |
| Joystick | A1 up, A2 left, A3 down, A4 click, A5 right | Unused |
| GPS socket | D4, D5 | Unused |
| LCD header | D6 | Unused |
| Free | A0, D3 | A0 is kept for an optional battery-voltage divider |

MCP2515 crystal: 16 MHz (schematic Q1, and marked "16.00" on the board). Bit timings must be computed for 16 MHz.

Power: as SparkFun built it. In the car, DB9 pin 9 (+12 V) feeds the Uno's VIN through fuse F1 and diode D1, and the laptop's USB also connects. Ground joins through DB9 pin 2.

Responsibilities:

- Pass frames both ways, with a receive timestamp in microseconds.
- Start **closed** (not on the bus) after every reset. Join the bus only when the host asks: listen-only or normal.
- Report health: frame counts, buffer overflows, MCP2515 error counters (TEC, REC) and error flags, bus-off.
- Self-test on request: SPI register read-back and an internal loopback frame.
- Host watchdog: when the host enables it, close the bus if no command arrives within the set time. A crashed or unplugged app then leaves the car's bus alone.
- AVR watchdog at 250 ms: a hung sketch resets, and resets always come back closed. It runs in interrupt-then-reset mode: the interrupt notes which step of the loop stopped, for the reset report.
- Reset report (0.1.1): the `#R` line says why the bridge started. A few bytes in `.noinit` survive a reset that keeps the power on, so the sketch can tell cold (the power was off) from warm (the reset pin, a brown-out, noise) from the watchdog, and counts warm starts. A function in `.init3` keeps MCUSR before anything clears it, but the Uno's stock boot loader clears it first: on a stock Uno it reads 0 and the report has no flags (checked on a genuine Uno R3, 2026-10-16; r2 held no flags either, so it isn't used). Checked on the same Uno: a hang made on purpose came back as `reset=watchdog step=2`.
- Checked mode and refusal reasons (0.1.2): in the truck about one command in 250 reached the Uno damaged (2026-10-17 drives; not seen at the desk, even at 60 times the load). A damaged frame command could still parse as a different, valid frame, for example a read (`02 01 01`) turned into an engine computer reset (`02 11 01`). In checked mode every frame line, both ways, ends in `;XX`, a CRC-8 of the line: the bridge refuses a frame command whose check fails, and Scana drops a received line whose check fails. Each refusal says why first (`#e BADCHECK ...`), with the start of the line it got.
- At power-up, look for the MCP2515 for up to 100 ms (its oscillator starts after the Uno's), and again whenever the host asks to open.
- Never send a frame on its own. Every frame on the bus comes from a host command.

MCP2515 driver (own code):

- **One-shot transmit** (CANCTRL.OSM): a frame nobody acknowledges is tried once, not repeated forever. The failure comes back as `#e TXERR` (or `#e TXARB` on a bench with no bus, where the unterminated lines read dominant).
- **One transmit buffer** (TXB0) and a 4-frame software queue: the MCP2515 sends its three buffers by number, not by age, which could reorder ISO-TP frames.
- **Receive order kept:** with roll-over, once RXB0 is read a new frame can land in RXB0 before RXB1 is read; the driver remembers to read RXB1 next.
- Receive buffers: RXM=11 (take every frame) unless `#M` sets a standard-ID mask and filter. INT (D2) low means a frame is waiting, so idle polls cost no SPI traffic.
- Bit timing for 16 MHz, sample point 81.25% at 10k-500k (16 TQ: sync 1, prop 5, phase 1 7, phase 2 3, SJW 1), 80% at 800k (10 TQ), 75% at 1M (8 TQ). The host tests check every rate's bit time and sample point.

Keep static RAM use under 75% of 2048 bytes (the gate checks it).

Limits:

- The MCP2515 has 2 receive buffers. At high bus load an AVR may miss frames. The OBD port on many cars only carries diagnostic traffic, so this matters for sniffing, not for diagnostics. Hardware filters (masks) cut the load when only replies are needed.
- The serial link carries about 100 kB/s at 1 Mbaud. A full 500 kbit/s bus is about 4,000 frames/s, or about 100 kB/s as SLCAN text: at the limit. Diagnostics use a few hundred frames/s at most.
- Opening the serial port resets the Uno (DTR). The host pulses DTR itself, then waits for the `#R` ready line (about 2 s in all), so every session starts closed with counters at zero.
- Measured on a genuine Uno R3 (2026-10-16): 500 of 500 loopback frames back unchanged, round trip 3.9-4.3 ms, 244 frames/s one at a time (each waits for its `z` and its echo).

## 4. Serial protocol

SLCAN (Lawicel CAN232/CANUSB) text lines at **1,000,000 baud** 8N1. 16 MHz divides 1 Mbaud exactly, and the Uno's USB chip (ATmega16U2) handles it: no errors in the bench tests.

Why SLCAN: Linux tools already speak it (`slcand` makes a SocketCAN interface for can-utils, Wireshark and SavvyCAN), and lines are self-synchronising (each ends in CR). Scana talks to the serial port directly, so it needs no root and no `slcand`.

Standard commands used:

| Command | Meaning | Reply |
|---|---|---|
| `S0`-`S8` | Bit rate 10k, 20k, 50k, 100k, 125k, 250k, 500k, 800k, 1M (only while closed) | CR, or BEL on error |
| `O` | Open in normal mode (sends ACKs, can transmit) | CR / BEL |
| `L` | Open listen-only (never drives the bus) | CR / BEL |
| `C` | Close (always works) | CR |
| `tIIILDD..` | Send a standard frame: 3 hex ID, length, data | `z` CR |
| `TIIIIIIIILDD..` | Send an extended frame: 8 hex ID | `Z` CR |
| `rIIIL`, `RIIIIIIIIL` | Send a remote request | `z` / `Z` CR |
| `F` | Status flags | `Fxx` CR |
| `V`, `N` | Version, serial | `V0100`, `N5CA1` |
| `Z0`, `Z1` | Millisecond timestamps off, on | CR |

Frames are only sent in normal or loopback mode; anything else gets BEL. Received frames: `tIIILDD..` CR, with `TTTT` (ms) after `Z1` or `:TTTTTTTT` (us) after `#U1`.

Scana extensions start with `#`, which Lawicel does not use, so `slcand` never sends them:

| Command | Meaning |
|---|---|
| `#I` | Identity: `#I scana-bridge <version> mcp2515 16MHz` |
| `#H` | Health: `#H up= mode= rate= rx= tx= txerr= hwovr= bad= tec= rec= eflg=` (mode C N L K) |
| `#T` | Self-test, only while closed: reset, register read-back, a loopback frame. `#T PASS` or `#T FAIL <step> <hex value read>` (step 1 `00`/`FF`: nothing answers on SPI) |
| `#K` | Open in the MCP2515's loopback mode: frames sent come straight back; nothing reaches the bus |
| `#P` | Ping: `#P`. Keeps the host watchdog fed |
| `#Wnnnn` | Host watchdog, 1-60000 ms, `#W0` off. Any command feeds it. When it fires: bus closed and `#e HOSTWD` |
| `#U1`, `#U0` | Microsecond timestamps on received frames (`:` and 8 hex after the data) |
| `#E1`, `#E0` | Push events: `#e ERRWARN`, `ERRPASSIVE`, `BUSOFF`, `ACTIVE`, `RXOVR n`, `TXERR`, `TXARB`, `HOSTWD`. From 0.1.2 a refused command is preceded by why: `NOTOPEN`, `TXFULL`, `BADCHECK <line>`, `BADCMD <line>`, `TOOLONG <line>` or `REFUSED <line>` (the first 24 characters) |
| `#C1`, `#C0` | Checked mode (0.1.2): frame commands must end in `;XX`, the CRC-8 (polynomial 0x07, start 0) of the line before the `;`, and received frame lines get one. Scana turns it on for 0.1.2 and later |
| `#Mmmm,fff` | Standard-ID acceptance filter, only while closed; `#M` accepts everything |
| `#R` | Ready line, sent once after reset: `#R scana-bridge 0.1.2 mcp2515=ok reset=warm step=1 count=2 flags=B` (`mcp2515=missing` if the chip didn't answer). The reset report: `reset=cold`, `warm` or `watchdog`; `step=` where the loop was (1 between tasks, 2 reading commands, 3 reading frames, 4 sending, 5 bus status, 6 passing frames on, 7 waiting to write, 8 LEDs); `count=` warm starts since power-up (opening the port counts); `flags=` the chip's reset flags, only if the boot loader left them, which the stock Uno's doesn't (P power-on, E reset pin, B brown-out, W watchdog). 0.1.0 sends no report |

Every command gets exactly one reply, so replies match commands in order. Malformed or unknown lines get BEL and are counted in `#H` (`bad=`; refused frames count too). Bytes outside printable ASCII are ignored (the Uno's USB chip can send one when the port opens), as are line feeds.

## 5. Laptop app: crates and threads

Cargo workspace, seven crates:

| Crate | Holds | Rules |
|---|---|---|
| `scana-proto` | CAN frames, the bridge's SLCAN protocol, ISO-TP (with address bytes), OBD-II services, the PID table, mode 06 (results, scaling, names), fault codes, candump lines, and the read-only parts of UDS and KWP2000 (`uds.rs`: services and their safety levels, module fault codes with status and failure type, identity data IDs) | No I/O, no threads, no clock: time comes in as an argument. Deterministic |
| `scana-db` | Scana's knowledge as text files in `data/`, built into the program: fault code descriptions and advice, what each value means, glossary, guided jobs, page help, the lesson, the Library (`data/learn/topics/`, with the word linker and search in `library.rs`), vehicle profiles, and the module address book (`data/modules/addresses.toml`) | A file that fails to parse gives an empty list, never a crash; tests check every file parses |
| `scana-link` | `SlcanLink` (the bridge over serial, `serialport` crate) and `SimLink` (the simulator); later replay and SocketCAN | One trait, `Link`. `SlcanLink` has a reader thread; replies and frames go down separate channels |
| `scana-sim` | Virtual vehicles: the Hijet (healthy, with the dash, with faults), a 2006 Corolla 1.8 with P0420 (a worn catalyst: the rear oxygen sensor copies the front; it idles and holds 2500 rpm in turns of 45 s, so live tests can finish; no permanent codes; a freeze frame from a cruise), a two-ECU car, 29-bit and 250k variants; four invented cars made to differ from those (`--sim-car NAME`): `ute-diesel` (a 2016-2020 turbo-diesel ute: engine 7E0/7E8 and gearbox 7E1/7E9 both answering 7DF, compression-ignition monitors, no oxygen sensor voltages, fuel rail, EGR, DPF and exhaust temperature PIDs, P2002 stored with a freeze frame), `hatch-two-ecus` (a 2015-2019 petrol hatch: both computers answer speed, coolant and engine speed, frames padded with 0x00, calibration IDs from both, a pending P0455 only) and `hybrid` (a 2019-2023 hybrid: engine 7E0/7E8 and a hybrid computer 7E2/7EA with PID 5B; the engine reads 0 rpm while the car is ready and creeps on electric power, in turns of 90 s; fuel type 17), and a fourth, `ford-ms-can`, on the second bus (section 7b); `--sim-engine-off` turns the engine off so clearing can be tried; freeze frames, mode 06, clearing resets readiness; simulated modules (`modules.rs`: ABS, airbag, cluster, A/C, body behind a gateway, UDS and KWP2000) | Used by tests, the demo cars and `--sim`. The module layout is invented for testing. The Corolla is simulated on CAN; a real 2006 Corolla may use the K-line, depending on market and build |
| `scana-core` | `Client` (with `Channel`s for any module), `detect`, readers (supported PIDs, live values, codes, freeze frames, readiness, test results, vehicle info), the poller, the clear-codes gate (`safety`), module discovery and module readers (`modules.rs`), the recorder and session store, the health report, frame descriptions, and the session worker | The worker owns the link on its own thread and talks to the app by channels |
| `scana-app` | The egui 0.37 (eframe) desktop app: 16 pages in a sidebar of sections (Car, Diagnose, Modules, Learn, Tools) with icons and a search box, the Learn panel with Back and Forward, linked words (`widgets.rs`), status bar, themes (workshop dark by default), the logo and icons (`assets.rs`, sources in `design/`), its own line charts (`chart.rs`), a screenshot mode | Never blocks: draws from the latest updates. Layouts reflow from 1366 x 768 up |
| `scana-cli` | `scana`: `ports`, `probe`, `loopback`, `monitor`, `detect`, `pids`, `codes`, `info`, `freeze`, `tests`, `health`, `clear`, `record`, `modules`, `module`, `send`; `--sim`, `--sim-dash`, `--sim-faults`, `--sim-corolla`, `--sim-car`, `--sim-engine-off`, `--port`, `--log`, `--pins`, `--make`, `--parked` | For headless checks by people and AI agents |

Threads:

```
 serial reader thread ──frames──► session worker ──Update──► app (egui, repaints on each update)
 serial writes        ◄──────────  (Client, poller, ◄─Command──
                                    recorder, report)
```

- The app never touches the serial port; it sends `Command`s (connect, live list, reads, health check, clear with consent, record, mark) and draws from `Update`s.
- Every wait has a timeout. Every error reaches the app as a plain-English message.
- A panic in the worker is caught: the connection resets and the app shows a message (`session.rs`, `guarded`).
- The worker recovers by itself: a pulled cable or bridge reset reconnects every 2 s; a silent car (key off, three missed replies in a row, or an unacknowledged frame) is looked for again every 5 s. A bridge that refuses a frame or answers out of turn mid-session: the client first looks for a waiting `#R` line (a restart: its report reaches the events log, and the worker reconnects). With none, it asks `#H`: if the bus is still open, the command was damaged on the way, so the client logs it, sends a refused frame once more and carries on. Before 0.3.2 every refusal reconnected, and opening the port resets the Uno: 95 four-second drop-outs in 40 minutes on the 2026-10-17 drives.
- The app's clock keeps counting through reconnects: the worker adds an offset to each new link's times before they reach the app, so the graphs don't draw back over themselves.

## 6. Protocol stack

| Layer | Standard | Notes |
|---|---|---|
| CAN 2.0A and 2.0B | ISO 11898 | 11-bit and 29-bit IDs |
| Transport | ISO 15765-2 (ISO-TP) | Single, first, consecutive and flow-control frames. Scana sends flow control with block size 0 and STmin 0 unless a profile says otherwise |
| OBD-II addressing | ISO 15765-4 | 11-bit: functional 0x7DF, physical 0x7E0-0x7E7, replies 0x7E8-0x7EF. 29-bit: functional 0x18DB33F1, physical 0x18DAxxF1, replies 0x18DAF1xx |
| OBD-II services | SAE J1979, ISO 15031-5 | 01 live data, 02 freeze frame, 03 stored codes, 04 clear, 06 test results, 07 pending codes, 09 vehicle info, 0A permanent codes |
| Codes | SAE J2012, ISO 15031-6 | P, C, B, U codes; descriptions from openly licensed lists (see 9) |
| Enhanced | ISO 14229-1 (UDS), and KWP2000 on CAN (ISO 14230-3) | Read only: UDS 0x3E, 0x22, 0x19; KWP 0x1A, 0x21, 0x18, 0x13. Modules behind a gateway use ISO 15765-2 extended addressing: an address byte in front of every frame, 7 bytes of ISO-TP after it |

Mode 01 requests on CAN may ask for up to 6 PIDs at once. The scheduler groups PIDs to cut the request count.

## 7. Connecting to a car

Bus manners. These are hard rules:

1. Open **listen-only** first at each rate in turn (500k, then 250k), 800 ms each. Frames without errors confirm a rate and end the listening; errors rule a rate out.
2. A quiet bus (common: many OBD ports carry no traffic until a tester asks) needs one request to find the rate. Follow ISO 15765-4: open normal at the confirmed rate, or each quiet one, send one functional `01 00` (11-bit, then 29-bit) and collect replies for 100 ms. Never transmit at a rate where listening heard errors.
3. A frame nobody acknowledges (`#e TXERR` or `TXARB`, one-shot) ends that rate at once, without trying 29-bit there. On the desk with no car this is the whole story: 2 frames sent in total, then "No car answered" with what was tried (tested 2026-10-16).
4. Once replies arrive, store the protocol, the replying ECU IDs and the supported PIDs (PIDs 00, 20, 40, 60, 80, A0, C0) in the session.
5. Watch for **other testers**: requests on 0x7DF or 0x7E0-0x7E7 that Scana did not send, for example a dash display sharing the port through an OBD splitter at about 20 requests a second. Scana then lowers its own rate and matches replies to its own requests only. A reply to a request for several PIDs must hold all of them: a dash display asking for the first PID alone gets an answer that would otherwise pass for Scana's (a partial answer is used only if no full one comes). While the other tester is active, Scana packs only as many PIDs as fit a one-frame reply. The Hijet's engine computer answers the dash in the middle of a longer reply and sometimes drops the longer one (about 7 % of four-PID replies on the 2026-10-16 drives); a single frame can't be cut off. Scana's ISO-TP receiver keeps a long reply going when a single frame arrives in its middle, which ISO 15765-2 doesn't expect but this ECU does. Simple dash displays often read only single-frame mode 01 replies, and may stop asking for a PID after a refusal (`7F 01`): so Scana only asks for mode 01 PIDs the ECU lists as supported. While another tester is active, warn before multi-frame requests (two testers sending flow control can confuse an ECU).
6. If TEC rises above 96 (error warning) or the bus goes bus-off, drop to listen-only and tell the user.
7. Never transmit while finding a bit rate on the second bus (pins 3 and 11): that search is listen-only (section 7b). Never send an OBD-II request (0x7DF, 0x7E0-0x7E7, or the 29-bit OBD-II requests) on the second bus, at detection or later. `Client::send_frame` refuses them there, whoever asks.

## 7a. Finding and reading modules

Stage 3. **Discovery** finds every module that answers a scan tool, with one tester-present request (`3E 00`) per likely request ID. A UDS module answers `7E 00`; a module that speaks another dialect refuses (`7F 3E ..`). Either answer means a module is there. Hard rules:

1. Only while the car is still, when an ECU reports vehicle speed (PID 0D). With no speed reading, discovery runs and says it couldn't check. Every computer that reports speed is read, and again every 64 probes: a hybrid can pull away on electric power with its engine at 0 rpm, and discovery stops with an error if the car moves.
2. Listen for 1 s first. Never send on an ID heard on the bus: it belongs to someone else. A gateway ID heard on the bus is skipped whole.
3. Never send on the OBD-II functional ID (0x7DF) or the OBD-II reply IDs (0x7E8-0x7EF).
4. One request at a time at the usual pace (section 8), a 60 ms wait for each answer, and a stop at the first bus error. The user can stop it at any time.
5. At most 48 modules: more means something answers everything, so Scana stops.

What it tries:

| Car | Request IDs | Count |
|---|---|---|
| 11-bit OBD-II | 0x700-0x7F7, without the IDs above | about 238 |
| A maker family with a gateway | the gateway ID with every address byte (Toyota family: 0x750, replies from 0x758) | 256 |
| 29-bit OBD-II | 0x18DAxxF1 for every target xx | 255 |

A tester-present answer counts from any 11-bit ID in 0x700-0x7FF, not only request + 8. A maker family can also list reply offsets (`[[family]]` in the address book; Chrysler family: request - 0x280, so 0x747 answers from 0x4C7), and an answer from request + offset counts even outside that range. The request range does not widen, and nothing changes when the family is unknown. A module found this way keeps its real reply ID in its channel, so later reads use it. GM-family requests (0x2xx) are outside the probed range and are not found.

On the demo car it tries 494 addresses in about 35 s; with a dash display sharing the bus, expect about 50 s.

**Names.** A module that gives its system name (UDS F197, KWP 1A 97) is shown by it. Otherwise the address book (`data/modules/addresses.toml`) gives the usual module at that address for the maker family, shown with "(probably)". The family comes from the vehicle profile's make (Daihatsu is assumed to follow the Toyota layout until a discovery on the truck shows otherwise). Each address book entry names its source.

**Reading a module** (`modules::read_module`): identity first, which also settles the dialect. UDS: the identity data IDs F197, F187, F18A, F191-F195, F181, F188, F189, F18B, F18C, F190, one per request; `7F 22 11` means it isn't UDS. KWP: Toyota-style `1A 88 00` (lists sub-IDs), then `1A 88 nn`, then the common `1A` options. Then fault codes: UDS `19 02 FF` (or `19 02 08`), KWP `18 00 FF 00`, `18 02 FF 00` or `13`. OBD-II computers also give their service 03 and 07 codes, merged in.

Module codes carry more than OBD-II codes: UDS adds a failure type byte (`C0035-11`: "the circuit is shorted to ground") and a status byte (failing now, stored, pending, happened earlier, warning light). KWP has a status byte too (ISO 14230-3; makers vary).

**Data explorer**: `modules::scan_data` reads every ID in a range (UDS `22`, 16-bit IDs; KWP `21`, 8-bit) and keeps the ones answered. It stops if the module refuses the service itself, or after 5 silences in a row. Watching reads the found IDs in turn, taking turns with the live values, and the app lights up bytes that changed in the last 2 s. Names the user gives to bytes are kept in `$XDG_DATA_HOME/scana/names.toml`, on that computer only.

**Console** (advanced mode): one request typed in hex, to the explorer's module. `uds::level_of` checks it first; only safety level 0 (read) is sent.

Sessions: a discovery's frames are recorded like everything else, and `modules.toml` in the session folder lists what was found (without VINs). The health check reads every found module's codes again and adds an "All modules" section.

## 7b. The second bus (pins 3 and 11)

Some cars carry a second CAN bus on OBD-II pins 3 (CAN high) and 11 (CAN low). Ford (for example the FG Falcon, 2008-2014), Mazda and Volvo put a medium-speed bus there at 125 kbit/s, in ISO 11898-2 style, so the bridge's MCP2551 can talk to it. Body modules live on it: the instrument cluster, body control, audio, parking aid. Nothing is known to answer OBD-II (0x7DF) requests there. Some newer Fords may run it at 500 kbit/s (low confidence). Facts from github.com/jakka351/FG-Falcon, read 2026-10-17; unverified for other cars. The firmware needs no change: it already speaks 125 kbit/s (`S4`). The user's cable or switch decides which pins reach the bridge; Scana is told with `--pins 6-14|3-11` (CLI) or the Home page's choice (app), and the connection carries `Bus::Main` (the default) or `Bus::Second`.

Rules:

- **Finding it is listen-only.** `detect_second_bus` opens listen-only at 125 kbit/s, then 500, then 250, 1 s each, and takes the first rate with clean traffic (frames, no errors). It never transmits. Silence at every rate gives "Nothing heard on pins 3 and 11..." and errors only give "Pins 3 and 11 carry something Scana can't read...". Recovery after a lost link looks again the same way.
- **The connection has no OBD ECUs.** It holds the bit rate, 11-bit or 29-bit IDs as heard, and the label "Second CAN bus (pins 3 and 11)". No VIN can be read, so the make is unknown unless the user gives it (`--make`, or the Home page's optional field). The session worker polls nothing, reads no readiness and does nothing OBD there. The bus stays open listen-only.
- **Works:** the bus monitor (listen-only), recording (`frames.log` and `events.log`, no values), and module discovery and module reads (the data explorer too).
- **Module requests transmit, so they need a parked car.** Scana can't read speed there. The CLI's `modules`, `module` and `send` refuse without `--parked`; the app asks "Is the car parked?" (a dialog like the clear-codes one) before Find modules. Only then the worker opens the bus in normal mode, and it returns to listen-only when the job ends. The tick is asked again after a lost link. Discovery follows section 7a (listen first, never send on an ID heard in use, never 0x7DF or 0x7E8-0x7EF) at the detected rate, and also leaves out 0x7E0-0x7E7. A maker family's gateways and reply offsets apply only if a make was given.
- **Does not work, refused everywhere** with "That needs the main bus on pins 6 and 14: set the cable or switch back and connect again.": PIDs, codes (03, 07, 0A), readiness, freeze frames, tests, vehicle info, the health check, clearing, `diagnose --car` and live data. Nothing is ever cleared on this bus. The app shows that one line in place of those pages (and the Diagnose page's car measurements).
- **Names.** `[[address]]` entries in `addresses.toml` can carry `bus = "second"`. A second-bus lookup uses only those entries, for the make's family, so the "any" family's OBD names (7E0 "Engine computer") never appear there; a main-bus lookup uses only entries without `bus`. The FG Falcon's modules (720/728 instrument cluster, 726/72E body electronics, 727/72F audio, 736/73E parking aid, 781/789 BPM, 767/76F AIM, 7A6/7AE front display interface module) are family `ford`, source `fgfalcon`, with "FG Falcon" in each name.
- **Simulator.** `VirtualCar::ms_can_ford()` (`--sim-car ford-ms-can`, which implies `--pins 3-11`): an invented 125 kbit/s bus with status frames from five IDs, those seven modules answering UDS or KWP2000, and no OBD ECU. A simulated link has a bus too; a car on the other pins is silent.

## 8. Polling

- A request budget: a 50 ms gap (20 requests/s) when Scana is alone, 100 ms (10 requests/s) with another tester on the bus. Both are settings (estimates; tune from the truck tests).
- Each PID has a period (`poller.rs`): fast-changing values every 200 ms, most every 1 s, rarely-changing ones every 5 s. The poller packs due PIDs for one ECU 6 to a request; an unknown PID goes alone.
- One request outstanding at a time; 150 ms timeout for one ECU. A missed reply is skipped; three in a row and Scana looks for the car again. A refusal drops that batch's PIDs and says so.
- The live screen shows the real update rate of each value.

## 9. Data: definitions and vehicle profiles

PID formulas and the mode 06 tables are protocol facts, so they live in code (`scana-proto/src/pids.rs`, `mode06.rs`; mode 06 scaling checked against python-OBD's table, 2026-10-16). Everything a person might want to read or change is a text file in `crates/scana-db/data/`:

- `dtc/generic.tsv`: about 2,000 generic codes with **Scana's own plain-English descriptions**. SAE's J2012 text is copyright. About 360 were written one by one; the rest from each code's component and fault, checked against the open python-OBD list (GPL-2.0-or-later) and reworded in Scana's style, with bank and sensor positions added where the family's pattern is known (the description says so where it isn't). Codes not in the list get a description of their group, worked out from the number. C0035 to C0050 are described as GM-numbered: other makes use those numbers differently.
- `dtc/advice.toml`: what common codes usually mean and what to check first (general guidance).
- `learn/pids.toml` (every PID Scana decodes), `glossary.toml`, `jobs.toml`, `pages.toml`, `lesson.toml`: the Learn content.
- `learn/topics/*.toml`: the Library, 121 topics in three groups: reading live data (what each value means, normal ranges, and what common faults look like in the numbers), car systems and parts, and CAN and diagnostics. Each has a one-sentence summary, a short explanation, and where they fit "how to read it", the parts, common faults, more detail, related links (other topics, words, PIDs, guides, codes, pages) and at least two sources read on the date given. Researched by AI agents (2026-10-16) from public pages, each checked: figures without a source were removed or marked unverified, and claims about Scana itself were corrected against its design. The files also add about 80 words to the glossary (240 in all).
- Linked words (`library::find_links`): every topic title and alias and every glossary word is a phrase. Text anywhere in the app is scanned for them, longest first, whole words only, each once per text (once per topic in the Learn panel). Acronyms (two or more capitals, no small letters: CAN, MAP) match only in capitals, so the word "can" stays plain. A word that is both a topic alias and a glossary word links to the topic. Hover shows the summary; a click opens it in the Learn panel. `data/learn/no-link.toml` lists phrases that stay plain though they hold a known word in another sense ("level ground", "a knock or rattle", "to trim"); `cargo run -p scana-db --example link_audit` lists every one-word link with its context for review.
- Search (`library::search`): codes, topics, words, live values, guides and pages, best first; the sidebar's search box shows the results in place of the menu.
- `diag/*.toml` and `diag/tools.toml`: the diagnosis guides (section 15).
- Who the car is (`readers::identify`): its profile if one matches; else the make from the engine computer's VIN (`modules::make_of_vin`: Toyota, Lexus, Daihatsu, Holden, GM), shown as "Unknown Toyota" and used for maker-specific codes and module names; else "Unknown car". The VIN is only read when no profile matches.
- `vehicles/*.toml`: profiles. A profile matches when the engine ECU's live PID list equals its `match_pids`, or is within 2 PIDs of it (the closest wins): a software update, or a list first copied from a handheld tool, can be a PID or two out. The Hijet's list was corrected from its own support masks on the 2026-10-16 drives (4F and 50 added, 41 removed). The Hijet's has ECU names, notes, warning limits (coolant 107/112 °C, charging 12.8-15.2 V running) and the 2026-10-10 scan values.
- Later: vehicle history keyed by VIN, where the car reports one.

## 10. Sessions, logs and reports

- One folder per session: `$XDG_DATA_HOME/scana/sessions/<YYYY-MM-DD_HHMMSS>_<vehicle>/`, else `~/.local/share/scana/sessions/`.
  - `frames.log`: every frame in candump log format (`(1760580000.123456) can0 7E8#04410C1D4C000000`: engine speed 1875 rpm). Frames Scana sent end in ` T`. can-utils and SavvyCAN read it.
  - `values.csv`: `time_s,ecu,pid,name,value,unit,text`, one row per decoded value; markers are rows with `pid` `MARK`. `time_s` counts from the session's start.
  - `events.log`: one line per event: local time, seconds since the start, and what happened in words: recording started, bus events, bridge restarts with the bridge's reset report, failed sends, errors, the link opened again, the car found again.
  - Each new link's clock starts at 0. After a reconnect the recorder rebases its clock on the wall clock (and does so itself if a link time jumps back over a second), so times in every file keep going forward through a bridge restart.
  - `session.toml`: started, ended, vehicle, link, bit rate, addressing, ECUs, profile, counts, codes found, app version. Written when the session ends; a folder without it was cut short.
  - `report.html`: the health report, one self-contained page.
- Recording is on when the user turns it on (or always, in Settings). A health check run while not recording records a short session of its own, so every report has a folder with its frames.
- The Sessions page lists folders newest first and loads `values.csv` back into the graphs.
- Settings: `~/.config/scana/config.toml`.
- Logs are written as they come and flushed every second, so a crash or a pulled cable loses at most 1 s.

## 11. Safety model

Every service Scana can send has a level. The UI shows the current level in the status bar.

| Level | Allowed | Gate |
|---|---|---|
| 0 Read | OBD-II 01, 02, 03, 06, 07, 09, 0A; UDS 0x10 01 (default session), 0x3E, 0x22, 0x19; KWP 0x1A, 0x21 | Default |
| 1 Clear | OBD-II 04; UDS 0x14 | A confirmation that says what is lost (`safety::CLEAR_WARNING`; the app needs a tick, the CLI a typed CLEAR or `--yes`). A health report is saved first. Then `safety::clear_codes` reads PIDs 0C and 0D (only where supported) and refuses if the engine runs, the car moves, or no ECU reports engine speed. On a hybrid (an ECU reports PID 5B, or fuel type 17 to 22) 0 rpm proves nothing, as the engine starts by itself while the car is ready: it also refuses when the system voltage (PID 42) is 13.2 V or more, the level of a car that is ready (an estimate, not from a maker's data). Only then is 04 sent |
| 2 Change (stage 4) | Actuator tests (UDS 0x2F), routines (0x31) and service resets, only from a vehicle profile entry with a source | Advanced mode on, per-action confirmation, vehicle conditions checked before and during, a time limit, and every step logged |
| Never | Programming (0x34-0x37), security access (0x27), coding or writing data (0x2E), changing communication (0x28, 0x85) | Not in the code |

The console (data explorer, advanced mode) sends only level 0 requests: `uds::level_of` refuses anything else before it reaches the bus, with the reason. Raw frames on any ID can't be sent at all; module discovery's probes follow the rules in section 7a.

## 12. Reliability rules

- No `unwrap` or `expect` outside tests and `main` set-up. Library errors are typed (`thiserror`); the binaries use `anyhow`.
- Every decoder handles short, long and nonsense input without a panic. Fuzz the ISO-TP reassembler and the SLCAN parser.
- Pulling the USB cable, turning the key off, or a bridge reset mid-request: Scana notices within 1 s, says what happened, and reconnects by itself when the bridge comes back.
- Settings and vehicle profiles are checked at load. A bad file gives a message with the line, not a crash.
- The app starts in under 2 s on an 8th-generation Core i5 laptop and stays under 200 MB of memory in a 2 h logging session (targets, to be measured).

## 13. Testing

| Kind | What | Where |
|---|---|---|
| Unit | Decoders against values from standards and from the truck's 2026-10-10 scan; data files parse | `scana-proto`, `scana-db` |
| Session worker | Whole connections on the demo cars: live values, codes, recording, health check with its report, clearing with consent, module discovery (and stopping it), the data explorer, the console | `scana-core/src/session.rs` |
| Second bus | Detection by listening only (nothing transmitted), the two failure messages, OBD refused, discovery behind the parked tick with second-bus names | `scana-core/src/{detect,modules,session}.rs`, `scana-db/src/modules.rs` |
| Modules | Discovery on 11-bit, gateway and 29-bit layouts, with a dash display on the bus; skipped IDs; reading UDS and KWP modules; data scans; the console refusing writes | `scana-core/src/modules.rs`, `scana-sim/src/modules.rs` |
| Screens | `scana-app --screenshots DIR` drives every page on the demo cars at 1366 x 768 and once at 1024 x 700 and saves PNGs to look at; `--shot-bridge DIR` connects to the real bridge | By hand after UI changes |
| Property and fuzz | ISO-TP framing round trips, random frame streams | `scana-proto`, `cargo fuzz` |
| Simulator | Whole sessions against virtual ECUs, with faults: lost frames, late replies, negative responses, a second tester, wrong bit rate, cable pulled | `scana-sim`, `tests/` |
| Golden logs | Real `frames.log` files from the truck replayed through the decoders; results must not change without a reason | `tests/golden/` |
| Firmware host tests | The SLCAN parser and command handler are plain C++ in a header, compiled and tested on the build machine with g++ | `firmware/scana-bridge/test/` |
| Firmware build | `arduino-cli compile --fqbn arduino:avr:uno firmware/scana-bridge`, RAM under 75% | Gate |
| Soak | 24 h against the simulator: no leaks, no stalls, logs complete | Before each release |
| Bench and car | Checks with a real bridge and a car, from a written test plan | By hand |

`tools/gate.sh` runs `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`, the firmware host tests and the firmware build. Work is done only when the gate passes.

## 14. Building and installing

- Develop with Rust 1.99 stable, and arduino-cli with the AVR core 1.8.8 for the firmware.
- A release binary runs on another Linux machine when that machine's C library (glibc) is at least as new as the build machine's. Stage 5 adds an install script and a desktop entry.
- Serial access: the user must be in `uucp`, the group that owns `/dev/ttyACM*` on Arch-based systems (`dialout` on Debian, Ubuntu and Fedora). Scana says so in plain words when the port can't be opened.
- Release builds are stripped (`scana` about 2 MB, `scana-app` about 23 MB). The app needs the desktop's usual graphics libraries (it loads them at start: Wayland or X11, OpenGL); most desktop Linux systems have them.
- The Uno R3 shows as USB `2341:0043`. Scana finds it by that ID and by the `#R` ready line, never by guessing a port.

## 15. Diagnosis guides

A fault code says what the engine computer saw, not what broke. A diagnosis guide (`crates/scana-db/data/diag/<id>.toml`, one per code family) takes the user from the code to the likely cause:

- What the code means, how the system works, safety, and notes for particular makes and models, each with a source and a kind shown as a label: maker bulletin, recall, maker information, or owner reports (unverified). The app shows only the notes for the connected car's make (all, labelled, with no car), and every guide ends with where to check recalls by VIN (Product Safety Australia, NHTSA). Tests are written for people who do the work themselves: the do-it-yourself way first, a dealer or workshop only where a licence, the maker's software or real danger requires it.
- **Causes**, each with a weight: how often it is the cause, relative to the others (an estimate from the sources; it only sets the starting order), what fixes it, and a parts cost class.
- **Tests**, each with steps in simplified technical English, the tools it needs (`data/diag/tools.toml`; none means eyes, ears and hands), and possible answers. Each answer has effects on causes: +1 doubles a cause's score, +2 multiplies it by 4, -1 halves it. Shares are the scores scaled to 100 %. An answer can carry a note, point to another guide, or say "fix this first".
- **Automatic answers** (`auto`) on a connected CAN car: `codes` (groups of code patterns), `live` (a measure over samples taken only while a hold condition is met, after a precondition: mean, min, max, spread, fuel trim total, oxygen sensor switches per minute, rear/front switch ratio, difference), `mode06` (how far the worst named monitor result is past its limit), `readiness`, `freeze`. The user can always change the answer.

Some code numbers mean different things on different makes (a Toyota's C0200 is a wheel speed sensor). A guide can list such numbers per make (`maker_codes`); Scana opens the guide when the car's make (from its vehicle profile, or its diagnostic family: Daihatsu follows Toyota) matches, and otherwise says the number is maker-specific and offers it.

`scana_core::diagnose` holds the arithmetic and the measures, as pure functions: the app and the command line collect the samples. On a connected car both read the quick tests (codes, test results, readiness, the freeze frame) as soon as a guide opens, and say what they read; live tests wait for the user, as they need the engine held at a speed. A live measure counts only the time its conditions held: a pause (the revs left the range) is not measuring time. The app's Diagnose page and `scana diagnose CODE` (no car needed; `--car` to measure) run the guides; `scana explain` and the Learn panel are the fallback for any code or word. A diagnosis can be saved as an HTML report (`<sessions>/diagnoses/`).

The tests in `diag.rs` check every guide: it parses, every effect names a cause, every automatic answer names a real result, every tool, linked guide and glossary word exists, live tests name valid measures and PIDs, and there are no em or en dashes.
