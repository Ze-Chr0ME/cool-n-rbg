//! Device drivers, one folder per manufacturer and one module per protocol inside it. Each
//! manufacturer is a Cargo feature, so a build only carries the drivers it needs.
pub mod corsair;
pub mod evision;
#[cfg(feature = "msi")]
pub mod msi;
#[cfg(feature = "smbus")]
pub mod transport;
