// Host tests for the bridge logic. Build and run:
//   g++ -std=c++17 -Wall -Wextra -Werror -I.. test_bridge.cpp -o /tmp/test_bridge && /tmp/test_bridge
// tools/gate.sh runs this.
#include <cstdio>
#include <cstring>
#include <deque>
#include <string>
#include <vector>

#include "bittiming.h"
#include "bridge.h"
#include "slcan.h"

static int failures = 0, checks = 0;
#define CHECK(cond)                                                    \
  do {                                                                 \
    checks++;                                                          \
    if (!(cond)) {                                                     \
      failures++;                                                      \
      std::printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);      \
    }                                                                  \
  } while (0)
#define CHECK_EQ(a, b)                                                                 \
  do {                                                                                 \
    checks++;                                                                          \
    auto got_ = (a);                                                                   \
    if (!(got_ == (b))) {                                                              \
      failures++;                                                                      \
      std::printf("FAIL %s:%d: %s == %s\n  got \"%s\"\n", __FILE__, __LINE__, #a, #b, \
                  printable(got_).c_str());                                            \
    }                                                                                  \
  } while (0)

static std::string printable(const std::string& s) {
  std::string o;
  for (char c : s) {
    if (c == '\r') o += "\\r";
    else if (c == '\a') o += "\\a";
    else o += c;
  }
  return o;
}
template <class T>
static std::string printable(const T& v) {
  return std::to_string(v);
}

struct FakeHal {
  bool chip = true;
  std::deque<int> in;
  std::string out;
  std::deque<CanFrame> rxq;
  std::vector<CanFrame> sent;
  uint32_t ms = 1000;
  bool pendingTx = false, holdTx = false;
  uint8_t txResult = TX_OK;
  uint8_t tec = 0, rec = 0, eflg = 0;
  int openMode = -1;
  uint8_t openRate = 0;
  bool openFilter = false, openOk = true;
  uint16_t mask = 0, filt = 0;
  uint8_t selfTestResult = 0;
  uint8_t space = 255;
  int closes = 0;
  uint32_t openTakesMs = 0;  // like the real chip: opening takes a few ms

  bool chipOk() { return chip; }
  bool canOpen(uint8_t m, uint8_t r, bool fo, uint16_t mk, uint16_t f) {
    ms += openTakesMs;
    openMode = m;
    openRate = r;
    openFilter = fo;
    mask = mk;
    filt = f;
    return openOk;
  }
  void canClose() {
    closes++;
    openMode = -1;
  }
  bool canRead(CanFrame& f) {
    if (rxq.empty()) return false;
    f = rxq.front();
    rxq.pop_front();
    return true;
  }
  bool canTxIdle() { return !pendingTx; }
  void canWrite(const CanFrame& f) {
    sent.push_back(f);
    pendingTx = true;
  }
  uint8_t canTxDone() {
    if (!pendingTx || holdTx) return TX_NONE;
    pendingTx = false;
    return txResult;
  }
  void canStatus(uint8_t& t, uint8_t& r, uint8_t& e) {
    t = tec;
    r = rec;
    e = eflg;
  }
  void canClearOverflow() { eflg &= (uint8_t)~(EFLG_RX0OVR | EFLG_RX1OVR); }
  uint8_t selfTest(uint8_t& detail) {
    detail = 0xFF;
    return selfTestResult;
  }
  uint32_t nowMs() { return ms; }
  int serialRead() {
    if (in.empty()) return -1;
    int c = in.front();
    in.pop_front();
    return c;
  }
  uint8_t serialSpace() { return space; }
  void serialWrite(const char* s, uint8_t n) { out.append(s, n); }
  void ledActivity() {}
  void ledState(uint8_t, uint8_t) {}
  uint8_t lastStep = 0;
  void step(uint8_t s) { lastStep = s; }
};

