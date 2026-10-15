//! Scana's knowledge, as plain text files in `data/` built into the program: fault code
//! descriptions and advice, what each live value means, the glossary, guided jobs, page help,
//! a short lesson, vehicle profiles, and where modules usually answer.
//!
//! The files are parsed the first time they are used. A file that fails to parse gives an
//! empty list rather than a crash; the tests make sure every file parses.

pub mod dtc;
pub mod learn;
pub mod modules;
pub mod profile;
