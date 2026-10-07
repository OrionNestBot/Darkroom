//! Develop engine.
pub mod aistore;
pub mod color;
pub mod dcp;
pub mod embedded;
pub mod filters;
pub mod geometry;
pub mod image;
pub mod lensmodel;
pub mod lensfun;
pub mod mask;
#[allow(dead_code)]
pub mod measured_curves;
pub mod pipeline;
pub mod settings;

#[cfg(test)]
mod direction_tests;

#[cfg(test)]
mod audit_tests;

// Calibration tools (only for building profiles and tone curves from reference renders; not part of the normal test run): cargo test --features calib
#[cfg(all(test, feature = "calib"))]
mod calib;

#[cfg(all(test, feature = "calib"))]
mod lrcompare;

#[cfg(all(test, feature = "calib"))]
mod profile_fit;
