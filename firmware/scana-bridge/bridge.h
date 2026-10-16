// The bridge's logic: SLCAN commands, Scana's '#' commands, bus modes, buffers,
// the host watchdog and events. Plain C++ templated on a hardware layer (Hal), so the
// same code runs on the Uno and in the host tests (test/test_bridge.cpp).
//
// Rules this file keeps (docs/DESIGN.md sections 3, 4 and 7):
// - It starts closed and never sends a frame unless the host asked.
// - Frames are only sent in normal or loopback mode, never in listen-only.
// - Every command gets exactly one reply: CR, BEL, "z\r", "Z\r" or one line ending in CR.
//   Asynchronous lines are received frames (t T r R) and events ("#e ...").
#pragma once
#include <stdint.h>
#include "frame.h"
#include "slcan.h"

#define SCANA_BRIDGE_VERSION "0.1.1"

enum BusMode : uint8_t { MODE_CLOSED = 0, MODE_NORMAL = 1, MODE_LISTEN = 2, MODE_LOOPBACK = 3 };

// Result of the last transmission, from Hal::canTxDone().
enum TxResult : uint8_t { TX_NONE = 0, TX_OK = 1, TX_ERROR = 2, TX_ARBITRATION = 3 };

// MCP2515 EFLG bits.
const uint8_t EFLG_RX1OVR = 0x80, EFLG_RX0OVR = 0x40, EFLG_TXBO = 0x20, EFLG_TXEP = 0x10,
              EFLG_RXEP = 0x08, EFLG_EWARN = 0x01;

enum BusState : uint8_t { BUS_ACTIVE = 0, BUS_WARNING = 1, BUS_PASSIVE = 2, BUS_OFF = 3 };

// Where the bridge is, kept for the reset report: after a watchdog reset the "#R" line says
// which step stopped. 1 (the top of loop) and 8 (the LEDs) are set by the sketch.
enum Step : uint8_t {
  STEP_LOOP = 1,
  STEP_SERIAL_IN = 2,
  STEP_CAN_RX = 3,
  STEP_CAN_TX = 4,
  STEP_STATUS = 5,
  STEP_SERIAL_OUT = 6,
  STEP_WRITE_WAIT = 7,
  STEP_LEDS = 8,
};

inline BusState busStateOf(uint8_t eflg) {
  if (eflg & EFLG_TXBO) return BUS_OFF;
  if (eflg & (EFLG_TXEP | EFLG_RXEP)) return BUS_PASSIVE;
  if (eflg & EFLG_EWARN) return BUS_WARNING;
  return BUS_ACTIVE;
}

// Hal must provide:
//   bool     chipOk();
//   bool     canOpen(uint8_t mode, uint8_t rateCode, bool filterOn, uint16_t mask, uint16_t filt);
//   void     canClose();
//   bool     canRead(CanFrame& f);          // fills f.us
//   bool     canTxIdle();
//   void     canWrite(const CanFrame& f);
//   uint8_t  canTxDone();                   // TxResult, once per transmission
//   void     canStatus(uint8_t& tec, uint8_t& rec, uint8_t& eflg);
//   void     canClearOverflow();
//   uint8_t  selfTest(uint8_t& detail);     // 0 = pass, else the failed step; detail = value read
//   uint32_t nowMs();
//   int      serialRead();                  // -1 if nothing waiting
//   uint8_t  serialSpace();
//   void     serialWrite(const char* s, uint8_t n);
//   void     ledActivity();
//   void     ledState(uint8_t mode, uint8_t busState);
//   void     step(uint8_t s);               // a Step, kept for the reset report
template <class Hal>
class Bridge {
 public:
  static const uint8_t RX_SLOTS = 20;   // received frames waiting for the serial port
  static const uint8_t TX_SLOTS = 4;    // frames waiting for the MCP2515's transmit buffer
  static const uint8_t LINE_MAX = 40;   // longest command line, without the CR

