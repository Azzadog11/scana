#include "mcp2515.h"

#include <Arduino.h>
#include <SPI.h>

#include "bittiming.h"
#include "bridge.h"

namespace {
// SPI instructions
const uint8_t I_RESET = 0xC0, I_READ = 0x03, I_WRITE = 0x02, I_BITMOD = 0x05,
              I_READ_STATUS = 0xA0, I_RTS_TX0 = 0x81, I_LOAD_TX0 = 0x40,
              I_READ_RX0 = 0x90, I_READ_RX1 = 0x94;
// Registers
const uint8_t R_RXF0 = 0x00, R_RXF1 = 0x04, R_RXF2 = 0x08, R_RXF3 = 0x10, R_RXF4 = 0x14,
              R_RXF5 = 0x18, R_RXM0 = 0x20, R_RXM1 = 0x24, R_CANSTAT = 0x0E, R_CANCTRL = 0x0F,
              R_TEC = 0x1C, R_CNF3 = 0x28, R_CANINTE = 0x2B, R_CANINTF = 0x2C, R_EFLG = 0x2D,
              R_TXB0CTRL = 0x30, R_RXB0CTRL = 0x60, R_RXB1CTRL = 0x70, R_CNF1 = 0x2A;
// REQOP values (CANCTRL bits 7-5)
const uint8_t OP_NORMAL = 0x00, OP_LOOPBACK = 0x40, OP_LISTEN = 0x60, OP_CONFIG = 0x80;
const uint8_t CANCTRL_ABAT = 0x10, CANCTRL_OSM = 0x08;
const uint8_t TXB_ABTF = 0x40, TXB_MLOA = 0x20, TXB_TXERR = 0x10, TXB_TXREQ = 0x08;
const uint8_t INT_RX0 = 0x01, INT_RX1 = 0x02, INT_TX0 = 0x04;

// The Uno's SPI runs at up to 8 MHz; the MCP2515 takes up to 10 MHz.
const SPISettings kSpi(8000000, MSBFIRST, SPI_MODE0);

// SIDH, SIDL, EID8, EID0 for a standard filter or mask value.
void stdIdRegs(uint16_t id, uint8_t out[4]) {
  out[0] = (uint8_t)(id >> 3);
  out[1] = (uint8_t)((id & 0x07) << 5);
  out[2] = 0;
  out[3] = 0;
}
}  // namespace

void Mcp2515::select() {
  SPI.beginTransaction(kSpi);
  digitalWrite(cs_, LOW);
}

void Mcp2515::deselect() {
  digitalWrite(cs_, HIGH);
  SPI.endTransaction();
}

void Mcp2515::reset() {
  select();
  SPI.transfer(I_RESET);
  deselect();
  delay(5);  // oscillator start-up after reset
  txPending_ = false;
  rx1Next_ = false;
}

uint8_t Mcp2515::readReg(uint8_t addr) {
  select();
  SPI.transfer(I_READ);
  SPI.transfer(addr);
  uint8_t v = SPI.transfer(0);
  deselect();
  return v;
}

void Mcp2515::readRegs(uint8_t addr, uint8_t* out, uint8_t n) {
  select();
  SPI.transfer(I_READ);
  SPI.transfer(addr);
  for (uint8_t i = 0; i < n; i++) out[i] = SPI.transfer(0);
  deselect();
}

void Mcp2515::writeReg(uint8_t addr, uint8_t v) {
  select();
  SPI.transfer(I_WRITE);
  SPI.transfer(addr);
  SPI.transfer(v);
  deselect();
}

void Mcp2515::writeRegs(uint8_t addr, const uint8_t* v, uint8_t n) {
  select();
  SPI.transfer(I_WRITE);
  SPI.transfer(addr);
  for (uint8_t i = 0; i < n; i++) SPI.transfer(v[i]);
  deselect();
}

void Mcp2515::modify(uint8_t addr, uint8_t mask, uint8_t v) {
  select();
  SPI.transfer(I_BITMOD);
  SPI.transfer(addr);
  SPI.transfer(mask);
  SPI.transfer(v);
  deselect();
}

uint8_t Mcp2515::readStatus() {
  select();
  SPI.transfer(I_READ_STATUS);
  uint8_t v = SPI.transfer(0);
  deselect();
  return v;
}

bool Mcp2515::setMode(uint8_t reqop) {
  modify(R_CANCTRL, 0xE0, reqop);
  unsigned long start = millis();
  while (millis() - start < 10) {
    if ((readReg(R_CANSTAT) & 0xE0) == reqop) return true;
  }
  return false;
}

