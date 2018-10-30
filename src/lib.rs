#![feature(plugin)]
#![feature(type_ascription)]
#![plugin(rocket_codegen)]
#![feature(proc_macro_hygiene, decl_macro)]

extern crate rand;

#[macro_use]
extern crate quick_error;
extern crate brotli;
extern crate clap;
extern crate flate2;
#[macro_use]
extern crate lazy_static;
extern crate hyper;
extern crate libc;
extern crate lz4;
extern crate rocket;
extern crate rocket_slog;
extern crate slog;
extern crate sloggers;
extern crate snap;
extern crate walkdir;
extern crate zstd;

#[macro_use]
pub mod compression;
pub mod config;
pub mod disk;
pub mod diskgc;
pub mod router;
pub mod util;