  explicit Bridge(Hal& hal) : hal_(hal) {}

  // `reset` is the reset report ("reset=warm flags=B step=3 count=2"), added to the "#R" line.
  void begin(const char* reset = "") {
    startMs_ = hal_.nowMs();
    char out[96];
    uint8_t n = 0;
    n = put(out, n, "#R scana-bridge " SCANA_BRIDGE_VERSION " mcp2515=");
    n = put(out, n, hal_.chipOk() ? "ok" : "missing");
    if (*reset) out[n++] = ' ';
    for (uint8_t k = 0; reset[k] && k < 56; k++) out[n++] = reset[k];
    out[n++] = '\r';
    writeAll(out, n);
    hal_.ledState(mode_, BUS_ACTIVE);
  }

  // Call as often as possible.
  void poll() {
    uint32_t now = hal_.nowMs();
    hal_.step(STEP_SERIAL_IN);
    readSerial(now);
    if (mode_ != MODE_CLOSED) {
      hal_.step(STEP_CAN_RX);
      receiveFrames();
      hal_.step(STEP_CAN_TX);
      transmit();
      if ((uint32_t)(now - lastStatusMs_) >= 10) {
        lastStatusMs_ = now;
        hal_.step(STEP_STATUS);
        checkStatus();
      }
      // Read the clock again: handling a command (opening the bus takes about 5 ms) may have
      // set lastCommandMs_ later than `now`, and the unsigned difference would wrap.
      int32_t quiet = (int32_t)(hal_.nowMs() - lastCommandMs_);
      if (watchdogMs_ && quiet >= (int32_t)watchdogMs_) {
        close();
        event("HOSTWD");
      }
    }
    hal_.step(STEP_SERIAL_OUT);
    sendFrames();
  }

  // For tests.
  BusMode mode() const { return mode_; }
  uint32_t rxCount() const { return rxCount_; }

 private:
  Hal& hal_;
  BusMode mode_ = MODE_CLOSED;
  uint8_t rate_ = 6;  // 500k
  slcan::TimeMode timeMode_ = slcan::TIME_NONE;
  bool events_ = false;
  bool filterOn_ = false;
  uint16_t mask_ = 0, filt_ = 0;
  uint16_t watchdogMs_ = 0;
  uint32_t lastCommandMs_ = 0, lastStatusMs_ = 0, startMs_ = 0;
  BusState busState_ = BUS_ACTIVE;
  bool txBusy_ = false;

  char line_[LINE_MAX + 1];
  uint8_t lineLen_ = 0;
  bool lineTooLong_ = false;

  CanFrame rx_[RX_SLOTS];
  uint8_t rxHead_ = 0, rxCount_q_ = 0;
  CanFrame tx_[TX_SLOTS];
  uint8_t txHead_ = 0, txCount_ = 0;

  uint32_t rxCount_ = 0, txOk_ = 0, txErr_ = 0, hwOverflow_ = 0, badCmd_ = 0;

  // ---- output ----

  static uint8_t put(char* out, uint8_t n, const char* s) {
    while (*s) out[n++] = *s++;
    return n;
  }
  static uint8_t putDec(char* out, uint8_t n, uint32_t v) {
    char tmp[10];
    uint8_t k = 0;
    do {
      tmp[k++] = (char)('0' + v % 10);
      v /= 10;
    } while (v);
    while (k) out[n++] = tmp[--k];
    return n;
  }

  // Replies and events must not be lost: wait for room in the serial buffer, and write
  // long lines in pieces (the Uno's transmit buffer holds 63 bytes).
  void writeAll(const char* s, uint8_t n) {
    while (n) {
      uint8_t room = hal_.serialSpace();
      if (!room) {
        hal_.step(STEP_WRITE_WAIT);
        continue;
      }
      uint8_t k = room < n ? room : n;
      hal_.serialWrite(s, k);
      s += k;
      n = (uint8_t)(n - k);
    }
  }
  void ok() { writeAll("\r", 1); }
  void bell() { writeAll("\a", 1); }