struct Rig {
  FakeHal hal;
  Bridge<FakeHal> b{hal};
  Rig() {
    b.begin();
    hal.out.clear();
  }
  // Sends one command line and returns everything the bridge wrote.
  std::string cmd(const std::string& line) {
    for (char c : line) hal.in.push_back(c);
    hal.in.push_back('\r');
    for (int i = 0; i < 5; i++) b.poll();
    std::string o = hal.out;
    hal.out.clear();
    return o;
  }
  std::string pollOut() {
    b.poll();
    std::string o = hal.out;
    hal.out.clear();
    return o;
  }
};

static CanFrame frame(uint32_t id, std::vector<uint8_t> d, bool ext = false) {
  CanFrame f{};
  f.id = id;
  f.ext = ext;
  f.len = (uint8_t)d.size();
  for (size_t i = 0; i < d.size(); i++) f.data[i] = d[i];
  return f;
}

static void testHexAndParse() {
  uint32_t v;
  CHECK(slcan::parseHex("7DF", 3, v) && v == 0x7DF);
  CHECK(slcan::parseHex("aB", 2, v) && v == 0xAB);
  CHECK(!slcan::parseHex("7G", 2, v));

  CanFrame f;
  CHECK(slcan::parseFrameCmd("t7DF80201000000000000", 21, f));
  CHECK(f.id == 0x7DF && !f.ext && !f.rtr && f.len == 8 && f.data[0] == 0x02 && f.data[1] == 0x01);
  CHECK(slcan::parseFrameCmd("t1230", 5, f) && f.len == 0);
  CHECK(slcan::parseFrameCmd("T18DB33F1802010C0000000000", 26, f));
  CHECK(f.id == 0x18DB33F1 && f.ext && f.data[2] == 0x0C);
  CHECK(slcan::parseFrameCmd("r7E08", 5, f) && f.rtr && f.len == 8);
  CHECK(slcan::parseFrameCmd("R18DAF1102", 10, f) && f.rtr && f.ext && f.len == 2);
  CHECK(!slcan::parseFrameCmd("t80080000000000000000", 21, f));   // id > 0x7FF
  CHECK(!slcan::parseFrameCmd("T2000000010", 11, f));             // id > 29 bits
  CHECK(!slcan::parseFrameCmd("t7DF9", 5, f));                    // dlc 9
  CHECK(!slcan::parseFrameCmd("t7DF202", 7, f));                  // short data
  CHECK(!slcan::parseFrameCmd("t7DF10102", 9, f));                // long data
  CHECK(!slcan::parseFrameCmd("t7DF1ZZ", 7, f));                  // not hex
  CHECK(!slcan::parseFrameCmd("x7DF0", 5, f));
}

static void testFormat() {
  char out[slcan::MAX_FRAME_LINE];
  CanFrame f = frame(0x7E8, {0x04, 0x41, 0x0C, 0x1D, 0x4C, 0x00, 0x00, 0x00});
  f.us = 0x0012ABCD;
  uint8_t n = slcan::formatFrame(f, out, slcan::TIME_NONE);
  CHECK_EQ(std::string(out, n), std::string("t7E8804410C1D4C000000\r"));
  n = slcan::formatFrame(f, out, slcan::TIME_US);
  CHECK_EQ(std::string(out, n), std::string("t7E8804410C1D4C000000:0012ABCD\r"));
  f.us = 61234000UL;  // 61.234 s -> 1234 ms within the minute = 0x04D2
  n = slcan::formatFrame(f, out, slcan::TIME_MS);
  CHECK_EQ(std::string(out, n), std::string("t7E8804410C1D4C00000004D2\r"));
  CanFrame e = frame(0x18DAF110, {0x03, 0x7F, 0x22, 0x31}, true);
  n = slcan::formatFrame(e, out, slcan::TIME_NONE);
  CHECK_EQ(std::string(out, n), std::string("T18DAF1104037F2231\r"));
  CanFrame r = frame(0x123, {});
  r.rtr = true;
  r.len = 2;
  n = slcan::formatFrame(r, out, slcan::TIME_NONE);
  CHECK_EQ(std::string(out, n), std::string("r1232\r"));
  CanFrame big = frame(0x1FFFFFFF, {1, 2, 3, 4, 5, 6, 7, 8}, true);
  big.us = 0xFFFFFFFF;
  n = slcan::formatFrame(big, out, slcan::TIME_US);
  CHECK(n <= slcan::MAX_FRAME_LINE);
}

