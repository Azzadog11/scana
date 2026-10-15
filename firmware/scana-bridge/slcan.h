// SLCAN (Lawicel) text helpers: parse frame commands, format received frames.
// Plain C++ so the host tests can use them.
#pragma once
#include <stdint.h>
#include "frame.h"

namespace slcan {

inline int8_t hexVal(char c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'A' && c <= 'F') return c - 'A' + 10;
  if (c >= 'a' && c <= 'f') return c - 'a' + 10;
  return -1;
}

// Reads n hex digits into out. False if any character is not hex.
inline bool parseHex(const char* s, uint8_t n, uint32_t& out) {
  out = 0;
  for (uint8_t i = 0; i < n; i++) {
    int8_t v = hexVal(s[i]);
    if (v < 0) return false;
    out = (out << 4) | (uint8_t)v;
  }
  return true;
}

// Parses "tIIILDD..", "TIIIIIIIILDD..", "rIIIL" or "RIIIIIIIIL". len excludes the CR.
// The length must be exact: a short or long line is rejected.
inline bool parseFrameCmd(const char* line, uint8_t len, CanFrame& f) {
  if (len < 1) return false;
  char kind = line[0];
  bool ext = (kind == 'T' || kind == 'R');
  bool rtr = (kind == 'r' || kind == 'R');
  if (!(kind == 't' || kind == 'T' || kind == 'r' || kind == 'R')) return false;
  uint8_t idDigits = ext ? 8 : 3;
  if (len < 1 + idDigits + 1) return false;
  uint32_t id, dlc;
  if (!parseHex(line + 1, idDigits, id)) return false;
  if (ext ? id > 0x1FFFFFFFUL : id > 0x7FF) return false;
  if (!parseHex(line + 1 + idDigits, 1, dlc) || dlc > 8) return false;
  uint8_t want = 1 + idDigits + 1 + (rtr ? 0 : (uint8_t)(2 * dlc));
  if (len != want) return false;
  f.id = id;
  f.ext = ext;
  f.rtr = rtr;
  f.len = (uint8_t)dlc;
  f.us = 0;
  for (uint8_t i = 0; i < 8; i++) f.data[i] = 0;
  if (!rtr) {
    const char* d = line + 2 + idDigits;
    for (uint8_t i = 0; i < dlc; i++) {
      uint32_t b;
      if (!parseHex(d + 2 * i, 2, b)) return false;
      f.data[i] = (uint8_t)b;
    }
  }
  return true;
}

inline char hexDigit(uint8_t v) { return "0123456789ABCDEF"[v & 0x0F]; }

inline uint8_t putHex(char* out, uint32_t v, uint8_t digits) {
  for (uint8_t i = 0; i < digits; i++) out[i] = hexDigit((uint8_t)(v >> (4 * (digits - 1 - i))));
  return digits;
}

enum TimeMode : uint8_t { TIME_NONE = 0, TIME_MS = 1, TIME_US = 2 };

// Longest line: 'T' + 8 + 1 + 16 + ':' + 8 + CR = 35.
const uint8_t MAX_FRAME_LINE = 36;

// Formats a received frame with its CR. Returns the length.
// TIME_MS appends 4 hex digits of milliseconds 0-59999 (Lawicel).
// TIME_US appends ':' and 8 hex digits of microseconds (Scana's #U1).
inline uint8_t formatFrame(const CanFrame& f, char* out, TimeMode tm) {
  uint8_t n = 0;
  out[n++] = f.ext ? (f.rtr ? 'R' : 'T') : (f.rtr ? 'r' : 't');
  n += putHex(out + n, f.id, f.ext ? 8 : 3);
  out[n++] = hexDigit(f.len);
  if (!f.rtr)
    for (uint8_t i = 0; i < f.len && i < 8; i++) n += putHex(out + n, f.data[i], 2);
  if (tm == TIME_MS) {
    n += putHex(out + n, (f.us / 1000UL) % 60000UL, 4);
  } else if (tm == TIME_US) {
    out[n++] = ':';
    n += putHex(out + n, f.us, 8);
  }
  out[n++] = '\r';
  return n;
}

}  // namespace slcan