bool Mcp2515::begin(uint8_t csPin, uint8_t intPin) {
  cs_ = csPin;
  int_ = intPin;
  pinMode(cs_, OUTPUT);
  digitalWrite(cs_, HIGH);
  pinMode(int_, INPUT);  // the MCP2515 drives INT itself
  SPI.begin();
  reset();
  // After reset the chip is in configuration mode, and CNF1 can be written and read back.
  present_ = (readReg(R_CANSTAT) & 0xE0) == OP_CONFIG;
  if (present_) {
    writeReg(R_CNF1, 0x5A);
    present_ = readReg(R_CNF1) == 0x5A;
    writeReg(R_CNF1, 0x00);
  }
  return present_;
}

bool Mcp2515::open(uint8_t mode, const uint8_t cnf[3], bool filterOn, uint16_t mask,
                   uint16_t filt) {
  if (!present_) return false;
  reset();
  if (!setMode(OP_CONFIG)) return false;
  const uint8_t timing[3] = {cnf[2], cnf[1], cnf[0]};  // CNF3, CNF2, CNF1 are consecutive
  writeRegs(R_CNF3, timing, 3);
  writeReg(R_CANINTE, INT_RX0 | INT_RX1);  // INT pin goes low when a frame arrives
  writeReg(R_CANINTF, 0);
  writeReg(R_EFLG, 0);
  writeReg(R_TXB0CTRL, 0);
  if (filterOn) {
    uint8_t m[4], f[4];
    stdIdRegs(mask, m);
    stdIdRegs(filt, f);
    writeRegs(R_RXM0, m, 4);
    writeRegs(R_RXM1, m, 4);
    const uint8_t filters[6] = {R_RXF0, R_RXF1, R_RXF2, R_RXF3, R_RXF4, R_RXF5};
    for (uint8_t i = 0; i < 6; i++) writeRegs(filters[i], f, 4);
    writeReg(R_RXB0CTRL, 0x04);  // filters on, roll over into RXB1
    writeReg(R_RXB1CTRL, 0x00);
  } else {
    writeReg(R_RXB0CTRL, 0x64);  // RXM=11: take every frame; roll over into RXB1
    writeReg(R_RXB1CTRL, 0x60);
  }
  uint8_t op = mode == MODE_NORMAL ? OP_NORMAL : mode == MODE_LISTEN ? OP_LISTEN : OP_LOOPBACK;
  // One-shot: a frame nobody acknowledges is not repeated forever.
  writeReg(R_CANCTRL, OP_CONFIG | CANCTRL_OSM);
  return setMode(op);
}

void Mcp2515::close() {
  if (!present_) return;
  modify(R_CANCTRL, CANCTRL_ABAT, CANCTRL_ABAT);  // abort anything waiting to send
  setMode(OP_CONFIG);
  modify(R_CANCTRL, CANCTRL_ABAT, 0);
  txPending_ = false;
}

bool Mcp2515::read(CanFrame& f) {
  if (digitalRead(int_) == HIGH) return false;  // nothing waiting: skip the SPI traffic
  uint8_t st = readStatus();
  uint8_t instr;
  // With roll-over, RXB1 only fills while RXB0 is full, so a frame in RXB1 is newer than
  // the one in RXB0 at that moment. Once RXB0 is read, a new frame can land in RXB0 before
  // RXB1 is read: remember to take RXB1 next, so frames stay in order.
  if (rx1Next_ && (st & INT_RX1)) {
    instr = I_READ_RX1;
    rx1Next_ = false;
  } else if (st & INT_RX0) {
    instr = I_READ_RX0;
    rx1Next_ = (st & INT_RX1) != 0;
  } else if (st & INT_RX1) {
    instr = I_READ_RX1;
    rx1Next_ = false;
  } else {
    return false;
  }
  uint8_t h[5];
  select();
  SPI.transfer(instr);
  for (uint8_t i = 0; i < 5; i++) h[i] = SPI.transfer(0);
  uint8_t len = h[4] & 0x0F;
  if (len > 8) len = 8;
  for (uint8_t i = 0; i < 8; i++) f.data[i] = i < len ? SPI.transfer(0) : 0;
  deselect();  // raising CS after READ RX BUFFER clears the RXnIF flag
  f.us = micros();
  f.len = len;
  f.ext = h[1] & 0x08;
  if (f.ext) {
    f.id = ((uint32_t)h[0] << 21) | ((uint32_t)(h[1] & 0xE0) << 13) |
           ((uint32_t)(h[1] & 0x03) << 16) | ((uint32_t)h[2] << 8) | h[3];
    f.rtr = h[4] & 0x40;
  } else {
    f.id = ((uint32_t)h[0] << 3) | (h[1] >> 5);
    f.rtr = h[1] & 0x10;
  }
  return true;
}

