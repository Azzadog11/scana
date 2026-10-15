// A small MCP2515 driver for the SparkFun CAN-BUS Shield (CS on D10, INT on D2, 16 MHz).
// Own code rather than a library, for one-shot transmit, a single transmit buffer
// (keeps frames in order) and the self-test. Register names follow Microchip's
// MCP2515 data sheet (DS20001801).
#pragma once
#include <stdint.h>
#include "frame.h"

class Mcp2515 {
 public:
  // Sets up SPI and resets the chip. False if the chip doesn't answer.
  bool begin(uint8_t csPin, uint8_t intPin);
  bool present() const { return present_; }

  // mode: 1 normal, 2 listen-only, 3 loopback (bridge.h BusMode). cnf from bittiming.h.
  bool open(uint8_t mode, const uint8_t cnf[3], bool filterOn, uint16_t mask, uint16_t filt);
  void close();

  bool read(CanFrame& f);       // false if nothing waiting
  bool txIdle();
  void write(const CanFrame& f);
  uint8_t txDone();             // TxResult from bridge.h
  void status(uint8_t& tec, uint8_t& rec, uint8_t& eflg);
  void clearOverflow();
  // 0 pass, else the failed step. detail: the value read at that step (FF = no chip answering).
  // Leaves the chip closed, and updates present().
  uint8_t selfTest(uint8_t& detail);

 private:
  uint8_t cs_ = 10, int_ = 2;
  bool present_ = false;
  bool txPending_ = false;
  bool rx1Next_ = false;  // RXB1 holds an older frame than RXB0: read it first

  void reset();
  uint8_t readReg(uint8_t addr);
  void readRegs(uint8_t addr, uint8_t* out, uint8_t n);
  void writeReg(uint8_t addr, uint8_t v);
  void writeRegs(uint8_t addr, const uint8_t* v, uint8_t n);
  void modify(uint8_t addr, uint8_t mask, uint8_t v);
  uint8_t readStatus();
  bool setMode(uint8_t reqop);
  void select();
  void deselect();
};