static void testBitTiming() {
  uint8_t cnf[3];
  for (uint8_t code = 0; code <= 8; code++) {
    CHECK(bitTiming(code, cnf));
    uint32_t brp = cnf[0] & 0x3F, sjw = (cnf[0] >> 6) + 1;
    uint32_t prop = (cnf[1] & 0x07) + 1, ps1 = ((cnf[1] >> 3) & 0x07) + 1;
    uint32_t ps2 = (cnf[2] & 0x07) + 1;
    uint32_t tq = 1 + prop + ps1 + ps2;
    CHECK(cnf[1] & 0x80);  // PS2 set by CNF3
    CHECK(tq >= 8 && tq <= 25);
    CHECK(ps2 >= 2 && ps2 > sjw - 1 && ps2 >= sjw && prop + ps1 >= ps2);
    double rate = 16e6 / (2.0 * (brp + 1)) / tq;
    CHECK(rate == (double)bitRateOf(code));
    double sp = 100.0 * (1 + prop + ps1) / tq;
    CHECK(sp >= 75.0 && sp <= 87.5);
  }
  CHECK(!bitTiming(9, cnf));
}

static void testBusManners() {
  Rig r;
  CHECK(r.b.mode() == MODE_CLOSED);
  CHECK_EQ(r.cmd("t7DF80201000000000000"), std::string("\a"));  // closed: refused
  CHECK(r.hal.sent.empty());
  CHECK_EQ(r.cmd("S6"), std::string("\r"));
  CHECK_EQ(r.cmd("L"), std::string("\r"));
  CHECK(r.b.mode() == MODE_LISTEN && r.hal.openMode == MODE_LISTEN && r.hal.openRate == 6);
  CHECK_EQ(r.cmd("t7DF80201000000000000"), std::string("\a"));  // listen-only: refused
  CHECK(r.hal.sent.empty());
  CHECK_EQ(r.cmd("S5"), std::string("\a"));  // rate only while closed
  CHECK_EQ(r.cmd("O"), std::string("\a"));   // already open
  CHECK_EQ(r.cmd("#T"), std::string("\a"));  // self-test only while closed
  CHECK_EQ(r.cmd("C"), std::string("\r"));
  CHECK(r.b.mode() == MODE_CLOSED && r.hal.closes == 1);
  CHECK_EQ(r.cmd("O"), std::string("\r"));
  CHECK_EQ(r.cmd("t7DF80201000000000000"), std::string("z\r"));
  CHECK(r.hal.sent.size() == 1 && r.hal.sent[0].id == 0x7DF && r.hal.sent[0].data[1] == 0x01);
  CHECK_EQ(r.cmd("T18DB33F1802010C0000000000"), std::string("Z\r"));
  CHECK(r.hal.sent.size() == 2 && r.hal.sent[1].ext);
}

static void testNoChip() {
  Rig r;
  r.hal.chip = false;
  CHECK_EQ(r.cmd("O"), std::string("\a"));
  CHECK(r.b.mode() == MODE_CLOSED);
  FakeHal h;
  h.chip = false;
  Bridge<FakeHal> b(h);
  b.begin();
  CHECK_EQ(h.out, std::string("#R scana-bridge " SCANA_BRIDGE_VERSION " mcp2515=missing\r"));
}

static void testResetReport() {
  FakeHal h;
  Bridge<FakeHal> b(h);
  b.begin("reset=watchdog step=7 count=2");
  CHECK_EQ(h.out, std::string("#R scana-bridge " SCANA_BRIDGE_VERSION " mcp2515=ok reset=watchdog step=7 count=2\r"));
  // Breadcrumbs: a poll ends at the serial output step.
  b.poll();
  CHECK(h.lastStep == STEP_SERIAL_OUT);
}

static void testOpenFails() {
  Rig r;
  r.hal.openOk = false;
  CHECK_EQ(r.cmd("O"), std::string("\a"));
  CHECK(r.b.mode() == MODE_CLOSED);
}

