//! Per-console support for the Nintendo hardware families this crate
//! converts: [`nx`] (Switch), [`ctr`] (3DS), [`ntr`] (DS), [`rvl`] (Wii),
//! [`wup`] (Wii U), and [`dol`] (GameCube). [`rvl`] and [`dol`] both sit on
//! the shared [`disc::rvz`]/[`disc::wbfs`] disc pipeline, which handles the
//! common GameCube and Wii disc container formats.
//!
//! The cartridge-era systems read headers only: [`hvc`] (Famicom/NES),
//! [`fds`] (Famicom Disk System), [`shvc`] (Super Famicom/SNES), [`nus`]
//! (Nintendo 64), [`dmg`] (Game Boy and Color), [`agb`] (Game Boy
//! Advance), and [`vue`] (Virtual Boy).

pub mod agb;
pub mod ctr;
pub mod disc;
pub mod dmg;
pub mod dol;
pub mod fds;
pub mod hvc;
pub mod ntr;
pub mod nus;
pub mod nx;
pub mod rvl;
pub mod shvc;
pub mod vue;
pub mod wup;