  void event(const char* what) {
    if (!events_) return;
    char out[32];
    uint8_t n = put(out, 0, "#e ");
    n = put(out, n, what);
    out[n++] = '\r';
    writeAll(out, n);
  }
  void eventCount(const char* what, uint32_t count) {
    if (!events_) return;
    char out[32];
    uint8_t n = put(out, 0, "#e ");
    n = put(out, n, what);
    out[n++] = ' ';
    n = putDec(out, n, count);
    out[n++] = '\r';
    writeAll(out, n);
  }

  // ---- serial input ----

  void readSerial(uint32_t now) {
    for (uint8_t budget = 0; budget < 64; budget++) {
      int c = hal_.serialRead();
      if (c < 0) return;
      if (c == '\r') {
        if (lineTooLong_) {
          badCmd_++;
          bell();
        } else if (lineLen_ > 0) {
          line_[lineLen_] = 0;
          lastCommandMs_ = now;
          handle(line_, lineLen_);
        }
        lineLen_ = 0;
        lineTooLong_ = false;
      } else if (c < 0x20 || c > 0x7E) {
        // Ignore line feeds (a terminal sending CR LF) and noise bytes, such as the one
        // the Uno's USB chip can send when the port opens.
      } else if (lineLen_ < LINE_MAX) {
        line_[lineLen_++] = (char)c;
      } else {
        lineTooLong_ = true;
      }
    }
  }

  // ---- commands ----

  void handle(const char* s, uint8_t n) {
    switch (s[0]) {
      case 'S':
        if (n == 2 && mode_ == MODE_CLOSED && s[1] >= '0' && s[1] <= '8') {
          rate_ = (uint8_t)(s[1] - '0');
          ok();
        } else {
          fail();
        }
        return;
      case 'O': open(n == 1 ? MODE_NORMAL : MODE_CLOSED); return;
      case 'L': open(n == 1 ? MODE_LISTEN : MODE_CLOSED); return;
      case 'C':
        if (n == 1) {
          close();
          ok();
        } else {
          fail();
        }
        return;
      case 't':
      case 'T':
      case 'r':
      case 'R': queueFrame(s, n); return;
      case 'F': flags(); return;
      case 'V': writeAll("V0100\r", 6); return;
      case 'N': writeAll("N5CA1\r", 6); return;
      case 'Z':
        if (n == 2 && (s[1] == '0' || s[1] == '1')) {
          timeMode_ = s[1] == '1' ? slcan::TIME_MS : slcan::TIME_NONE;
          ok();
        } else {
          fail();
        }
        return;
      case '#': extension(s + 1, (uint8_t)(n - 1)); return;
      default: fail(); return;
    }
  }

  void fail() {
    badCmd_++;
    bell();
  }

  void open(BusMode m) {
    if (m == MODE_CLOSED || mode_ != MODE_CLOSED || !hal_.chipOk()) {
      fail();
      return;
    }
    if (!hal_.canOpen(m, rate_, filterOn_, mask_, filt_)) {
      hal_.canClose();
      fail();
      return;
    }
    mode_ = m;
    rxHead_ = rxCount_q_ = 0;
    txHead_ = txCount_ = 0;
    txBusy_ = false;
    busState_ = BUS_ACTIVE;
    lastCommandMs_ = hal_.nowMs();
    hal_.ledState(mode_, busState_);
    ok();
  }

  void close() {
    if (mode_ != MODE_CLOSED) hal_.canClose();
    mode_ = MODE_CLOSED;
    txCount_ = 0;
    txBusy_ = false;
    hal_.ledState(mode_, BUS_ACTIVE);
  }