bool Mcp2515::txIdle() { return !txPending_ && !(readReg(R_TXB0CTRL) & TXB_TXREQ); }

void Mcp2515::write(const CanFrame& f) {
  uint8_t b[13];
  if (f.ext) {
    b[0] = (uint8_t)(f.id >> 21);
    b[1] = (uint8_t)(((f.id >> 13) & 0xE0) | 0x08 | ((f.id >> 16) & 0x03));
    b[2] = (uint8_t)(f.id >> 8);
    b[3] = (uint8_t)f.id;
  } else {
    b[0] = (uint8_t)(f.id >> 3);
    b[1] = (uint8_t)((f.id & 0x07) << 5);
    b[2] = 0;
    b[3] = 0;
  }
  uint8_t len = f.len > 8 ? 8 : f.len;
  b[4] = len | (f.rtr ? 0x40 : 0);
  for (uint8_t i = 0; i < len; i++) b[5 + i] = f.data[i];
  modify(R_CANINTF, INT_TX0, 0);
  select();
  SPI.transfer(I_LOAD_TX0);
  for (uint8_t i = 0; i < 5 + (f.rtr ? 0 : len); i++) SPI.transfer(b[i]);
  deselect();
  select();
  SPI.transfer(I_RTS_TX0);
  deselect();
  txPending_ = true;
}

uint8_t Mcp2515::txDone() {
  if (!txPending_) return TX_NONE;
  uint8_t c = readReg(R_TXB0CTRL);
  if (c & TXB_TXREQ) return TX_NONE;
  txPending_ = false;
  if (c & TXB_MLOA) return TX_ARBITRATION;
  if (c & (TXB_TXERR | TXB_ABTF)) return TX_ERROR;
  return TX_OK;
}

void Mcp2515::status(uint8_t& tec, uint8_t& rec, uint8_t& eflg) {
  uint8_t r[2];
  readRegs(R_TEC, r, 2);  // TEC then REC
  tec = r[0];
  rec = r[1];
  eflg = readReg(R_EFLG);
}

void Mcp2515::clearOverflow() { modify(R_EFLG, EFLG_RX0OVR | EFLG_RX1OVR, 0); }

uint8_t Mcp2515::selfTest(uint8_t& detail) {
  // 1: the chip answers and is in configuration mode after a reset.
  reset();
  detail = readReg(R_CANSTAT);
  present_ = (detail & 0xE0) == OP_CONFIG;
  if (!present_) return 1;
  // 2: registers write and read back.
  writeReg(R_CNF1, 0x55);
  detail = readReg(R_CNF1);
  if (detail != 0x55) return 2;
  writeReg(R_CNF1, 0xAA);
  detail = readReg(R_CNF1);
  if (detail != 0xAA) return 2;
  detail = 0;
  // 3-5: a frame through the chip's internal loopback (nothing reaches the bus).
  uint8_t cnf[3];
  bitTiming(6, cnf);
  if (!open(MODE_LOOPBACK, cnf, false, 0, 0)) {
    detail = readReg(R_CANSTAT);
    close();
    return 3;
  }
  CanFrame out = {};
  out.id = 0x5CA;
  out.len = 4;
  out.data[0] = 0xDE;
  out.data[1] = 0xAD;
  out.data[2] = 0xBE;
  out.data[3] = 0xEF;
  write(out);
  CanFrame in = {};
  bool sent = false, got = false;
  unsigned long start = millis();
  while (millis() - start < 20 && !(sent && got)) {
    if (!sent) {
      uint8_t r = txDone();
      if (r == TX_OK) sent = true;
      else if (r != TX_NONE) break;
    }
    if (!got && read(in)) got = true;
  }
  detail = readReg(R_TXB0CTRL);
  close();
  if (!sent) return 3;
  if (!got) return 4;
  detail = (uint8_t)in.id;
  if (in.id != out.id || in.ext || in.len != 4 || in.data[0] != 0xDE || in.data[3] != 0xEF)
    return 5;
  return 0;
}
