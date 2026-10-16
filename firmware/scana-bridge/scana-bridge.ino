// scana-bridge: a USB to CAN bridge for Scana.
// Arduino Uno R3 + SparkFun CAN-BUS Shield (MCP2515, MCP2551, 16 MHz crystal).
// Serial: 1,000,000 baud, SLCAN text lines plus Scana's '#' commands (docs/DESIGN.md section 4).
//
// Build:  arduino-cli compile --fqbn arduino:avr:uno firmware/scana-bridge
// Upload: arduino-cli upload --fqbn arduino:avr:uno -p /dev/ttyACM0 firmware/scana-bridge
#include <SPI.h>
#include <avr/interrupt.h>
#include <avr/wdt.h>

#include "bittiming.h"
#include "bridge.h"
#include "mcp2515.h"

const uint8_t PIN_CAN_CS = 10;
const uint8_t PIN_CAN_INT = 2;
const uint8_t PIN_SD_CS = 9;     // keep the microSD card off the SPI bus
const uint8_t PIN_LED_RXTX = 7;  // blinks on frames
const uint8_t PIN_LED_STATE = 8; // off closed, on normal, slow blink listen, fast blink loopback or errors
const unsigned long BAUD = 1000000;

Mcp2515 can;

// ---- Why the bridge started: the reset report in the "#R" line ----
//
// These live in .noinit, which start-up doesn't clear: they survive a reset that keeps the
// power on (the reset pin, the watchdog, usually a brown-out), and hold random bytes after
// the power was off. The magic number tells the two apart.
const uint16_t WARM_MAGIC = 0x5CA1;
const uint8_t WATCHDOG_MAGIC = 0xA5;
uint16_t warmMagic __attribute__((section(".noinit")));
uint8_t warmCount __attribute__((section(".noinit")));       // warm starts since power-up
uint8_t watchdogFired __attribute__((section(".noinit")));   // WATCHDOG_MAGIC: the watchdog reset us
uint8_t watchdogStep __attribute__((section(".noinit")));    // the Step that stopped
volatile uint8_t bridgeStep __attribute__((section(".noinit")));
uint8_t savedMcusr __attribute__((section(".noinit")));

// Runs before the C runtime starts. Keeps the reset flags before anything clears them, and
// stops the watchdog: after a watchdog reset it stays on at 15 ms and would reset again.
// The Uno's stock boot loader clears MCUSR before the sketch runs, so on a stock Uno this
// reads 0 and the report has no flags (checked 2026-10-16; r2 held no flags either). The
// warm magic and the watchdog interrupt below don't depend on it.
void saveResetFlags() __attribute__((naked, used, section(".init3")));
void saveResetFlags() {
  savedMcusr = MCUSR;
  MCUSR = 0;
  wdt_disable();
}

// The watchdog runs in interrupt-then-reset mode: if loop() stops for 250 ms this notes where,
// and 250 ms later the watchdog resets the bridge.
ISR(WDT_vect) {
  watchdogFired = WATCHDOG_MAGIC;
  watchdogStep = bridgeStep;
}

static char* putText(char* p, const char* s) {
  while (*s) *p++ = *s++;
  return p;
}

static char* putNumber(char* p, uint8_t v) {
  if (v >= 100) *p++ = (char)('0' + v / 100);
  if (v >= 10) *p++ = (char)('0' + v / 10 % 10);
  *p++ = (char)('0' + v % 10);
  return p;
}

// Writes the report, for example "reset=watchdog step=7 count=2 flags=W", into `out` (40 bytes).
//   reset=cold      the power was off: the bridge lost its supply.
//   reset=warm      the power stayed on: the reset pin (the port opening pulses it), a short
//                   brown-out, or noise. Mid-session, with the port open, not the port.
//   reset=watchdog  the firmware stopped for 250 ms; step= says where.
//   count=          warm starts since the power came on (opening the port counts too).
//   flags=          the chip's reset flags, only if the boot loader left them (the stock
//                   Uno's doesn't): P power-on, E reset pin, B brown-out, W watchdog.
static void resetReport(char* out) {
  bool warm = warmMagic == WARM_MAGIC;
  char* p = out;
  if (!warm) {
    p = putText(p, "reset=cold");
    warmCount = 0;
  } else {
    if (warmCount < 255) warmCount++;
    if (watchdogFired == WATCHDOG_MAGIC) {
      p = putText(p, "reset=watchdog step=");
      p = putNumber(p, watchdogStep);
    } else {
      p = putText(p, "reset=warm step=");
      p = putNumber(p, bridgeStep);
    }
    p = putText(p, " count=");
    p = putNumber(p, warmCount);
  }
  uint8_t flags = savedMcusr;
  if (flags & 0x0F) {
    p = putText(p, " flags=");
    if (flags & _BV(PORF)) *p++ = 'P';
    if (flags & _BV(EXTRF)) *p++ = 'E';
    if (flags & _BV(BORF)) *p++ = 'B';
    if (flags & _BV(WDRF)) *p++ = 'W';
  }
  *p = 0;
  warmMagic = WARM_MAGIC;
  watchdogFired = 0;
  bridgeStep = 0;
}