  void queueFrame(const char* s, uint8_t n) {
    CanFrame f;
    if ((mode_ != MODE_NORMAL && mode_ != MODE_LOOPBACK) || txCount_ >= TX_SLOTS ||
        !slcan::parseFrameCmd(s, n, f)) {
      fail();
      return;
    }
    tx_[(uint8_t)(txHead_ + txCount_) % TX_SLOTS] = f;
    txCount_++;
    transmit();
    writeAll(f.ext ? "Z\r" : "z\r", 2);
  }

  // Lawicel status flags: bit 2 error warning, bit 3 data overrun, bit 5 error passive,
  // bit 7 bus error (used here for bus-off).
  void flags() {
    uint8_t tec = 0, rec = 0, eflg = 0;
    if (mode_ != MODE_CLOSED) hal_.canStatus(tec, rec, eflg);
    uint8_t f = 0;
    if (eflg & EFLG_EWARN) f |= 0x04;
    if ((eflg & (EFLG_RX0OVR | EFLG_RX1OVR)) || hwOverflow_) f |= 0x08;
    if (eflg & (EFLG_TXEP | EFLG_RXEP)) f |= 0x20;
    if (eflg & EFLG_TXBO) f |= 0x80;
    char out[4] = {'F', slcan::hexDigit(f >> 4), slcan::hexDigit(f), '\r'};
    writeAll(out, 4);
  }

  void extension(const char* s, uint8_t n) {
    if (n == 0) {
      fail();
      return;
    }
    switch (s[0]) {
      case 'I':
        if (n == 1) {
          writeAll("#I scana-bridge " SCANA_BRIDGE_VERSION " mcp2515 16MHz\r",
                   sizeof("#I scana-bridge " SCANA_BRIDGE_VERSION " mcp2515 16MHz\r") - 1);
        } else {
          fail();
        }
        return;
      case 'P':
        if (n == 1) writeAll("#P\r", 3); else fail();
        return;
      case 'H':
        if (n == 1) health(); else fail();
        return;
      case 'T':
        if (n == 1 && mode_ == MODE_CLOSED) {
          uint8_t detail = 0;
          uint8_t step = hal_.selfTest(detail);
          char out[20];
          uint8_t k = put(out, 0, step ? "#T FAIL " : "#T PASS");
          if (step) {
            k = putDec(out, k, step);
            out[k++] = ' ';
            out[k++] = slcan::hexDigit(detail >> 4);
            out[k++] = slcan::hexDigit(detail);
          }
          out[k++] = '\r';
          writeAll(out, k);
        } else {
          fail();
        }
        return;
      case 'K': open(n == 1 ? MODE_LOOPBACK : MODE_CLOSED); return;
      case 'W': {
        uint32_t v = 0;
        if (n < 2 || n > 6) {
          fail();
          return;
        }
        for (uint8_t i = 1; i < n; i++) {
          if (s[i] < '0' || s[i] > '9') {
            fail();
            return;
          }
          v = v * 10 + (uint8_t)(s[i] - '0');
        }
        if (v > 60000) {
          fail();
          return;
        }
        watchdogMs_ = (uint16_t)v;
        ok();
        return;
      }
      case 'U':
        if (n == 2 && (s[1] == '0' || s[1] == '1')) {
          timeMode_ = s[1] == '1' ? slcan::TIME_US : slcan::TIME_NONE;
          ok();
        } else {
          fail();
        }
        return;
      case 'E':
        if (n == 2 && (s[1] == '0' || s[1] == '1')) {
          events_ = s[1] == '1';
          ok();
        } else {
          fail();
        }
        return;
      case 'M': filter(s + 1, (uint8_t)(n - 1)); return;
      default: fail(); return;
    }
  }

