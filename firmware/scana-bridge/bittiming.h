// MCP2515 bit timing for a 16 MHz crystal. Plain C++ so the host tests can check it.
//
// One bit = sync (1 TQ) + propagation + phase 1 + phase 2. TQ = 2 x (BRP + 1) / 16 MHz.
// Most rates use 16 TQ with the sample point at 13/16 = 81.25%, in the range
// SAE J2284 asks for at 500 kbit/s. 800k and 1M can't fit 16 TQ, so they use 10 and 8.
#pragma once
#include <stdint.h>

// SLCAN "Sn" codes: 0=10k 1=20k 2=50k 3=100k 4=125k 5=250k 6=500k 7=800k 8=1M.
// Fills cnf[0]=CNF1, cnf[1]=CNF2, cnf[2]=CNF3. Returns false for an unknown code.
inline bool bitTiming(uint8_t code, uint8_t cnf[3]) {
  // CNF2 0xB4: BTLMODE=1, SAM=0, PHSEG1=6 (7 TQ), PRSEG=4 (5 TQ). CNF3 0x02: PHSEG2=2 (3 TQ).
  static const uint8_t brp16[7] = {49, 24, 9, 4, 3, 1, 0};  // codes 0-6 at 16 TQ per bit
  if (code <= 6) {
    cnf[0] = brp16[code];  // SJW = 1 TQ
    cnf[1] = 0xB4;
    cnf[2] = 0x02;
    return true;
  }
  if (code == 7) {  // 800k: 10 TQ = 1 + 3 + 4 + 2, sample point 80%
    cnf[0] = 0x00;
    cnf[1] = 0x80 | (3 << 3) | 2;
    cnf[2] = 0x01;
    return true;
  }
  if (code == 8) {  // 1M: 8 TQ = 1 + 2 + 3 + 2, sample point 75%
    cnf[0] = 0x00;
    cnf[1] = 0x80 | (2 << 3) | 1;
    cnf[2] = 0x01;
    return true;
  }
  return false;
}

// Bits per second for each code, for messages and tests.
inline uint32_t bitRateOf(uint8_t code) {
  static const uint32_t rates[9] = {10000, 20000, 50000, 100000, 125000,
                                    250000, 500000, 800000, 1000000};
  return code <= 8 ? rates[code] : 0;
}