struct UnoHal {
  unsigned long activityUntil = 0;
  uint8_t mode = MODE_CLOSED, bus = BUS_ACTIVE;

  // If the chip didn't answer at start-up (shield loose), look again before giving up.
  bool chipOk() { return can.present() || can.begin(PIN_CAN_CS, PIN_CAN_INT); }
  bool canOpen(uint8_t m, uint8_t rateCode, bool filterOn, uint16_t mask, uint16_t filt) {
    uint8_t cnf[3];
    if (!bitTiming(rateCode, cnf)) return false;
    return can.open(m, cnf, filterOn, mask, filt);
  }
  void canClose() { can.close(); }
  bool canRead(CanFrame& f) { return can.read(f); }
  bool canTxIdle() { return can.txIdle(); }
  void canWrite(const CanFrame& f) { can.write(f); }
  uint8_t canTxDone() { return can.txDone(); }
  void canStatus(uint8_t& tec, uint8_t& rec, uint8_t& eflg) { can.status(tec, rec, eflg); }
  void canClearOverflow() { can.clearOverflow(); }
  uint8_t selfTest(uint8_t& detail) { return can.selfTest(detail); }
  uint32_t nowMs() { return millis(); }
  int serialRead() { return Serial.read(); }
  uint8_t serialSpace() {
    int n = Serial.availableForWrite();
    return n > 255 ? 255 : (uint8_t)n;
  }
  void serialWrite(const char* s, uint8_t n) { Serial.write((const uint8_t*)s, n); }
  void ledActivity() {
    digitalWrite(PIN_LED_RXTX, HIGH);
    activityUntil = millis() + 15;
  }
  void ledState(uint8_t m, uint8_t b) {
    mode = m;
    bus = b;
  }
  void step(uint8_t s) { bridgeStep = s; }
  void updateLeds() {
    unsigned long now = millis();
    if (activityUntil && (long)(now - activityUntil) >= 0) {
      digitalWrite(PIN_LED_RXTX, LOW);
      activityUntil = 0;
    }
    bool on;
    if (mode == MODE_CLOSED) on = false;
    else if (bus != BUS_ACTIVE) on = (now / 60) % 2;        // fast blink: bus errors
    else if (mode == MODE_NORMAL) on = true;
    else if (mode == MODE_LISTEN) on = (now / 500) % 2;     // slow blink
    else on = (now / 125) % 2;                              // loopback
    digitalWrite(PIN_LED_STATE, on ? HIGH : LOW);
  }
};

UnoHal hal;
Bridge<UnoHal> bridge(hal);

void setup() {
  char report[40];
  resetReport(report);
  pinMode(PIN_SD_CS, OUTPUT);
  digitalWrite(PIN_SD_CS, HIGH);
  pinMode(PIN_LED_RXTX, OUTPUT);
  pinMode(PIN_LED_STATE, OUTPUT);
  Serial.begin(BAUD);
  // At power-up the MCP2515's oscillator may not be running yet: try for about 100 ms.
  for (uint8_t i = 0; i < 10 && !can.begin(PIN_CAN_CS, PIN_CAN_INT); i++) delay(10);
  bridge.begin(report);
  wdt_enable(WDTO_250MS);  // a hang resets the bridge, and it comes back closed
  WDTCSR |= _BV(WDIE);     // ... after the interrupt notes where it stopped
}

void loop() {
  wdt_reset();
  // A stall that ended before the reset: forget it, and arm the interrupt again.
  if (!(WDTCSR & _BV(WDIE))) {
    watchdogFired = 0;
    WDTCSR |= _BV(WDIE);
  }
  bridgeStep = STEP_LOOP;
  bridge.poll();
  bridgeStep = STEP_LEDS;
  hal.updateLeds();
}