  // "#M" accepts everything. "#Mmmm,fff" accepts standard IDs where (id & mask) == filt.
  void filter(const char* s, uint8_t n) {
    if (mode_ != MODE_CLOSED) {
      fail();
      return;
    }
    if (n == 0) {
      filterOn_ = false;
      ok();
      return;
    }
    uint32_t m, f;
    if (n != 7 || s[3] != ',' || !slcan::parseHex(s, 3, m) || !slcan::parseHex(s + 4, 3, f) ||
        m > 0x7FF || f > 0x7FF) {
      fail();
      return;
    }
    filterOn_ = true;
    mask_ = (uint16_t)m;
    filt_ = (uint16_t)f;
    ok();
  }

  void health() {
    uint8_t tec = 0, rec = 0, eflg = 0;
    if (mode_ != MODE_CLOSED) hal_.canStatus(tec, rec, eflg);
    static const char modes[] = "CNLK";
    char out[120];
    uint8_t n = put(out, 0, "#H up=");
    n = putDec(out, n, (hal_.nowMs() - startMs_) / 1000UL);
    n = put(out, n, " mode=");
    out[n++] = modes[mode_];
    n = put(out, n, " rate=");
    n = putDec(out, n, rate_);
    n = put(out, n, " rx=");
    n = putDec(out, n, rxCount_);
    n = put(out, n, " tx=");
    n = putDec(out, n, txOk_);
    n = put(out, n, " txerr=");
    n = putDec(out, n, txErr_);
    n = put(out, n, " hwovr=");
    n = putDec(out, n, hwOverflow_);
    n = put(out, n, " bad=");
    n = putDec(out, n, badCmd_);
    n = put(out, n, " tec=");
    n = putDec(out, n, tec);
    n = put(out, n, " rec=");
    n = putDec(out, n, rec);
    n = put(out, n, " eflg=");
    out[n++] = slcan::hexDigit(eflg >> 4);
    out[n++] = slcan::hexDigit(eflg);
    out[n++] = '\r';
    writeAll(out, n);
  }

  // ---- CAN ----

  void receiveFrames() {
    // Stop reading when the queue is full: the MCP2515 holds two more, then counts overruns.
    while (rxCount_q_ < RX_SLOTS) {
      CanFrame& f = rx_[(uint8_t)(rxHead_ + rxCount_q_) % RX_SLOTS];
      if (!hal_.canRead(f)) return;
      rxCount_q_++;
      rxCount_++;
      hal_.ledActivity();
    }
  }

  void transmit() {
    if (txBusy_) {
      uint8_t r = hal_.canTxDone();
      if (r == TX_NONE) return;
      txBusy_ = false;
      if (r == TX_OK) {
        txOk_++;
        hal_.ledActivity();
      } else {
        txErr_++;
        event(r == TX_ARBITRATION ? "TXARB" : "TXERR");
      }
    }
    if (txCount_ && hal_.canTxIdle()) {
      hal_.canWrite(tx_[txHead_]);
      txHead_ = (uint8_t)((txHead_ + 1) % TX_SLOTS);
      txCount_--;
      txBusy_ = true;
    }
  }

  void checkStatus() {
    uint8_t tec, rec, eflg;
    hal_.canStatus(tec, rec, eflg);
    if (eflg & (EFLG_RX0OVR | EFLG_RX1OVR)) {
      hwOverflow_++;
      hal_.canClearOverflow();
      eventCount("RXOVR", hwOverflow_);
    }
    BusState s = busStateOf(eflg);
    if (s != busState_) {
      busState_ = s;
      static const char* const names[4] = {"ACTIVE", "ERRWARN", "ERRPASSIVE", "BUSOFF"};
      event(names[s]);
      hal_.ledState(mode_, busState_);
    }
  }

  void sendFrames() {
    char out[slcan::MAX_FRAME_LINE];
    while (rxCount_q_) {
      CanFrame& f = rx_[rxHead_];
      uint8_t n = slcan::formatFrame(f, out, timeMode_);
      if (hal_.serialSpace() < n) return;  // try again on the next poll
      hal_.serialWrite(out, n);
      rxHead_ = (uint8_t)((rxHead_ + 1) % RX_SLOTS);
      rxCount_q_--;
    }
  }
};
