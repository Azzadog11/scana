# Scana

Scan software from down under: an open laptop scan tool for cars with OBD-II on CAN, built from an Arduino Uno R3 and a SparkFun CAN-BUS Shield.

- **scana-bridge**: firmware for the Uno R3 and the shield. It passes CAN frames between the car and the laptop over USB, and nothing else.
- **scana-app**: a Rust desktop app for Linux. It reads and clears fault codes, shows live data and graphs, records drives, writes health reports, and explains every value and code as you go.
- **scana**: the same engine on the command line, for scripts and testing.

It is tuned first for the 2024 Daihatsu Hijet Jumbo (S510P), and works with any car that has OBD-II on CAN (ISO 15765-4, 11-bit or 29-bit, 250 or 500 kbit/s).

## Status

Version 0.1.0. The firmware runs on an Uno R3 with the shield; the app and the command line work end to end on built-in demo cars and talk to the real bridge. Not yet proven on a car: expect rough edges.

## Hardware

| Part | Notes |
|---|---|
| Arduino Uno R3 | Genuine or a close clone with an ATmega16U2 USB chip |
| SparkFun CAN-BUS Shield, DEV-13262 | MCP2515 and MCP2551, 16 MHz crystal. Leave the DB9 jumpers on their default OBD-II setting |
| OBD-II to DB9 cable | Wired like SparkFun's CAB-10087: CAN-H on DB9 pin 3, CAN-L on pin 5, ground on pin 2. Check yours with a meter |
| A laptop running Linux | The app is laid out for 1366 x 768 and scales up |

## Safety

Scana only reads, except one thing: clearing fault codes, which asks first, saves a report, and refuses unless the engine is off and the car is still. It never reprograms, codes or unlocks anything. On the bus it listens before it talks, keeps to about 20 requests a second, and stops at the first transmit error. Use it at your own risk; see the licence.

## Build and run

Needs Rust 1.99 or newer, and arduino-cli with the AVR core for the firmware.

```
arduino-cli upload --fqbn arduino:avr:uno -p /dev/ttyACM0 firmware/scana-bridge   # flash the bridge once
cargo run --release -p scana-app                     # the desktop app
cargo run --release -p scana-app -- --demo           # straight to the demo car, no hardware needed
cargo run --release -p scana-cli -- probe            # check the bridge
cargo run --release -p scana-cli -- health           # health check of the car, with a report
cargo run --release -p scana-cli -- record           # record every live value until Enter
cargo run --release -p scana-cli -- --sim --sim-dash pids   # the simulated Hijet
tools/gate.sh                                        # formatting, lint, tests, firmware checks
```

Your user must be in the group that owns `/dev/ttyACM*` (`uucp` on Arch-based systems, `dialout` on Debian, Ubuntu and Fedora).

| Read | For |
|---|---|
| `docs/DESIGN.md` | How it works: firmware, serial protocol, crates, protocol stack, safety, tests |
| `CLAUDE.md` | Rules for anyone, human or AI, changing the code |

## Licence

Scana is free software under the GNU General Public License, version 3 or (at your option) any later version: see `LICENSE`. You may use, study, change and share it. If you share a changed version, you must share its source under the same licence, so Scana and its forks stay open.
