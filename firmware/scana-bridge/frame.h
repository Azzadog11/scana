// A CAN frame as the bridge handles it. Plain C++ so the host tests can use it.
#pragma once
#include <stdint.h>

struct CanFrame {
  uint32_t id;       // 11-bit or 29-bit identifier
  uint32_t us;       // receive time, micros() on the bridge
  uint8_t len;       // 0-8 data bytes
  bool ext;          // 29-bit identifier
  bool rtr;          // remote request (no data)
  uint8_t data[8];
};
