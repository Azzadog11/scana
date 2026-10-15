// scana-bridge: a USB to CAN bridge for Scana.
// Arduino Uno R3 + SparkFun CAN-BUS Shield (MCP2515, MCP2551, 16 MHz crystal).
// Serial: 1,000,000 baud, SLCAN text lines plus Scana's '#' commands (docs/DESIGN.md section 4).
//
// Build:  arduino-cli compile --fqbn arduino:avr:uno firmware/scana-bridge
// Upload: arduino-cli upload --fqbn arduino:avr:uno -p /dev/ttyACM0 firmware/scana-bridge
#include <SPI.h>
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
  MCUSR = 0;
  wdt_disable();
  pinMode(PIN_SD_CS, OUTPUT);
  digitalWrite(PIN_SD_CS, HIGH);
  pinMode(PIN_LED_RXTX, OUTPUT);
  pinMode(PIN_LED_STATE, OUTPUT);
  Serial.begin(BAUD);
  // At power-up the MCP2515's oscillator may not be running yet: try for about 100 ms.
  for (uint8_t i = 0; i < 10 && !can.begin(PIN_CAN_CS, PIN_CAN_INT); i++) delay(10);
  bridge.begin();
  wdt_enable(WDTO_250MS);  // a hang resets the bridge, and it comes back closed
}

void loop() {
  wdt_reset();
  bridge.poll();
  hal.updateLeds();
}
