//! Per-console support for the five Nintendo hardware families this crate
//! converts: [`nx`] (Switch), [`ctr`] (3DS), [`rvl`] (Wii), [`wup`] (Wii U),
//! and [`dol`] (GameCube). [`rvl`] and [`dol`] both sit on the shared
//! [`rvz`]/[`wbfs`] disc pipeline, which handles the common GameCube and Wii
//! disc container formats.

pub mod ctr;
pub mod disc_input;
pub mod dol;
pub mod gcz;
pub mod legacy_input;
pub mod nkit;
pub mod nx;
pub mod rvl;
pub mod rvz;
pub mod wbfs;
pub mod wia;
pub mod wup;
