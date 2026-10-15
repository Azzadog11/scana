# Scana

Scan software from down under: an open laptop scan tool for cars with OBD-II on CAN, built from an Arduino Uno R3 and a SparkFun CAN-BUS Shield.

- **scana-bridge**: firmware for the Uno R3 and the shield. It passes CAN frames between the car and the laptop over USB, and nothing else.
- **scana-app**: a Rust desktop app for Linux. It reads and clears fault codes, shows live data and graphs, records drives, writes health reports, finds every module in the car (brakes, airbags, body) and reads their codes, and explains every value and code as you go.
- **Diagnosis guides**: for common fault codes, the likely causes and tests that tell them apart, in plain English. Scana measures some tests itself on a connected car; the rest you answer from what you see, a meter, or another scan tool, so they work on any car.
- **scana**: the same engine on the command line, for scripts and testing.

It is tuned first for the 2024 Daihatsu Hijet Jumbo (S510P), and works with any car that has OBD-II on CAN (ISO 15765-4, 11-bit or 29-bit, 250 or 500 kbit/s).

## Status

Version 0.1.0. The firmware runs on an Uno R3 with the shield; the app and the command line work end to end on built-in demo cars and talk to the real bridge. Module discovery, module codes and the data explorer (stage 3) work on the simulated modules. Not yet proven on a car: expect rough edges.

## Hardware

| Part | Notes |
|---|---|
| Arduino Uno R3 | Genuine or a close clone with an ATmega16U2 USB chip |
| SparkFun CAN-BUS Shield, DEV-13262 | MCP2515 and MCP2551, 16 MHz crystal. Leave the DB9 jumpers on their default OBD-II setting |
| OBD-II to DB9 cable | Wired like SparkFun's CAB-10087: CAN-H on DB9 pin 3, CAN-L on pin 5, ground on pin 2. Check yours with a meter |
| A laptop running Linux | The app is laid out for 1366 x 768 and scales up |

## Safety

Scana only reads, except one thing: clearing fault codes, which asks first, saves a report, and refuses unless the engine is off and the car is still. It never reprograms, codes or unlocks anything. On the bus it listens before it talks, keeps to about 20 requests a second, and stops at the first transmit error. Looking for modules only runs with the car still, and never sends on an ID it hears in use. Use it at your own risk; see the licence.

## Build and run

Needs Rust 1.99 or newer, and arduino-cli with the AVR core for the firmware.

```
arduino-cli upload --fqbn arduino:avr:uno -p /dev/ttyACM0 firmware/scana-bridge   # flash the bridge once
cargo run --release -p scana-app                     # the desktop app
cargo run --release -p scana-app -- --demo           # straight to the demo car, no hardware needed
cargo run --release -p scana-cli -- probe            # check the bridge
cargo run --release -p scana-cli -- health           # health check of the car, with a report
cargo run --release -p scana-cli -- record           # record every live value until Enter
cargo run --release -p scana-cli -- modules          # find every module and read its codes
cargo run --release -p scana-cli -- module 7B0 --scan 00-FF   # one module: identity, codes, data IDs
cargo run --release -p scana-cli -- diagnose P0420   # step-by-step diagnosis, no car needed
cargo run --release -p scana-cli -- explain fuel trim          # look up a code, a word or a tool
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