static void testReceive() {
  Rig r;
  r.cmd("L");
  r.hal.rxq.push_back(frame(0x7E8, {0x04, 0x41, 0x05, 0x46, 0, 0, 0, 0}));
  CHECK_EQ(r.pollOut(), std::string("t7E880441054600000000\r"));
  CHECK(r.b.rxCount() == 1);
  r.cmd("C");
  r.hal.rxq.push_back(frame(0x7E8, {0x01}));
  CHECK_EQ(r.pollOut(), std::string(""));  // closed: not read
}

static void testRxQueueFull() {
  Rig r;
  r.cmd("L");
  r.hal.space = 0;  // serial port blocked
  for (int i = 0; i < 25; i++) r.hal.rxq.push_back(frame(0x100 + i, {1}));
  for (int i = 0; i < 5; i++) r.b.poll();
  CHECK(r.hal.rxq.size() == 5);  // 20 queued, 5 left in the "chip"
  r.hal.space = 255;
  r.b.poll();
  r.b.poll();
  CHECK(r.hal.rxq.empty());
  CHECK(r.b.rxCount() == 25);
  // Order kept: the first line is id 0x100.
  CHECK(r.hal.out.rfind("t1001", 0) == 0);
}

static void testTxQueue() {
  Rig r;
  r.cmd("O");
  r.hal.holdTx = true;
  for (int i = 0; i < 5; i++) CHECK_EQ(r.cmd("t1230"), std::string("z\r"));  // 1 in chip + 4 queued
  CHECK_EQ(r.cmd("t1230"), std::string("\a"));
  CHECK(r.hal.sent.size() == 1);
  r.hal.holdTx = false;
  for (int i = 0; i < 10; i++) r.b.poll();
  CHECK(r.hal.sent.size() == 5);
}

static void testEvents() {
  Rig r;
  CHECK_EQ(r.cmd("#E1"), std::string("\r"));
  r.cmd("O");
  r.hal.eflg = EFLG_TXEP;
  r.hal.ms += 20;
  CHECK_EQ(r.pollOut(), std::string("#e ERRPASSIVE\r"));
  r.hal.eflg = EFLG_TXBO;
  r.hal.ms += 20;
  CHECK_EQ(r.pollOut(), std::string("#e BUSOFF\r"));
  r.hal.eflg = EFLG_RX0OVR;
  r.hal.ms += 20;
  CHECK_EQ(r.pollOut(), std::string("#e RXOVR 1\r#e ACTIVE\r"));
  CHECK(r.hal.eflg == 0);
  r.hal.txResult = TX_ERROR;
  CHECK_EQ(r.cmd("t7DF0"), std::string("z\r#e TXERR\r"));
  r.hal.txResult = TX_ARBITRATION;
  CHECK_EQ(r.cmd("t7DF0"), std::string("z\r#e TXARB\r"));
  // Events off: nothing.
  r.cmd("#E0");
  r.hal.txResult = TX_ERROR;
  CHECK_EQ(r.cmd("t7DF0"), std::string("z\r"));
}

static void testHostWatchdog() {
  Rig r;
  r.cmd("#E1");
  CHECK_EQ(r.cmd("#W100"), std::string("\r"));
  r.cmd("L");
  r.hal.ms += 99;
  r.pollOut();
  CHECK(r.b.mode() == MODE_LISTEN);
  r.cmd("#P");  // keeps it alive
  r.hal.ms += 99;
  r.pollOut();
  CHECK(r.b.mode() == MODE_LISTEN);
  r.hal.ms += 2;
  CHECK_EQ(r.pollOut(), std::string("#e HOSTWD\r"));
  CHECK(r.b.mode() == MODE_CLOSED);
  CHECK_EQ(r.cmd("#W0"), std::string("\r"));
  r.cmd("L");
  r.hal.ms += 100000;
  r.pollOut();
  CHECK(r.b.mode() == MODE_LISTEN);  // watchdog off
  // Opening takes time on the real chip: the watchdog must not fire straight away.
  r.cmd("C");
  r.hal.openTakesMs = 6;
  r.cmd("#W2000");
  CHECK_EQ(r.cmd("#K"), std::string("\r"));
  CHECK(r.b.mode() == MODE_LOOPBACK);
  r.hal.openTakesMs = 0;
  r.cmd("C");
  CHECK_EQ(r.cmd("#W60001"), std::string("#e REFUSED #W60001\r\a"));
  CHECK_EQ(r.cmd("#W"), std::string("#e REFUSED #W\r\a"));
  CHECK_EQ(r.cmd("#W1x"), std::string("#e REFUSED #W1x\r\a"));
}

static void testCommands() {
  Rig r;
  CHECK_EQ(r.cmd("V"), std::string("V0100\r"));
  CHECK_EQ(r.cmd("N"), std::string("N5CA1\r"));
  CHECK_EQ(r.cmd("#I"), std::string("#I scana-bridge " SCANA_BRIDGE_VERSION " mcp2515 16MHz\r"));
  CHECK_EQ(r.cmd("#P"), std::string("#P\r"));
  CHECK_EQ(r.cmd("#T"), std::string("#T PASS\r"));
  r.hal.selfTestResult = 4;
  CHECK_EQ(r.cmd("#T"), std::string("#T FAIL 4 FF\r"));
  CHECK_EQ(r.cmd("Z1"), std::string("\r"));
  CHECK_EQ(r.cmd("#U1"), std::string("\r"));
  CHECK_EQ(r.cmd("Z2"), std::string("\a"));
  CHECK_EQ(r.cmd("S9"), std::string("\a"));
  CHECK_EQ(r.cmd("S"), std::string("\a"));
  CHECK_EQ(r.cmd("X"), std::string("\a"));
  CHECK_EQ(r.cmd("#"), std::string("\a"));
  CHECK_EQ(r.cmd("#Q"), std::string("\a"));
  CHECK_EQ(r.cmd("F"), std::string("F00\r"));
  CHECK_EQ(r.cmd(std::string(50, 'A')), std::string("\a"));  // too long
  // CR LF from a terminal still works.
  for (char c : std::string("V\r\n")) r.hal.in.push_back(c);
  r.b.poll();
  CHECK_EQ(r.hal.out, std::string("V0100\r"));
  r.hal.out.clear();
  // Noise bytes before a command are ignored.
  for (int c : {0x00, 0xFF, int('#'), int('P'), int('\r')}) r.hal.in.push_back(c);
  r.b.poll();
  CHECK_EQ(r.hal.out, std::string("#P\r"));
  r.hal.out.clear();
  // Empty lines get no reply.
  CHECK_EQ(r.cmd(""), std::string(""));
  std::string h = r.cmd("#H");
  CHECK(h.rfind("#H up=", 0) == 0 && h.find(" mode=C") != std::string::npos &&
        h.find(" bad=") != std::string::npos && h.back() == '\r');
}

static void testLoopbackAndFilter() {
  Rig r;
  CHECK_EQ(r.cmd("#M7F8,7E8"), std::string("\r"));
  CHECK_EQ(r.cmd("#K"), std::string("\r"));
  CHECK(r.b.mode() == MODE_LOOPBACK && r.hal.openMode == MODE_LOOPBACK);
  CHECK(r.hal.openFilter && r.hal.mask == 0x7F8 && r.hal.filt == 0x7E8);
  CHECK_EQ(r.cmd("t1230"), std::string("z\r"));  // loopback may transmit
  CHECK_EQ(r.cmd("#M"), std::string("\a"));      // filters only while closed
  r.cmd("C");
  CHECK_EQ(r.cmd("#M"), std::string("\r"));
  CHECK_EQ(r.cmd("#M800,000"), std::string("\a"));
  CHECK_EQ(r.cmd("#M7F8-7E8"), std::string("\a"));
  r.cmd("L");
  CHECK(!r.hal.openFilter);
}

static void testSmallSerialBuffer() {
  Rig r;
  r.hal.space = 7;  // replies longer than the buffer go out in pieces
  std::string h = r.cmd("#H");
  CHECK(h.rfind("#H up=", 0) == 0 && h.back() == '\r' && h.size() > 60);
  CHECK_EQ(r.cmd("#I"), std::string("#I scana-bridge " SCANA_BRIDGE_VERSION " mcp2515 16MHz\r"));
}

// A line with its ";XX" check, as Scana sends it in checked mode.
static std::string withCheck(const std::string& body) {
  uint8_t c = slcan::crc8(body.c_str(), (uint8_t)body.size());
  return body + ";" + slcan::hexDigit(c >> 4) + slcan::hexDigit(c);
}

static void testCrc() {
  CHECK_EQ((int)slcan::crc8("123456789", 9), 0xF4);  // CRC-8 (0x07) check value
  std::string good = withCheck("t7E0802010C0000000000");
  uint8_t n = (uint8_t)good.size();
  CHECK(slcan::checkLine(good.c_str(), n));
  CHECK(n == 21);
  std::string bad = good;
  bad[6] = '1';  // one digit changed on the way
  n = (uint8_t)bad.size();
  CHECK(!slcan::checkLine(bad.c_str(), n));
  n = 21;
  CHECK(!slcan::checkLine("t7E0802010C0000000000", n));  // no check at all
}

// With events on, every refusal says why first, so the host's log shows what arrived.
static void testRefusalReasons() {
  Rig r;
  r.cmd("#E1");
  CHECK_EQ(r.cmd("t7E0802010C0000000000"), std::string("#e NOTOPEN\r\a"));
  r.cmd("O");
  CHECK_EQ(r.cmd("t7E0802010C00000000"), std::string("#e BADCMD t7E0802010C00000000\r\a"));  // 2 digits lost
  CHECK_EQ(r.cmd("X1"), std::string("#e BADCMD X1\r\a"));
  CHECK_EQ(r.cmd("O"), std::string("#e REFUSED O\r\a"));  // already open
  CHECK_EQ(r.cmd(std::string(45, 'A')), std::string("#e TOOLONG AAAAAAAAAAAAAAAAAAAAAAAA\r\a"));
  r.hal.holdTx = true;
  for (int i = 0; i < 5; i++) r.cmd("t1230");
  CHECK_EQ(r.cmd("t1230"), std::string("#e TXFULL\r\a"));
  r.hal.holdTx = false;
  // Events off: only the BEL, as before.
  r.cmd("#E0");
  CHECK_EQ(r.cmd("X1"), std::string("\a"));
}

// Checked mode: frame commands need a good ";XX", and received frames get one.
static void testCheckedMode() {
  Rig r;
  r.cmd("#E1");
  CHECK_EQ(r.cmd("#C1"), std::string("\r"));
  r.cmd("O");
  CHECK_EQ(r.cmd(withCheck("t7E0802010C0000000000")), std::string("z\r"));
  CHECK(r.hal.sent.size() == 1 && r.hal.sent[0].id == 0x7E0 && r.hal.sent[0].data[2] == 0x0C);
  CHECK_EQ(r.cmd("t7E0802010C0000000000"), std::string("#e BADCHECK t7E0802010C0000000000\r\a"));
  std::string bad = withCheck("t7E0802010C0000000000");
  bad[6] = '1';  // 02 01 0C would have become 12 01 0C: refused, never sent
  CHECK(r.cmd(bad).find("#e BADCHECK") == 0);
  CHECK(r.hal.sent.size() == 1);
  r.hal.rxq.push_back(frame(0x7E8, {0x04, 0x41, 0x0C, 0x12, 0x34}));
  CHECK_EQ(r.pollOut(), withCheck("t7E8504410C1234") + "\r");
  r.cmd("#C0");
  CHECK_EQ(r.cmd("t7E0802010C0000000000"), std::string("z\r"));
}

int main() {
  testSmallSerialBuffer();
  testHexAndParse();
  testFormat();
  testBitTiming();
  testBusManners();
  testNoChip();
  testResetReport();
  testOpenFails();
  testReceive();
  testRxQueueFull();
  testTxQueue();
  testEvents();
  testHostWatchdog();
  testCommands();
  testLoopbackAndFilter();
  testCrc();
  testRefusalReasons();
  testCheckedMode();
  std::printf("%d checks, %d failures\n", checks, failures);
  return failures ? 1 : 0;
}
